//! ftmd keeps serving when a pane misbehaves: it stops reading its input, its holder stalls, or a client sends
//! nonsense.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, Daemon, start_daemon, temp_root, wait_for};
use ftm_core::model::{Kind, Layout, PaneId, Workspace};
use ftm_core::protocol::{Request, Response, SpawnRequest};
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
