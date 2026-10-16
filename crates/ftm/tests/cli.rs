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
