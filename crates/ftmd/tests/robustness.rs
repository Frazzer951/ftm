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
    std::fs::read_dir(daemon.holders()).map_or(0, |d| d.count())
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
    std::thread::sleep(Duration::from_millis(500));

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
    let (_, pane) = spawn_command(&mut conn, &dir, Some("sleep 1; yes | head -c 2000000; echo done-$((1+1))"));

    let pid = daemon.pid();
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    std::thread::sleep(Duration::from_secs(8));
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
    let mut daemon = start_daemon("emfile", |_| {});
    let pid = daemon.pid();
    let open = std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap().count() as u64;
    let limit = libc::rlimit {
        rlim_cur: open + 4,
        rlim_max: open + 4,
    };
    assert_eq!(
        unsafe { libc::prlimit(pid, libc::RLIMIT_NOFILE, &limit, std::ptr::null_mut()) },
        0
    );

    let flood: Vec<_> = (0..16)
        .filter_map(|_| std::os::unix::net::UnixStream::connect(&daemon.socket).ok())
        .collect();
    assert!(!flood.is_empty());
    std::thread::sleep(Duration::from_millis(500));
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
                announced |= workspaces.iter().any(|w| w.tabs.iter().any(|t| t.layout.contains(pane)))
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
