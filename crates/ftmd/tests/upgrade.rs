//! ftmd moves onto a replaced binary by itself, keeping every pane, unless the new build cannot adopt the holders.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, alive, copy_executable, start_daemon_logged, temp_root, wait_for, wait_for_output, write_executable};
use ftm_core::model::{Kind, PaneId, Workspace};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Detection polls every 2s and waits for the file to hold still another 2s; this leaves room for a loaded CI box.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(40);

fn copy_ftmd(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("ftmd");
    copy_executable(Path::new(env!("CARGO_BIN_EXE_ftmd")), &exe);
    exe
}

/// Swaps a new file in under `exe`'s name the way an installer does: write beside it, then rename over it.
fn replace(exe: &Path, write: impl FnOnce(&Path)) {
    let tmp = exe.with_file_name(".ftmd.new");
    write(&tmp);
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, exe).unwrap();
}

fn replace_with_script(exe: &Path, script: &str) {
    replace(exe, |tmp| write_executable(tmp, format!("#!/bin/sh\n{script}\n").as_bytes()));
}

fn frame_version() -> u8 {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ftmd"))
        .arg("--handshake")
        .output()
        .unwrap();
    let handshake: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    handshake["frame"].as_u64().unwrap() as u8
}

fn spawn_terminal(conn: &mut Conn, cwd: &Path) -> PaneId {
    let Response::Spawned { workspace } = conn.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: Some("upgrade".into()),
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
    only_pane(&workspace)
}

fn only_pane(ws: &Workspace) -> PaneId {
    ws.tabs[0].layout.panes()[0]
}

fn capture(conn: &mut Conn, pane: PaneId) -> String {
    match conn.send(&Request::Capture { pane, raw: false }) {
        Response::Output { data } => String::from_utf8(B64.decode(data).unwrap()).unwrap(),
        r => panic!("{r:?}"),
    }
}

/// Types an `echo` whose output (spelled with arithmetic, so the echoed command never matches) must show up.
fn echo(conn: &mut Conn, pane: PaneId, tag: &str) {
    conn.type_in(pane, &format!("echo {tag}-$((40+2))\n"));
    let needle = format!("{tag}-42");
    wait_for(&needle, Duration::from_secs(10), || capture(conn, pane).contains(&needle));
}

fn ping(conn: &mut Conn) -> String {
    match conn.send(&Request::Ping) {
        Response::Pong { version } => version,
        r => panic!("{r:?}"),
    }
}

/// Reads events until an `UpdateBlocked` arrives, and returns its reason.
fn wait_blocked(events: &mut Conn) -> String {
    events.set_read_timeout(Duration::from_millis(500));
    let deadline = Instant::now() + SWITCH_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(Event::UpdateBlocked { reason, .. }) = events.try_line() {
            return reason;
        }
    }
    panic!("no update_blocked event within {SWITCH_TIMEOUT:?}");
}

#[test]
fn a_replaced_binary_takes_over_and_keeps_the_panes() {
    let dir = temp_root("upgrade-ok");
    let exe = copy_ftmd(&dir);
    let mut daemon = start_daemon_logged("upgrade-ok", &exe);
    let first = daemon.pid();
    let mut conn = Conn::open(&daemon.socket);
    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));
    let pane = spawn_terminal(&mut conn, &dir);
    echo(&mut conn, pane, "before");

    replace(&exe, |tmp| copy_executable(Path::new(env!("CARGO_BIN_EXE_ftmd")), tmp));

    wait_for("the new ftmd to restore", SWITCH_TIMEOUT, || {
        daemon.log().contains("restored")
    });
    let log = daemon.log();
    assert!(log.contains("taking over"), "{log}");
    assert!(log.contains("restored 1 workspaces, 1 panes (0 gone)"), "{log}");
    let second = daemon.latest_pid().unwrap();
    assert_ne!(second, first);
    assert!(alive(second));
    wait_for("the old ftmd to exit", Duration::from_secs(10), || {
        daemon.child().try_wait().unwrap().is_some()
    });

    // Clients of the old ftmd see their connection close, and reconnect to the new one on the same socket.
    events.set_read_timeout(Duration::from_secs(10));
    while events.try_line::<Event>().is_some() {}
    assert!(conn.try_send(&Request::Ping).is_none());

    let mut conn = Conn::open(&daemon.socket);
    ping(&mut conn);
    assert!(capture(&mut conn, pane).contains("before-42"));
    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));
    conn.type_in(pane, "echo after-$((40+3))\n");
    wait_for_output(&mut events, pane, "after-43");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_build_with_another_frame_version_does_not_take_over() {
    let dir = temp_root("upgrade-frame");
    let exe = copy_ftmd(&dir);
    let mut daemon = start_daemon_logged("upgrade-frame", &exe);
    let mut conn = Conn::open(&daemon.socket);
    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));
    let pane = spawn_terminal(&mut conn, &dir);
    echo(&mut conn, pane, "before");

    let other = frame_version().wrapping_add(1);
    replace_with_script(&exe, &format!(r#"echo '{{"version":"9.9.9","frame":{other}}}'"#));

    let reason = wait_blocked(&mut events);
    assert!(reason.contains("holder protocol"), "{reason}");
    assert!(daemon.child().try_wait().unwrap().is_none(), "ftmd exited:\n{}", daemon.log());
    assert!(!daemon.log().contains("taking over"));
    ping(&mut conn);
    echo(&mut conn, pane, "after");

    // A late subscriber (the app reconnecting) still hears about it.
    let mut late = Conn::open(&daemon.socket);
    assert!(matches!(late.send(&Request::Subscribe), Response::Ok));
    wait_blocked(&mut late);

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_build_from_another_channel_does_not_take_over() {
    let dir = temp_root("upgrade-channel");
    let exe = copy_ftmd(&dir);
    let mut daemon = start_daemon_logged("upgrade-channel", &exe);
    let mut conn = Conn::open(&daemon.socket);
    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));
    let pane = spawn_terminal(&mut conn, &dir);
    echo(&mut conn, pane, "before");

    let frame = frame_version();
    let other = if ftm_core::paths::CHANNEL == "nightly" {
        "stable"
    } else {
        "nightly"
    };
    replace_with_script(
        &exe,
        &format!(
            r#"if [ "$1" = --handshake ]; then echo '{{"version":"9.9.9","frame":{frame},"channel":"{other}"}}'; else touch "$0.started"; sleep 30; fi"#
        ),
    );

    let reason = wait_blocked(&mut events);
    assert!(reason.contains(&format!("{other} build")), "{reason}");
    assert!(daemon.child().try_wait().unwrap().is_none(), "ftmd exited:\n{}", daemon.log());
    assert!(
        !exe.with_file_name("ftmd.started").exists(),
        "the other channel's ftmd was started"
    );
    ping(&mut conn);
    echo(&mut conn, pane, "after");

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_ftmd_that_fails_to_start_leaves_the_old_one_serving() {
    let dir = temp_root("upgrade-fail");
    let exe = copy_ftmd(&dir);
    let mut daemon = start_daemon_logged("upgrade-fail", &exe);
    let mut conn = Conn::open(&daemon.socket);
    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));
    let pane = spawn_terminal(&mut conn, &dir);
    echo(&mut conn, pane, "before");

    let frame = frame_version();
    replace_with_script(
        &exe,
        &format!(
            r#"if [ "$1" = --handshake ]; then echo '{{"version":"9.9.9","frame":{frame}}}'; else echo broken >&2; exit 3; fi"#
        ),
    );

    let reason = wait_blocked(&mut events);
    assert!(reason.contains("did not take over"), "{reason}");
    assert!(daemon.child().try_wait().unwrap().is_none(), "ftmd exited:\n{}", daemon.log());
    // The same connection keeps working: the state lock taken for the handover was given back.
    ping(&mut conn);
    echo(&mut conn, pane, "after");
    let mut fresh = Conn::open(&daemon.socket);
    ping(&mut fresh);

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}
