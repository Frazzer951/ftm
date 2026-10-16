//! `ftm` errors that come up before it talks to ftmd.

use ftm_core::protocol::{Request, Response};
use std::process::Command;

#[test]
fn an_unreadable_brief_names_the_file() {
    let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
        .args(["spawn", "agent", "--brief", "/nonexistent/ftm-brief.md"])
        .env("FTM_SOCKET", "/nonexistent/ftmd.sock")
        .env_remove("FTM_WORKSPACE_ID")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("/nonexistent/ftm-brief.md"), "{stderr}");
}

/// Without a fetch gc cannot see an upstream deletion, so asking for both would silently remove nothing extra.
#[test]
fn gc_refuses_upstream_deleted_without_a_fetch() {
    let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
        .args(["gc", "--no-fetch", "--include-upstream-deleted"])
        .env("FTM_SOCKET", "/nonexistent/ftmd.sock")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
}

#[test]
fn ftmd_hanging_up_without_a_reply_is_an_error() {
    let dir = std::env::temp_dir().join(format!("ftm-cli-eof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("s.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let mut conn = conn.unwrap();
            let mut line = String::new();
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut conn), &mut line);
            // Answer the lookup `attach` makes first, so its own connection is the one hung up on.
            if serde_json::from_str::<Request>(&line).is_ok_and(|r| matches!(r, Request::List)) {
                let mut reply = serde_json::to_vec(&Response::Workspaces { workspaces: vec![] }).unwrap();
                reply.push(b'\n');
                std::io::Write::write_all(&mut conn, &reply).unwrap();
            }
        }
    });
    for args in [&["ping"][..], &["attach", "00000000-0000-0000-0000-000000000000"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
            .args(args)
            .env("FTM_SOCKET", &socket)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("ftmd closed the connection"), "{args:?}: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
