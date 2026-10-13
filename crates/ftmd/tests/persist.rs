//! Panes live in their holders, so they outlive ftmd restarts, and `ftm attach` drives one from a terminal.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, alive, ftm_bin, start_daemon, temp_root, wait_for, wait_for_output};
use ftm_core::model::{Kind, PaneId, SplitDir, Status, Workspace};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
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
/// marker is spelled with arithmetic so the echoed command line never matches it.
fn pids(conn: &mut Conn, pane: PaneId, tag: &str) -> (i32, i32) {
    conn.type_in(pane, &format!("echo X$((1)){tag}=$$:$PPID.\n"));
    let marker = format!("X1{tag}=");
    let mut found = None;
    wait_for(&format!("{tag} pids"), Duration::from_secs(10), || {
        let text = capture(conn, pane);
        found = text.lines().find_map(|l| {
            let (a, b) = l.strip_prefix(marker.as_str())?.strip_suffix('.')?.split_once(':')?;
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
    });
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    let (shell, holder) = pids(&mut req, pane, "before");
    req.type_in(pane, "printf 'line%s\\n' $(seq 1 40) ; echo marker-$((6*7))\n");
    wait_for_output(&mut events, pane, "marker-42");
    let tree = serde_json::to_value(req.list()).unwrap();
    drop((req, events));

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
