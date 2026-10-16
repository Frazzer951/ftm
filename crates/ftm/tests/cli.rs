//! `ftm` errors that come up before it talks to ftmd.

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
        }
    });
    let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
        .arg("ping")
        .env("FTM_SOCKET", &socket)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ftmd closed the connection"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
