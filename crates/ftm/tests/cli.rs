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

#[test]
fn spawn_refuses_an_unknown_color() {
    let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
        .args(["spawn", "terminal", "--color", "teal"])
        .env("FTM_SOCKET", "/nonexistent/ftmd.sock")
        .env_remove("FTM_WORKSPACE_ID")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown color \"teal\""), "{stderr}");
}

#[test]
fn move_takes_exactly_one_destination() {
    let id = "00000000-0000-0000-0000-000000000001";
    for args in [vec!["move", id], vec!["move", id, "--parent", id, "--top-level"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
            .args(&args)
            .env("FTM_SOCKET", "/nonexistent/ftmd.sock")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
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

fn with_config(name: &str, config: Option<&str>) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ftm-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ftm")).unwrap();
    if let Some(config) = config {
        std::fs::write(dir.join("ftm/config.toml"), config).unwrap();
    }
    dir
}

fn session_start(config_home: &std::path::Path, source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ftm"))
        .args(["hook", "session-start"])
        .env("XDG_CONFIG_HOME", config_home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(r#"{{"hook_event_name":"SessionStart","source":"{source}"}}"#);
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The setting is read when the hook runs, so flipping it reaches the next agent without restarting ftmd.
#[test]
fn session_start_tells_fresh_agents_about_tasks_while_they_are_on() {
    let on = with_config("tasks-on", None);
    let out: serde_json::Value = serde_json::from_str(&session_start(&on, "startup")).unwrap();
    let guide = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(
        guide.contains("ftm ask") && guide.contains("[FTM task #N answered]"),
        "{guide}"
    );
    assert_eq!(session_start(&on, "compact"), "");
    let off = with_config("tasks-off", Some("[tasks]\nenabled = false\n"));
    assert_eq!(session_start(&off, "startup"), "");
    let broken = with_config("tasks-broken", Some("[tasks\n"));
    assert!(session_start(&broken, "startup").contains("additionalContext"));

    let out = Command::new(env!("CARGO_BIN_EXE_ftm"))
        .args(["ask", "q", "--id", "00000000-0000-0000-0000-000000000001"])
        .env("XDG_CONFIG_HOME", &off)
        .env("FTM_SOCKET", "/nonexistent/ftmd.sock")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("tasks are off in FTM settings"), "{stderr}");
    for dir in [on, off, broken] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
