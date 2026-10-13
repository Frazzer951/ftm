//! Helpers shared by the ftmd integration tests.
#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ftm_core::model::{PaneId, Workspace};
use ftm_core::protocol::{Event, Request, Response};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
    envs: Vec<(OsString, Option<OsString>)>,
    state_home: PathBuf,
}

impl Daemon {
    fn spawn(envs: &[(OsString, Option<OsString>)], socket: &Path) -> Child {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ftmd"));
        for (key, value) in envs {
            match value {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        let child = cmd.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(socket).is_err() {
            assert!(Instant::now() < deadline, "ftmd did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    /// SIGTERM: ftmd exits and leaves the panes to their holders.
    pub fn stop(&mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let _ = self.child.wait();
    }

    /// Stops ftmd and starts a new one with the same environment, which reconnects to the panes.
    pub fn restart(&mut self) {
        self.stop();
        self.child = Self::spawn(&self.envs, &self.socket);
    }

    pub fn state_home(&self) -> &Path {
        &self.state_home
    }

    pub fn holders(&self) -> PathBuf {
        ftm_core::paths::holders_dir(&self.socket)
    }
}

impl Drop for Daemon {
    /// Closing every workspace is what kills panes now (stopping ftmd leaves them running), so do that first and
    /// wait for the holders to go.
    fn drop(&mut self) {
        if let Ok(stream) = UnixStream::connect(&self.socket) {
            let mut conn = Conn::from_stream(stream);
            if let Some(Response::Workspaces { workspaces }) = conn.try_send(&Request::List) {
                for ws in workspaces {
                    conn.try_send(&Request::Close {
                        id: ws.id,
                        remove_worktree: false,
                    });
                }
            }
        }
        let holders = self.holders();
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read_dir(&holders).is_ok_and(|mut d| d.next().is_some()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        self.stop();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&holders);
        let _ = std::fs::remove_dir_all(&self.state_home);
    }
}

/// macOS caps socket paths at ~104 bytes, so this stays in /tmp rather than the (long) per-user temp dir.
/// Panes run `/bin/sh`, nothing FTM-related leaks in from the environment the tests run in (which may itself be an
/// FTM pane), and the saved tree goes to a private state dir; `configure` adds the rest.
pub fn start_daemon(name: &str, configure: impl FnOnce(&mut Command)) -> Daemon {
    let socket = PathBuf::from(format!("/tmp/ftmd-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let state_home = temp_root(&format!("{name}-state"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ftmd"));
    cmd.env("FTM_SOCKET", &socket)
        .env("SHELL", "/bin/sh")
        .env("XDG_STATE_HOME", &state_home);
    for key in ["FTM_WORKSPACE_ID", "FTM_PANE_ID", "FTM_NAME", "FTM_BRIEF", "FTM_AGENT_CMD"] {
        cmd.env_remove(key);
    }
    configure(&mut cmd);
    let envs: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();
    let child = Daemon::spawn(&envs, &socket);
    Daemon {
        child,
        socket,
        envs,
        state_home,
    }
}

pub struct Conn {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Conn {
    pub fn open(socket: &Path) -> Self {
        Self::from_stream(UnixStream::connect(socket).unwrap())
    }

    fn from_stream(write: UnixStream) -> Self {
        write.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Conn { write, read }
    }

    /// Never panics, for cleanup.
    pub fn try_send(&mut self, request: &Request) -> Option<Response> {
        let mut out = serde_json::to_vec(request).ok()?;
        out.push(b'\n');
        self.write.write_all(&out).ok()?;
        let mut line = String::new();
        self.read.read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
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

pub fn wait_for(what: &str, timeout: Duration, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `ftm` is another package, so its binary is not a `CARGO_BIN_EXE_*` here; build it into the same target dir.
pub fn ftm_bin() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "--quiet", "-p", "ftm", "--bin", "ftm"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .unwrap();
    assert!(status.success(), "building ftm");
    Path::new(env!("CARGO_BIN_EXE_ftmd")).with_file_name("ftm")
}
