//! Helpers shared by the ftmd integration tests.
//!
//! Tests in a binary run in parallel, on CI boxes much slower than a laptop. To keep a new test from flaking:
//! - Start ftmd with `start_daemon`/`start_daemon_logged`: its own socket, and HOME, state, config and data dirs, so
//!   it never touches the user's daemon or files. Run git through `git`, and `ftm` commands that run git (`ftm gc`)
//!   through `without_user_git_config`. Scratch dirs come from `temp_root`, which removes them on drop.
//! - Write a file that will be executed with `write_executable`/`copy_executable`, never `std::fs::write`/`copy`:
//!   another thread's fork can hold the writable fd open and exec then fails with ETXTBSY.
//! - Wait for a condition (`wait_for`, `wait_until`, `wait_for_output`), never a fixed sleep, with a deadline of
//!   seconds that a loaded CI box still meets.
//! - Never change the process environment (`std::env::set_var`); pass values to the child `Command` instead.
//! - Name everything per test (the `name` given to `start_daemon` and `temp_root` must be unique in the binary).
#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ftm_core::model::{PaneId, Workspace};
use ftm_core::protocol::{Event, Request, Response};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct Daemon {
    child: Child,
    pub socket: PathBuf,
    envs: Vec<(OsString, Option<OsString>)>,
    state_home: TempRoot,
    exe: PathBuf,
    /// ftmd's output, kept only by `start_daemon_logged`.
    log: Option<PathBuf>,
}

impl Daemon {
    fn spawn(exe: &Path, envs: &[(OsString, Option<OsString>)], socket: &Path, log: Option<&Path>) -> Child {
        let mut cmd = Command::new(exe);
        for (key, value) in envs {
            match value {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        if let Some(log) = log {
            let file = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
            cmd.stdout(file.try_clone().unwrap()).stderr(file);
        }
        let child = cmd.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(socket).is_err() {
            assert!(Instant::now() < deadline, "ftmd did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    /// SIGTERM: ftmd exits and leaves the panes to their holders. Also stops an ftmd this one handed over to.
    pub fn stop(&mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
        let _ = self.child.wait();
        if let Some(pid) = self.log.as_ref().and_then(|_| self.latest_pid())
            && pid != self.child.id() as i32
        {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(10);
            while alive(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// Stops ftmd and starts a new one with the same environment, which reconnects to the panes.
    pub fn restart(&mut self) {
        self.stop();
        self.child = Self::spawn(&self.exe, &self.envs, &self.socket, self.log.as_deref());
    }

    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// The first ftmd's process, to see whether it has exited.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.log.as_ref().expect("started with a log")).unwrap_or_default()
    }

    /// The pid in the last "listening on"/"taking over" line: the ftmd serving the socket now.
    pub fn latest_pid(&self) -> Option<i32> {
        self.log()
            .lines()
            .rev()
            .filter(|l| l.contains("listening on") || l.contains("taking over"))
            .find_map(|l| l.rsplit_once("(pid ")?.1.split_once(')')?.0.parse().ok())
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
        let _ = std::fs::remove_file(self.socket.with_extension("claude-settings.json"));
    }
}

/// macOS caps socket paths at ~104 bytes, so this stays in /tmp rather than the (long) per-user temp dir.
/// Panes run `/bin/sh`, nothing FTM-related leaks in from the environment the tests run in (which may itself be an
/// FTM pane), and HOME, the saved tree, config and worktrees are private dirs rather than the user's, so neither a
/// login shell's profile nor git's global config comes in; `configure` adds the rest.
pub fn start_daemon(name: &str, configure: impl FnOnce(&mut Command)) -> Daemon {
    start(name, Path::new(env!("CARGO_BIN_EXE_ftmd")), false, configure)
}

/// Runs `exe` (a copy of ftmd the test may replace) with its output kept for `Daemon::log`.
pub fn start_daemon_logged(name: &str, exe: &Path) -> Daemon {
    start(name, exe, true, |_| {})
}

fn start(name: &str, exe: &Path, logged: bool, configure: impl FnOnce(&mut Command)) -> Daemon {
    let socket = PathBuf::from(format!("/tmp/ftmd-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let state_home = temp_root(&format!("{name}-state"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ftmd"));
    cmd.env("FTM_SOCKET", &socket)
        .env("SHELL", "/bin/sh")
        .env("HOME", state_home.join("home"))
        .env("XDG_STATE_HOME", &state_home)
        .env("XDG_CONFIG_HOME", state_home.join("config"))
        .env("XDG_DATA_HOME", state_home.join("data"));
    without_user_git_config(&mut cmd);
    std::fs::create_dir_all(state_home.join("home")).unwrap();
    for key in [
        "FTM_WORKSPACE_ID",
        "FTM_PANE_ID",
        "FTM_NAME",
        "FTM_BRIEF",
        "FTM_SETUP",
        "FTM_AGENT_CMD",
    ] {
        cmd.env_remove(key);
    }
    configure(&mut cmd);
    let envs: Vec<_> = cmd.get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();
    let log = logged.then(|| state_home.join("ftmd.log"));
    let child = Daemon::spawn(exe, &envs, &socket, log.as_deref());
    Daemon {
        child,
        socket,
        envs,
        state_home,
        exe: exe.to_path_buf(),
        log,
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

    /// macOS refuses (EINVAL) once ftmd has closed the connection, which the next read reports anyway.
    pub fn set_read_timeout(&self, timeout: Duration) {
        let _ = self.write.set_read_timeout(Some(timeout));
    }

    /// `None` once ftmd closes the connection (or the read times out).
    pub fn try_line<T: serde::de::DeserializeOwned>(&mut self) -> Option<T> {
        let mut line = String::new();
        match self.read.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))),
        }
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

/// Keeps the user's and the system's git config (signing, hooks, templates) out of a git the test runs, directly or
/// through ftmd or `ftm`.
pub fn without_user_git_config(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = without_user_git_config(Command::new("git").arg("-C").arg(dir).args(args))
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A fresh scratch dir under the (canonicalized) temp dir, removed on drop.
pub fn temp_root(name: &str) -> TempRoot {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("ftmd-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    TempRoot(root)
}

pub struct TempRoot(PathBuf);

impl std::ops::Deref for TempRoot {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for TempRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for TempRoot {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn wait_for(what: &str, timeout: Duration, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Executables are written by a child process: a fork from another test thread inherits any writable fd this process
/// holds on the file until that child execs, and exec refuses a file open for writing (ETXTBSY).
pub fn write_executable(path: &Path, contents: &[u8]) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(contents).unwrap();
    assert!(child.wait().unwrap().success(), "writing {}", path.display());
}

/// See `write_executable`.
pub fn copy_executable(from: &Path, to: &Path) {
    let status = Command::new("cp").arg(from).arg(to).status().unwrap();
    assert!(status.success(), "copying {} to {}", from.display(), to.display());
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
