//! ftmd keeps serving when a pane misbehaves: it stops reading its input, its holder stalls, or a client sends
//! nonsense.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, Daemon, start_daemon, temp_root, wait_for};
use ftm_core::model::{Kind, Layout, PaneId, Status, Workspace};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use std::path::Path;
use std::time::Duration;

fn spawn_command(conn: &mut Conn, cwd: &Path, command: Option<&str>) -> (Workspace, PaneId) {
    let Response::Spawned { workspace } = conn.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: Some("t".into()),
        parent: None,
        cwd: cwd.to_path_buf(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: command.map(str::to_string),
    })) else {
        panic!("expected spawned")
    };
    let Layout::Pane { id } = workspace.tabs[0].layout else {
        panic!("expected one pane")
    };
    (workspace, id)
}

fn holder_sockets(daemon: &Daemon) -> usize {
    std::fs::read_dir(daemon.holders()).map_or(0, Iterator::count)
}

#[test]
fn a_pane_that_stops_reading_input_stalls_nothing_else() {
    let dir = temp_root("stall");
    let daemon = start_daemon("stall", |_| {});
    let mut conn = Conn::open(&daemon.socket);
    let (_, pane) = spawn_command(&mut conn, &dir, Some("sleep 100"));

    let socket = daemon.socket.clone();
    std::thread::spawn(move || {
        let mut paste = Conn::open(&socket);
        paste.try_send(&Request::Input {
            pane,
            data: B64.encode(b"echo pasted\n".repeat(1 << 16)),
        });
    });
    // The terminal echoes what fits in its input queue; the rest backs up behind it.
    wait_for("the paste to start", Duration::from_secs(10), || {
        capture(&mut conn, pane).contains("echo pasted")
    });

    conn.set_read_timeout(Duration::from_secs(5));
    assert!(matches!(
        conn.try_send(&Request::Resize {
            pane,
            cols: 100,
            rows: 30
        }),
        Some(Response::Ok)
    ));
    assert!(matches!(conn.try_send(&Request::Ping), Some(Response::Pong { .. })));
    assert!(matches!(conn.try_send(&Request::ClosePane { pane }), Some(Response::Ok)));
    wait_for("the pane's holder to exit", Duration::from_secs(5), || {
        holder_sockets(&daemon) == 0
    });
    assert!(matches!(conn.try_send(&Request::Ping), Some(Response::Pong { .. })));
}

/// A holder gives up on an ftmd that stops reading its output for 5s. ftmd reconnects and gets the buffer again,
/// instead of freezing the pane or taking it for dead.
#[test]
fn a_stalled_ftmd_reconnects_to_its_holders() {
    let dir = temp_root("stalled-reader");
    let mut daemon = start_daemon("stalled-reader", |_| {});
    let mut conn = Conn::open(&daemon.socket);
    let (_, pane) = spawn_command(
        &mut conn,
        &dir,
        Some("sleep 1; yes | head -c 2000000; echo done-$((1+1)); touch flooded"),
    );

    let pid = daemon.pid();
    // SAFETY: kill takes no pointers.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    // The flood only gets through once the holder has given up on the stopped ftmd.
    wait_for("the holder to stop waiting on ftmd", Duration::from_secs(30), || {
        dir.join("flooded").exists()
    });
    // SAFETY: kill takes no pointers.
    unsafe { libc::kill(pid, libc::SIGCONT) };
    assert!(daemon.child().try_wait().unwrap().is_none());

    wait_for("the output after the stall", Duration::from_secs(20), || {
        match conn.send(&Request::Capture { pane, raw: false }) {
            Response::Output { data } => String::from_utf8_lossy(&B64.decode(data).unwrap()).contains("done-2"),
            r => panic!("{r:?}"),
        }
    });
}

fn capture(conn: &mut Conn, pane: PaneId) -> String {
    match conn.send(&Request::Capture { pane, raw: false }) {
        Response::Output { data } => String::from_utf8_lossy(&B64.decode(data).unwrap()).into_owned(),
        r => panic!("{r:?}"),
    }
}

#[test]
fn a_zero_size_resize_leaves_the_pane_working() {
    let dir = temp_root("zero-size");
    let daemon = start_daemon("zero-size", |_| {});
    let mut conn = Conn::open(&daemon.socket);
    let (_, pane) = spawn_command(&mut conn, &dir, None);
    for (rows, cols) in [(0, 80), (24, 0), (0, 0)] {
        assert!(matches!(conn.send(&Request::Resize { pane, cols, rows }), Response::Ok));
    }
    // A double-width character must fit even at the smallest size.
    conn.type_in(pane, "printf '\\344\\270\\200\\n'; touch printed\n");
    wait_for("the wide character to be printed", Duration::from_secs(10), || {
        dir.join("printed").exists()
    });
    conn.send(&Request::Resize {
        pane,
        cols: 80,
        rows: 24,
    });
    conn.type_in(pane, "echo zero-$((1+1))\n");
    wait_for("the pane to keep working", Duration::from_secs(10), || {
        capture(&mut conn, pane).contains("zero-2")
    });
}

/// Running out of file descriptors fails one accept, not the whole daemon (and with it every pane's connection).
#[cfg(target_os = "linux")]
#[test]
fn running_out_of_descriptors_does_not_stop_ftmd() {
    use common::start_daemon_logged;
    let mut daemon = start_daemon_logged("emfile", Path::new(env!("CARGO_BIN_EXE_ftmd")));
    let pid = daemon.pid();
    let open = std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap().count() as u64;
    let limit = libc::rlimit {
        rlim_cur: open + 4,
        rlim_max: open + 4,
    };
    assert_eq!(
        // SAFETY: `limit` is a live rlimit, and a null old-limit pointer is allowed.
        unsafe { libc::prlimit(pid, libc::RLIMIT_NOFILE, &raw const limit, std::ptr::null_mut()) },
        0
    );

    let flood: Vec<_> = (0..16)
        .filter_map(|_| std::os::unix::net::UnixStream::connect(&daemon.socket).ok())
        .collect();
    assert!(!flood.is_empty());
    wait_for("an accept to fail", Duration::from_secs(10), || {
        daemon.log().contains("Too many open files") || daemon.child().try_wait().unwrap().is_some()
    });
    drop(flood);

    assert!(daemon.child().try_wait().unwrap().is_none(), "ftmd exited");
    let mut conn = Conn::open(&daemon.socket);
    assert!(matches!(conn.send(&Request::Ping), Response::Pong { .. }));
}

/// `Unknown` only exists so a client can read a newer ftmd; ftmd never takes one in, so it never hands one out.
#[test]
fn unknown_kinds_and_statuses_are_refused() {
    let dir = temp_root("unknown");
    let daemon = start_daemon("unknown", |_| {});
    let mut conn = Conn::open(&daemon.socket);
    let (ws, _) = spawn_command(&mut conn, &dir, None);
    let refused = |r: Option<Response>| matches!(r, Some(Response::Error { .. }));
    assert!(refused(conn.try_send(&Request::SetStatus {
        id: ws.id,
        status: Status::Unknown,
        background_agents: 0,
    })));
    assert!(refused(conn.try_send(&Request::Spawn(SpawnRequest {
        kind: Kind::Unknown,
        name: None,
        parent: None,
        cwd: dir.to_path_buf(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: None,
    }))));
    let list = conn.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].status, Status::Starting);
}

/// A client drops output for panes it does not know yet, so a new pane's first output (what its holder had before
/// ftmd connected) must come after the layout that adds it.
#[test]
fn a_new_pane_is_announced_before_its_output() {
    let dir = temp_root("announce");
    let daemon = start_daemon("announce", |_| {});
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    let mut conn = Conn::open(&daemon.socket);
    let (_, pane) = spawn_command(&mut conn, &dir, Some("echo early-$((1+1)); sleep 100"));

    let mut announced = false;
    let mut seen = String::new();
    while !seen.contains("early-2") {
        match events.line() {
            Event::Workspaces { workspaces } => {
                announced |= workspaces.iter().any(|w| w.tabs.iter().any(|t| t.layout.contains(pane)));
            },
            Event::Output { pane: p, data, reset } if p == pane => {
                assert!(announced, "output for pane {pane} before any layout had it");
                if reset {
                    seen.clear();
                }
                seen.push_str(&String::from_utf8_lossy(&B64.decode(data).unwrap()));
            },
            _ => {},
        }
    }
}

/// Without the hook settings an agent never reports a status, so it must not wait in `starting` for one, nor count as
/// failed when its shell exits later.
#[test]
fn agents_without_status_hooks_are_not_left_starting() {
    let dir = temp_root("no-hooks");
    let settings = format!("/tmp/ftmd-{}-no-hooks.claude-settings.json", std::process::id());
    std::fs::create_dir_all(&settings).unwrap();
    let daemon = start_daemon("no-hooks", |c| {
        c.env("FTM_AGENT_CMD", "true");
    });
    let mut conn = Conn::open(&daemon.socket);
    let Response::Spawned { workspace } = conn.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Agent,
        name: Some("a".into()),
        parent: None,
        cwd: dir.to_path_buf(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: None,
    })) else {
        panic!("expected spawned")
    };
    assert_eq!(workspace.status, Status::Idle);
    let pane = workspace.tabs[0].layout.panes()[0];
    conn.type_in(pane, "exit\n");
    wait_for("the shell to exit", Duration::from_secs(10), || {
        conn.list()[0].tabs.is_empty()
    });
    assert_eq!(conn.list()[0].status, Status::Idle);
    let _ = std::fs::remove_dir(&settings);
}

/// SIGKILLs the session a test's pane runs in, if the test failed before closing it.
struct KillSession(std::path::PathBuf);

impl Drop for KillSession {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            // SAFETY: getsid takes no pointers.
            let sid = unsafe { libc::getsid(pid) };
            if sid > 0 {
                let _ = std::process::Command::new("pkill")
                    .args(["-KILL", "-s", &sid.to_string()])
                    .status();
            }
        }
    }
}

/// Stops ftmd until the holder of a flooding pane hangs up on it, and has `queued` sent while it is stopped.
fn stall_until_hung_up(daemon: &Daemon, dir: &Path, queued: Request) -> std::thread::JoinHandle<Option<Response>> {
    let pid = daemon.pid();
    // SAFETY: kill takes no pointers.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    wait_for("the holder to stop waiting on ftmd", Duration::from_secs(30), || {
        dir.join("flooded").exists()
    });
    let socket = daemon.socket.clone();
    let (sent, wait) = std::sync::mpsc::channel();
    let request = std::thread::spawn(move || {
        let mut conn = Conn::open(&socket);
        conn.set_read_timeout(Duration::from_secs(30));
        let _ = sent.send(());
        conn.try_send(&queued)
    });
    wait.recv().unwrap();
    // Queued in ftmd's socket before it runs again.
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: kill takes no pointers.
    unsafe { libc::kill(pid, libc::SIGCONT) };
    request
}

/// A pane closed while ftmd is reconnecting to its holder still has its processes killed, rather than the Kill going
/// to the connection the holder already hung up on.
#[test]
fn closing_a_pane_whose_holder_hung_up_kills_it() {
    let dir = temp_root("close-hung-up");
    let daemon = start_daemon("close-hung-up", |_| {});
    let mut conn = Conn::open(&daemon.socket);
    let _cleanup = KillSession(dir.join("pid"));
    let (_, pane) = spawn_command(
        &mut conn,
        &dir,
        Some("echo $$ > pid; sleep 1; yes | head -c 3000000; touch flooded; sleep 300"),
    );

    let closed = stall_until_hung_up(&daemon, &dir, Request::ClosePane { pane });
    assert!(matches!(closed.join().unwrap(), Some(Response::Ok)));
    wait_for("the pane's holder to exit", Duration::from_secs(15), || {
        holder_sockets(&daemon) == 0
    });
}

/// A resize sent while the holder has hung up on ftmd still reaches the terminal once ftmd reconnects.
#[test]
fn a_resize_while_the_holder_hung_up_still_lands() {
    let dir = temp_root("resize-hung-up");
    let daemon = start_daemon("resize-hung-up", |_| {});
    let mut conn = Conn::open(&daemon.socket);
    let _cleanup = KillSession(dir.join("pid"));
    let (_, pane) = spawn_command(
        &mut conn,
        &dir,
        Some("echo $$ > pid; sleep 1; yes | head -c 3000000; touch flooded; read x; stty size > size"),
    );

    let resized = stall_until_hung_up(
        &daemon,
        &dir,
        Request::Resize {
            pane,
            cols: 100,
            rows: 30,
        },
    );
    assert!(matches!(resized.join().unwrap(), Some(Response::Ok)));
    assert!(matches!(
        conn.try_send(&Request::Input {
            pane,
            data: B64.encode("\n"),
        }),
        Some(Response::Ok)
    ));
    let mut size = String::new();
    wait_for("the shell to report its size", Duration::from_secs(15), || {
        size = std::fs::read_to_string(dir.join("size")).unwrap_or_default();
        size.ends_with('\n')
    });
    assert_eq!(size.trim(), "30 100");
}
