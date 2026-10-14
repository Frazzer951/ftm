//! Moving a running ftmd onto a new build of its own binary (a `brew upgrade`, a reinstall, a dev rebuild) without
//! losing a pane. ftmd watches the file it was started from; once a replacement has settled and answers
//! `--handshake` with the same holder frame version, ftmd starts it with the listening socket and a readiness pipe,
//! and exits once the new ftmd has checked both and parsed the saved tree. The socket stays bound throughout, so
//! clients queue rather than find nothing and launch a second ftmd. The new ftmd then adopts every holder, as after
//! any restart. A replacement that cannot adopt the holders is reported with `Event::UpdateBlocked` instead, as is
//! one from another release channel: on Linux, installing the nightly package over stable (or back) puts the other
//! channel's ftmd at this path, and that one belongs to the other channel's socket. This ftmd keeps serving its panes
//! until it exits, and the other channel starts a daemon of its own.

use crate::frame;
use crate::state::{self, Shared};
use eyre::{Result, WrapErr, bail, eyre};
use ftm_core::paths;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub const HANDSHAKE: &str = "--handshake";
const LISTEN_FD: &str = "FTMD_LISTEN_FD";
const READY_FD: &str = "FTMD_READY_FD";

const POLL: Duration = Duration::from_secs(2);
/// A bundle being copied into place changes for a while; only a file that has held still this long is tried.
const SETTLE: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Covers parsing a large saved tree on a loaded machine.
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const PARENT_EXIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, Deserialize)]
pub struct Handshake {
    pub version: String,
    pub frame: u8,
    /// Absent from builds that predate it.
    #[serde(default)]
    pub channel: Option<String>,
}

pub fn print_handshake() {
    let handshake = Handshake {
        version: env!("CARGO_PKG_VERSION").to_string(),
        frame: frame::VERSION,
        channel: Some(paths::CHANNEL.to_string()),
    };
    println!("{}", serde_json::to_string(&handshake).expect("serializable"));
}

/// What the ftmd being replaced handed over, read from the environment and removed from it before any thread starts,
/// so pane holders and their shells never inherit it.
pub struct Inherited {
    listen: RawFd,
    ready: OwnedFd,
    parent: libc::pid_t,
}

impl Inherited {
    /// # Safety
    /// Must run while the process is still single-threaded.
    pub unsafe fn take() -> Option<Inherited> {
        let fd = |key: &str| {
            let value = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            value?.parse::<RawFd>().ok().filter(|fd| *fd > 2)
        };
        let (listen, ready) = (fd(LISTEN_FD), fd(READY_FD));
        for fd in [listen, ready].into_iter().flatten() {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let (listen, ready) = (listen?, ready?);
        Some(Inherited {
            listen,
            ready: unsafe { OwnedFd::from_raw_fd(ready) },
            parent: unsafe { libc::getppid() },
        })
    }

    /// The handed-over fd, provided it is a listening Unix socket bound to `socket`.
    pub fn listener(&self, socket: &Path) -> Result<std::os::unix::net::UnixListener> {
        let fd = self.listen;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0 || stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
            bail!("{LISTEN_FD}={fd} is not a socket");
        }
        let mut kind: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ok = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut libc::c_int).cast(),
                &mut len,
            )
        };
        if ok != 0 || kind != libc::SOCK_STREAM {
            bail!("{LISTEN_FD}={fd} is not a stream socket");
        }
        let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
        let addr = listener.local_addr().wrap_err("reading the handed-over socket's address")?;
        if addr.as_pathname() != Some(socket) {
            bail!("{LISTEN_FD}={fd} is bound to {addr:?}, not {}", socket.display());
        }
        // macOS has no SO_ACCEPTCONN. listen() again is harmless on a listening socket and fails on a connected one.
        if unsafe { libc::listen(fd, libc::SOMAXCONN) } != 0 {
            return Err(std::io::Error::last_os_error()).wrap_err_with(|| format!("{LISTEN_FD}={fd} cannot listen"));
        }
        listener.set_nonblocking(true)?;
        Ok(listener)
    }

    /// Tells the old ftmd it may exit, then waits until it has, so nothing it still holds races the restore.
    pub fn ready(self) -> Result<()> {
        let mut ready = std::fs::File::from(self.ready);
        std::io::Write::write_all(&mut ready, b"r").wrap_err("signalling the old ftmd")?;
        drop(ready);
        let deadline = Instant::now() + PARENT_EXIT_TIMEOUT;
        while unsafe { libc::getppid() } == self.parent {
            if Instant::now() > deadline {
                tracing::warn!("old ftmd (pid {}) has not exited; restoring anyway", self.parent);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Fingerprint {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let m = std::fs::metadata(path).ok()?;
    Some(Fingerprint {
        dev: m.dev(),
        ino: m.ino(),
        size: m.size(),
        mtime: (m.mtime(), m.mtime_nsec()),
    })
}

/// A replacement binary that answered the handshake with our frame version.
pub struct Candidate {
    pub exe: PathBuf,
    pub version: String,
}

/// Polls the path ftmd was started from (resolved now: the file is swapped underneath it later) and sends each
/// compatible replacement to the accept loop. An incompatible one is reported and not tried again until it changes.
pub fn watch(shared: Shared) -> Option<mpsc::Receiver<Candidate>> {
    let exe = std::env::current_exe().ok()?;
    let mut baseline = fingerprint(&exe)?;
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut seen: Option<(Fingerprint, Instant)> = None;
        loop {
            tokio::time::sleep(POLL).await;
            let now = fingerprint(&exe);
            let Some(now) = now.filter(|f| *f != baseline) else {
                seen = None;
                continue;
            };
            match seen {
                Some((f, since)) if f == now && since.elapsed() >= SETTLE => {},
                Some((f, _)) if f == now => continue,
                _ => {
                    seen = Some((now, Instant::now()));
                    continue;
                },
            }
            seen = None;
            baseline = now;
            tracing::info!("{} was replaced; checking the new build", exe.display());
            let probed = {
                let exe = exe.clone();
                tokio::task::spawn_blocking(move || probe(&exe)).await
            };
            match probed.map_err(eyre::Report::from).and_then(|p| p) {
                Ok(Handshake {
                    version,
                    channel: Some(channel),
                    ..
                }) if channel != paths::CHANNEL => {
                    let reason = format!(
                        "the new ftmd is a {channel} build and this one {}: it runs its own daemon, and this one keeps \
                         its panes until it exits",
                        paths::CHANNEL
                    );
                    tracing::warn!("not restarting onto ftmd {version}: {reason}");
                    state::update_blocked(&shared, Some(version), reason);
                },
                Ok(h) if h.frame == frame::VERSION => {
                    let candidate = Candidate {
                        exe: exe.clone(),
                        version: h.version,
                    };
                    if tx.send(candidate).await.is_err() {
                        return;
                    }
                },
                Ok(h) => {
                    let reason = format!(
                        "the new ftmd speaks holder protocol {}, the running one {}: moving onto it would close every pane",
                        h.frame,
                        frame::VERSION
                    );
                    tracing::warn!("not restarting onto ftmd {}: {reason}", h.version);
                    state::update_blocked(&shared, Some(h.version), reason);
                },
                Err(e) => {
                    let reason = format!("the new ftmd did not answer {HANDSHAKE}: {e:#}");
                    tracing::warn!("not restarting: {reason}");
                    state::update_blocked(&shared, None, reason);
                },
            }
        }
    });
    Some(rx)
}

fn probe(exe: &Path) -> Result<Handshake> {
    let mut child = Command::new(exe)
        .arg(HANDSHAKE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .wrap_err("starting it")?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("no answer within {PROBE_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut out = String::new();
    child.stdout.take().expect("piped").read_to_string(&mut out)?;
    if !status.success() {
        bail!("it exited with {status}");
    }
    let line = out.lines().last().unwrap_or_default();
    serde_json::from_str(line).wrap_err_with(|| format!("unexpected answer {line:?}"))
}

/// Starts `exe` with the listener and waits for it to say it is ready, then exits, still holding the state lock so
/// no pane reader can save over the tree the new ftmd is restoring. Returns only when the new ftmd did not come up,
/// with the lock released and the old ftmd still whole.
pub fn hand_over(exe: &Path, listener: RawFd, shared: &Shared) -> Result<Infallible> {
    let frozen = state::freeze(shared)?;
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).wrap_err("creating the readiness pipe");
    }
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&read, &write] {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let write_fd = write.as_raw_fd();
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args_os().skip(1))
        .env(LISTEN_FD, listener.to_string())
        .env(READY_FD, write_fd.to_string())
        .stdin(Stdio::null());
    // Its own session, like the holders: it must outlive whatever started this ftmd. Only the two handed-over fds
    // cross the exec; everything else ftmd opens is close-on-exec.
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            for fd in [listener, write_fd] {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().wrap_err_with(|| format!("starting {}", exe.display()))?;
    drop(write);
    let failure = match wait_ready(&read, &mut child) {
        Ok(()) => {
            tracing::info!("handed over to ftmd pid {}", child.id());
            std::process::exit(0);
        },
        Err(e) => e,
    };
    let _ = child.kill();
    let _ = child.wait();
    drop(frozen);
    Err(failure)
}

fn wait_ready(read: &OwnedFd, child: &mut std::process::Child) -> Result<()> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("the new ftmd was not ready within {READY_TIMEOUT:?}");
        }
        let mut pfd = libc::pollfd {
            fd: read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, left.as_millis().min(i32::MAX as u128) as i32) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if n == 0 {
            continue;
        }
        let mut byte = [0u8; 1];
        let got = unsafe { libc::read(read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
        if got == 1 {
            return Ok(());
        }
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                return Err(eyre!("the new ftmd exited before it was ready ({status})"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        bail!("the new ftmd closed the readiness pipe without signalling");
    }
}
