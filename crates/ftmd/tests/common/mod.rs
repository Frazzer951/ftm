//! Helpers shared by the ftmd integration tests.
#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ftm_core::model::{PaneId, Workspace};
use ftm_core::protocol::{Event, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
}

impl Drop for Daemon {
    /// SIGTERM, so ftmd terminates its panes before exiting; SIGKILL would leave HUP-ignoring jobs behind.
    fn drop(&mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// macOS caps socket paths at ~104 bytes, so this stays in /tmp rather than the (long) per-user temp dir.
/// Panes run `/bin/sh`, and nothing FTM-related leaks in from the environment the tests run in (which may itself be
/// an FTM pane); `configure` adds the rest.
pub fn start_daemon(name: &str, configure: impl FnOnce(&mut Command)) -> Daemon {
    let socket = PathBuf::from(format!("/tmp/ftmd-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ftmd"));
    cmd.env("FTM_SOCKET", &socket).env("SHELL", "/bin/sh");
    for key in ["FTM_WORKSPACE_ID", "FTM_PANE_ID", "FTM_NAME", "FTM_BRIEF", "FTM_AGENT_CMD"] {
        cmd.env_remove(key);
    }
    configure(&mut cmd);
    let child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "ftmd did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    Daemon { child, socket }
}

pub struct Conn {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Conn {
    pub fn open(socket: &Path) -> Self {
        let write = UnixStream::connect(socket).unwrap();
        write.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Conn { write, read }
    }

    pub fn line<T: serde::de::DeserializeOwned>(&mut self) -> T {
        let mut line = String::new();
        self.read.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    }

    pub fn send(&mut self, request: &Request) -> Response {
        let mut out = serde_json::to_vec(request).unwrap();
        out.push(b'\n');
        self.write.write_all(&out).unwrap();
        match self.line() {
            Response::Error { message } => panic!("{message}"),
            r => r,
        }
    }

    pub fn type_in(&mut self, pane: PaneId, text: &str) {
        self.send(&Request::Input {
            pane,
            data: B64.encode(text),
        });
    }

    pub fn list(&mut self) -> Vec<Workspace> {
        match self.send(&Request::List) {
            Response::Workspaces { workspaces } => workspaces,
            r => panic!("{r:?}"),
        }
    }
}

/// Reads events until `pane`'s accumulated output contains `needle`.
pub fn wait_for_output(events: &mut Conn, pane: PaneId, needle: &str) -> String {
    let mut seen = String::new();
    loop {
        if let Event::Output { pane: p, data, reset } = events.line() {
            if p != pane {
                continue;
            }
            if reset {
                seen.clear();
            }
            seen.push_str(&String::from_utf8_lossy(&B64.decode(data).unwrap()));
            if seen.contains(needle) {
                return seen;
            }
        }
    }
}

pub fn wait_until(conn: &mut Conn, what: &str, ok: impl Fn(&[Workspace]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok(&conn.list()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A fresh scratch dir under the (canonicalized) temp dir.
pub fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("ftmd-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn wait_for(what: &str, timeout: Duration, ok: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
