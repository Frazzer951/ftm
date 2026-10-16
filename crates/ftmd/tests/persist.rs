//! Panes live in their holders, so they outlive ftmd restarts, and `ftm attach` drives one from a terminal.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, alive, ftm_bin, start_daemon, start_daemon_logged, temp_root, wait_for, wait_for_output};
use ftm_core::model::{Kind, PaneId, SplitDir, Status, Workspace, WorkspaceId};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn spawn_terminal(conn: &mut Conn, cwd: &Path, name: &str) -> Workspace {
    let Response::Spawned { workspace } = conn.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: Some(name.into()),
        parent: None,
        cwd: cwd.to_path_buf(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: None,
    })) else {
        panic!("expected spawned")
    };
    workspace
}

fn capture(conn: &mut Conn, pane: PaneId) -> String {
    match conn.send(&Request::Capture { pane, raw: false }) {
        Response::Output { data } => String::from_utf8(B64.decode(data).unwrap()).unwrap(),
        r => panic!("{r:?}"),
    }
}

/// Runs `echo` in the pane and returns the numbers it printed: the shell's pid and its parent (the holder). The
/// marker is spelled with arithmetic so the echoed command line never matches it. It is looked for anywhere in a
/// line: typed before a shell without line editing (dash) prints its prompt, the output follows that prompt.
fn pids(conn: &mut Conn, pane: PaneId, tag: &str) -> (i32, i32) {
    conn.type_in(pane, &format!("echo X$((1)){tag}=$$:$PPID.\n"));
    let marker = format!("X1{tag}=");
    let mut found = None;
    wait_for(&format!("{tag} pids"), Duration::from_secs(10), || {
        let text = capture(conn, pane);
        found = text.lines().find_map(|l| {
            let (a, b) = l.split_once(marker.as_str())?.1.strip_suffix('.')?.split_once(':')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        });
        found.is_some()
    });
    found.unwrap()
}

#[test]
fn ftmd_restart_keeps_sessions() {
    let dir = temp_root("restart");
    let mut daemon = start_daemon("restart", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let ws = spawn_terminal(&mut req, &dir, "kept");
    let pane = ws.tabs[0].layout.panes()[0];
    let Response::Pane { pane: right } = req.send(&Request::Split {
        pane,
        dir: SplitDir::Row,
    }) else {
        panic!()
    };
    req.send(&Request::SetStatus {
        id: ws.id,
        status: Status::Working,
        background_agents: 0,
    });
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    let (shell, holder) = pids(&mut req, pane, "before");
    req.type_in(pane, "echo; printf 'line%s\\n' $(seq 1 40) ; echo marker-$((6*7))\n");
    wait_for_output(&mut events, pane, "marker-42");
    let tree = serde_json::to_value(req.list()).unwrap();
    drop((req, events));

    // Pane sockets and the saved tree are owner-only; the shared dir the test socket lives in is left alone.
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&daemon.holders()), 0o700);
    assert_eq!(mode(&daemon.socket), 0o600);
    assert_eq!(mode(&daemon.state_home().join("ftm")), 0o700);
    assert_eq!(mode(Path::new("/tmp")) & 0o777, 0o777);

    daemon.restart();
    assert!(alive(shell) && alive(holder), "pane processes died with ftmd");

    let mut req = Conn::open(&daemon.socket);
    assert_eq!(
        serde_json::to_value(req.list()).unwrap(),
        tree,
        "tree changed across the restart"
    );
    // The new ftmd rebuilt the screen and scrollback from the holder's buffer.
    let text = capture(&mut req, pane);
    assert!(text.lines().any(|l| l == "line1") && text.contains("marker-42"), "{text}");

    // A subscriber gets the replay, then live output; input still reaches the same shell.
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    wait_for_output(&mut events, pane, "marker-42");
    assert_eq!(pids(&mut req, pane, "after"), (shell, holder));
    req.type_in(right, "echo right-$((1+1))\n");
    wait_for_output(&mut events, right, "right-2");
}

/// A terminal's command (an update's `makepkg -si`) runs once: a restarted ftmd reattaches to the pane's shell rather
/// than starting the pane again.
#[test]
fn ftmd_restart_does_not_rerun_a_terminal_command() {
    let dir = temp_root("rerun");
    let mut daemon = start_daemon("rerun", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    let runs = dir.join("runs");
    let Response::Spawned { workspace } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: Some("once".into()),
        parent: None,
        cwd: dir.clone(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: Some(format!("echo run >> '{}'; echo ran-$((2+2))", runs.display())),
    })) else {
        panic!("expected spawned")
    };
    let pane = workspace.tabs[0].layout.panes()[0];
    wait_for_output(&mut events, pane, "ran-4");
    let before = pids(&mut req, pane, "before");
    drop((req, events));

    daemon.restart();

    let mut req = Conn::open(&daemon.socket);
    assert_eq!(pids(&mut req, pane, "after"), before);
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "run\n");
}

#[test]
fn panes_whose_holder_died_are_dropped() {
    let dir = temp_root("dropped");
    let mut daemon = start_daemon("dropped", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let both = spawn_terminal(&mut req, &dir, "both");
    let gone = spawn_terminal(&mut req, &dir, "gone");
    let survivor = both.tabs[0].layout.panes()[0];
    let Response::Pane { pane: victim } = req.send(&Request::Split {
        pane: survivor,
        dir: SplitDir::Column,
    }) else {
        panic!()
    };
    let lone = gone.tabs[0].layout.panes()[0];
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    let doomed = [pids(&mut req, victim, "v"), pids(&mut req, lone, "l")];
    drop((req, events));

    // As after a reboot: ftmd is down and the holders are gone.
    daemon.stop();
    for (shell, holder) in doomed {
        unsafe {
            libc::kill(holder, libc::SIGKILL);
            libc::kill(shell, libc::SIGKILL);
        }
    }
    wait_for("holders to die", Duration::from_secs(5), || {
        doomed.iter().all(|&(_, h)| !alive(h))
    });
    daemon.restart();

    let mut req = Conn::open(&daemon.socket);
    let list = req.list();
    let find = |id| list.iter().find(|w: &&Workspace| w.id == id).unwrap();
    assert_eq!(find(both.id).tabs[0].layout.panes(), vec![survivor]);
    assert!(find(gone.id).tabs.is_empty(), "workspace stays, without tabs");
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    req.type_in(survivor, "echo still-$((2+2))\n");
    wait_for_output(&mut events, survivor, "still-4");
}

#[test]
fn attach_streams_a_pane_and_detaches() {
    let dir = temp_root("attach");
    let daemon = start_daemon("attach", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let ws = spawn_terminal(&mut req, &dir, "attached");
    let pane = ws.tabs[0].layout.panes()[0];
    req.type_in(pane, "echo earlier-$((3+3))\n");
    wait_for("output before attaching", Duration::from_secs(10), || {
        capture(&mut req, pane).contains("earlier-6")
    });

    // The protocol: Ok, a redraw of the current screen, then live output.
    let mut stream = Conn::open(&daemon.socket);
    assert!(matches!(stream.send(&Request::Attach { pane }), Response::Ok));
    let Event::Output { data, reset, .. } = stream.line() else {
        panic!("expected a redraw")
    };
    assert!(reset && String::from_utf8_lossy(&B64.decode(data).unwrap()).contains("earlier-6"));

    // The CLI in a real terminal: typing reaches the pane, resizes follow, Ctrl+] d detaches.
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(ftm_bin());
    cmd.args(["attach", &ws.id.to_string()]);
    cmd.env("FTM_SOCKET", &daemon.socket);
    let mut child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let seen = Arc::new(Mutex::new(String::new()));
    let mut reader = pty.master.try_clone_reader().unwrap();
    let sink = seen.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let mut keys = pty.master.take_writer().unwrap();
    wait_for("attach to redraw the screen", Duration::from_secs(10), || {
        seen.lock().unwrap().contains("earlier-6")
    });
    keys.write_all(b"stty size; echo typed-$((4+4))\r").unwrap();
    wait_for("typed input to run in the pane", Duration::from_secs(10), || {
        seen.lock().unwrap().contains("typed-8")
    });
    assert!(
        capture(&mut req, pane).contains("30 100"),
        "attach did not size the pane to its terminal"
    );

    keys.write_all(&[0x1d, b'd']).unwrap();
    wait_for("ftm attach to exit", Duration::from_secs(10), || {
        matches!(child.try_wait(), Ok(Some(_)))
    });
    assert!(seen.lock().unwrap().contains("[detached from pane"));
    // Detaching leaves the pane running.
    req.type_in(pane, "echo after-detach\n");
    wait_for("pane to keep running", Duration::from_secs(10), || {
        capture(&mut req, pane).contains("after-detach")
    });
}

#[test]
fn rename_broadcasts_and_survives_restart() {
    let dir = temp_root("rename");
    let mut daemon = start_daemon("rename", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let ws = spawn_terminal(&mut req, &dir, "spawned");
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);

    assert!(matches!(
        req.send(&Request::Rename {
            id: ws.id,
            name: "  my task  ".into(),
        }),
        Response::Ok
    ));
    let renamed = loop {
        if let Event::Workspaces { workspaces } = events.line() {
            let w = workspaces.into_iter().find(|w| w.id == ws.id).unwrap();
            if w.title.is_some() {
                break w;
            }
        }
    };
    assert_eq!(renamed.title.as_deref(), Some("my task"));
    assert_eq!(renamed.name, "spawned");

    for blank in [" ", "\t\n"] {
        let rejected = req.try_send(&Request::Rename {
            id: ws.id,
            name: blank.into(),
        });
        assert!(matches!(rejected, Some(Response::Error { .. })), "{blank:?}: {rejected:?}");
    }
    let missing = req.try_send(&Request::Rename {
        id: WorkspaceId::new_v4(),
        name: "x".into(),
    });
    assert!(matches!(missing, Some(Response::Error { .. })));
    drop((req, events));

    daemon.restart();
    let mut req = Conn::open(&daemon.socket);
    let title = |req: &mut Conn| req.list().into_iter().find(|w| w.id == ws.id).unwrap().title;
    assert_eq!(title(&mut req).as_deref(), Some("my task"));

    // An empty name goes back to the spawn name, and the new pane still gets it as FTM_NAME.
    req.send(&Request::Rename {
        id: ws.id,
        name: String::new(),
    });
    assert_eq!(title(&mut req), None);
    let Response::Pane { pane } = req.send(&Request::NewTab { workspace: ws.id }) else {
        panic!()
    };
    req.type_in(pane, "echo name-$FTM_NAME-end\n");
    wait_for("FTM_NAME in a new tab", Duration::from_secs(10), || {
        capture(&mut req, pane).contains("name-spawned-end")
    });
}

fn folders(conn: &mut Conn) -> Vec<std::path::PathBuf> {
    match conn.send(&Request::Folders) {
        Response::Folders { folders, .. } => folders.into_iter().map(|f| f.path).collect(),
        r => panic!("{r:?}"),
    }
}

#[test]
fn folders_and_pins_are_remembered() {
    let dir = temp_root("folders");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let mut daemon = start_daemon("folders", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let first = spawn_terminal(&mut req, &a, "first");
    spawn_terminal(&mut req, &b, "second");
    // Nested workspaces do not add folders; reopening one moves it to the front.
    let Response::Spawned { .. } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: None,
        parent: Some(first.id),
        cwd: dir.clone(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: None,
    })) else {
        panic!()
    };
    assert_eq!(folders(&mut req), vec![b.clone(), a.clone()]);
    spawn_terminal(&mut req, &a, "again");
    assert_eq!(folders(&mut req), vec![a.clone(), b.clone()]);
    req.send(&Request::Pin {
        id: first.id,
        pinned: true,
    });

    // Subscribers get the folders with the tree.
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    assert!(matches!(events.line(), Event::Workspaces { .. }));
    let Event::Folders { folders: seen, .. } = events.line() else {
        panic!("expected folders")
    };
    assert_eq!(seen.len(), 2);
    drop((req, events));

    daemon.restart();
    let mut req = Conn::open(&daemon.socket);
    assert_eq!(folders(&mut req), vec![a.clone(), b.clone()]);
    assert!(req.list().iter().find(|w| w.id == first.id).unwrap().pinned);
    req.send(&Request::RemoveFolder { path: a.clone() });
    assert_eq!(folders(&mut req), vec![b]);
}

#[test]
fn finishing_a_turn_marks_the_workspace_unseen_until_looked_at() {
    let dir = temp_root("unseen");
    let mut daemon = start_daemon("unseen", |_| {});
    let mut req = Conn::open(&daemon.socket);
    let id = spawn_terminal(&mut req, &dir, "agent").id;
    let set = |req: &mut Conn, status| {
        assert!(matches!(
            req.send(&Request::SetStatus {
                id,
                status,
                background_agents: 0,
            }),
            Response::Ok
        ));
        req.list().into_iter().find(|w| w.id == id).unwrap().unseen
    };
    assert!(!set(&mut req, Status::Idle), "starting to idle is not a finished turn");
    assert!(!set(&mut req, Status::Working));
    assert!(set(&mut req, Status::Idle));
    assert!(set(&mut req, Status::Done), "stays unseen when the session then ends");
    assert!(!set(&mut req, Status::Working), "working again clears it");
    assert!(!set(&mut req, Status::NeedsInput));
    assert!(set(&mut req, Status::Done));

    daemon.restart();
    let mut req = Conn::open(&daemon.socket);
    assert!(
        req.list().into_iter().find(|w| w.id == id).unwrap().unseen,
        "unseen survives a restart"
    );
    assert!(matches!(req.send(&Request::MarkSeen { id }), Response::Ok));
    assert!(!req.list().into_iter().find(|w| w.id == id).unwrap().unseen);
    assert!(matches!(
        req.try_send(&Request::MarkSeen {
            id: WorkspaceId::new_v4()
        }),
        Some(Response::Error { .. })
    ));
}

/// Every pane's end is in ftmd's log with its cause, so a pane that dies unexpectedly leaves a trace.
#[test]
fn pane_exits_are_logged_with_their_cause() {
    let dir = temp_root("exitlog");
    let daemon = start_daemon_logged("exitlog", Path::new(env!("CARGO_BIN_EXE_ftmd")));
    let mut req = Conn::open(&daemon.socket);
    let ws = spawn_terminal(&mut req, &dir, "logged");
    let pane = ws.tabs[0].layout.panes()[0];
    let Response::Pane { pane: closed } = req.send(&Request::NewTab { workspace: ws.id }) else {
        panic!()
    };
    req.type_in(pane, "exit 3\n");
    req.send(&Request::ClosePane { pane: closed });
    wait_for("both exits in the log", Duration::from_secs(10), || {
        let log = daemon.log();
        log.contains(&format!("closing pane {pane} of logged: its holder hung up"))
            && log.contains(&format!("pane {pane}: shell exited with status 3"))
            && log.contains(&format!("closing pane {closed} of logged: ClosePane requested"))
            && log.contains(&format!("pane {closed}: hanging up shell group"))
    });
    let log = daemon.log();
    let request = format!("ClosePane {{ pane: {closed} }} from pid {}", std::process::id());
    assert!(log.contains(&request), "{log}");
}
