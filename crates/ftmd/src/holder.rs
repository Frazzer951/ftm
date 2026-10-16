//! `ftmd pty-holder <socket> <rows> <cols> <cwd> <program> [args...]`: owns one pane's PTY and process tree so they
//! outlive ftmd (dtach-style). It serves one ftmd at a time on its own socket: every new connection gets `Hello`, the
//! buffered output, then live output, and replaces the previous connection. It exits once the terminal's processes
//! are gone, removing its socket, but not before some ftmd has had the output: a pane that dies at once still shows
//! why.

use crate::frame::{self, Frame};
use eyre::{Result, WrapErr, bail, eyre};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const BUFFER_CAP: usize = 1 << 20;
const BUFFER_SLACK: usize = 256 << 10;
/// How long processes get to exit after SIGHUP, and then the terminal to reach end of stream after SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// How long a holder whose processes are gone waits for its first ftmd, which is still connecting when a pane's
/// command fails at once. Matches ftmd's own wait for a new holder.
const ATTACH_GRACE: Duration = Duration::from_secs(5);

struct Shared {
    /// A `Kill` in progress. The terminal hits end of stream as soon as the shell dies (macOS revokes it), while a
    /// SIGHUP-ignoring job may still need its SIGKILL, so exiting waits for this.
    killing: Option<std::thread::JoinHandle<()>>,
    buffer: Vec<u8>,
    client: Option<UnixStream>,
    /// Some ftmd has been sent the buffer.
    attached: bool,
    /// The terminal reached end of stream.
    eof: bool,
    rows: u16,
    cols: u16,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn run(args: &[String]) -> Result<()> {
    let [socket, rows, cols, cwd, program, rest @ ..] = args else {
        bail!("usage: ftmd pty-holder <socket> <rows> <cols> <cwd> <program> [args...]");
    };
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let socket = PathBuf::from(socket);
    let pane: Arc<str> = std::env::var(ftm_core::ENV_PANE_ID)
        .unwrap_or_else(|_| socket.file_stem().unwrap_or_default().to_string_lossy().into_owned())
        .into();
    let (rows, cols): (u16, u16) = (rows.parse()?, cols.parse()?);
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).wrap_err_with(|| format!("binding {}", socket.display()))?;

    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| eyre!("opening pty: {e}"))?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(rest);
    cmd.cwd(cwd);
    let child = pair.slave.spawn_command(cmd).map_err(|e| eyre!("starting {program}: {e}"))?;
    drop(pair.slave);
    tracing::info!("pane {pane}: started {program} as pid {:?}", child.process_id());
    let reader = pair.master.try_clone_reader().map_err(|e| eyre!("{e}"))?;
    let mut writer = pair.master.take_writer().map_err(|e| eyre!("{e}"))?;
    // A terminal whose program is not reading fills up and blocks writes, so they happen here and the connection keeps
    // being read: a `Kill` or `Resize` behind a big paste still lands.
    let (input, queued) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for bytes in queued {
            if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                return;
            }
        }
    });
    let master = Arc::new(Mutex::new(pair.master));
    let child = Arc::new(Mutex::new(child));
    let shared = Arc::new(Mutex::new(Shared {
        killing: None,
        buffer: Vec::new(),
        client: None,
        attached: false,
        eof: false,
        rows,
        cols,
    }));

    {
        let (pane, shared, child, socket) = (pane.clone(), shared.clone(), child.clone(), socket.clone());
        std::thread::spawn(move || pump_output(&pane, reader, &shared, &child, &socket));
    }

    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let _ = conn.set_write_timeout(Some(Duration::from_secs(5)));
        let Ok(mut out) = conn.try_clone() else { continue };
        {
            let mut s = lock(&shared);
            let hello = Frame::Hello {
                version: frame::VERSION,
                rows: s.rows,
                cols: s.cols,
            };
            if frame::write(&mut out, &hello).is_err() || frame::write(&mut out, &Frame::Output(s.buffer.clone())).is_err() {
                continue;
            }
            s.attached = true;
            if let Some(old) = s.client.replace(out) {
                let _ = old.shutdown(std::net::Shutdown::Both);
            }
        }
        let (pane, shared, input, master, child, socket) = (
            pane.clone(),
            shared.clone(),
            input.clone(),
            master.clone(),
            child.clone(),
            socket.clone(),
        );
        std::thread::spawn(move || serve(&pane, conn, &shared, &input, &master, &child, &socket));
    }
    Ok(())
}

/// Copies PTY output into the buffer and to the current client, under one lock so a new client's replay and the
/// live stream neither overlap nor gap. Draining continues while processes are being killed, so none of them block on
/// a full terminal. At end of stream every process on the terminal is gone: reap and exit, once an ftmd has the output.
fn pump_output(
    pane: &str,
    mut reader: Box<dyn Read + Send>,
    shared: &Mutex<Shared>,
    child: &Mutex<Box<dyn Child + Send + Sync>>,
    socket: &Path,
) {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let mut s = lock(shared);
        s.buffer.extend_from_slice(&buf[..n]);
        if s.buffer.len() > BUFFER_CAP + BUFFER_SLACK {
            let excess = s.buffer.len() - BUFFER_CAP;
            s.buffer.drain(..excess);
        }
        // An ftmd that stopped reading may hold half a frame: hang up on it entirely, so it reconnects for the buffer.
        if let Some(client) = s.client.as_mut()
            && frame::write(client, &Frame::Output(buf[..n].to_vec())).is_err()
            && let Some(client) = s.client.take()
        {
            let _ = client.shutdown(std::net::Shutdown::Both);
        }
    }
    lock(shared).eof = true;
    let deadline = Instant::now() + ATTACH_GRACE;
    while !lock(shared).attached && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let killing = lock(shared).killing.take();
    if let Some(killing) = killing {
        let _ = killing.join();
    }
    exit(pane, child, socket)
}

fn exit(pane: &str, child: &Mutex<Box<dyn Child + Send + Sync>>, socket: &Path) -> ! {
    let _ = std::fs::remove_file(socket);
    match lock(child).wait() {
        Ok(status) => match status.signal() {
            Some(signal) => tracing::info!("pane {pane}: shell exited on {signal}"),
            None => tracing::info!("pane {pane}: shell exited with status {}", status.exit_code()),
        },
        Err(e) => tracing::warn!("pane {pane}: reaping the shell: {e}"),
    }
    std::process::exit(0);
}

fn serve(
    pane: &str,
    mut conn: UnixStream,
    shared: &Arc<Mutex<Shared>>,
    input: &Sender<Vec<u8>>,
    master: &Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: &Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    socket: &Path,
) {
    while let Ok(Some(frame)) = frame::read(&mut conn) {
        match frame {
            Frame::Input(bytes) => {
                let _ = input.send(bytes);
            },
            Frame::Resize { rows, cols } => {
                let mut s = lock(shared);
                (s.rows, s.cols) = (rows, cols);
                let _ = lock(master).resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            },
            Frame::Kill => {
                let mut s = lock(shared);
                if s.killing.is_none() {
                    let (pane, shared, master, child, socket) = (
                        pane.to_string(),
                        shared.clone(),
                        master.clone(),
                        child.clone(),
                        socket.to_path_buf(),
                    );
                    s.killing = Some(std::thread::spawn(move || {
                        terminate(&pane, &master, &child);
                        // A process that left the session can still hold the terminal open.
                        let deadline = Instant::now() + KILL_GRACE;
                        while !lock(&shared).eof {
                            if Instant::now() > deadline {
                                tracing::warn!("pane {pane}: terminal still open after SIGKILL; exiting anyway");
                                exit(&pane, &child, &socket);
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    }));
                }
            },
            Frame::Hello { .. } | Frame::Output(_) => {},
        }
    }
}

/// Hangs up the shell's process group and the terminal's foreground group, then SIGKILLs whatever is left. An agent
/// started by an interactive shell runs in its own group and Claude Code ignores SIGHUP, so signalling the shell alone
/// leaves it running; a job that ignores SIGHUP outlives the shell. A group id is free for reuse once its processes
/// are gone, so the shell stays unreaped until this is done and other groups are only signalled while they are still
/// in the pane's session.
fn terminate(pane: &str, master: &Mutex<Box<dyn MasterPty + Send>>, child: &Mutex<Box<dyn Child + Send + Sync>>) {
    let Some(shell) = lock(child).process_id().map(u32::cast_signed) else {
        return;
    };
    let fg = lock(master).process_group_leader().filter(|&g| g > 0 && g != shell);
    tracing::info!("pane {pane}: hanging up shell group {shell} and foreground group {fg:?}");
    signal_groups(shell, fg, libc::SIGHUP);
    let deadline = Instant::now() + KILL_GRACE;
    while running(shell) || fg.is_some_and(|g| in_session(g, shell)) || !session_groups(shell).is_empty() {
        if Instant::now() > deadline {
            tracing::warn!("pane {pane}: still running {KILL_GRACE:?} after SIGHUP; sending SIGKILL");
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The shell's group may still hold a job that ignored the hangup.
    signal_groups(shell, fg, libc::SIGKILL);
}

fn signal_groups(shell: libc::pid_t, fg: Option<libc::pid_t>, signal: libc::c_int) {
    // SAFETY: kill takes no pointers. `shell` is our unreaped child and leads its own process group, so the id is not
    // reused.
    unsafe { libc::kill(-shell, signal) };
    for group in fg.into_iter().chain(session_groups(shell)) {
        if group != shell && in_session(group, shell) {
            // SAFETY: kill takes no pointers; in_session just checked that `group` is still a group in the shell's
            // session.
            unsafe { libc::kill(-group, signal) };
        }
    }
}

/// The process groups of the session `session` leads that still have a live process.
#[cfg(target_os = "linux")]
fn session_groups(session: libc::pid_t) -> Vec<libc::pid_t> {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut groups: Vec<libc::pid_t> = procs
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|e| {
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            // pid (comm) state ppid pgrp session ...; comm may contain anything, including ") ".
            let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
            let state = fields.next()?;
            let (pgrp, sid) = (fields.nth(1)?.parse().ok()?, fields.next()?.parse::<libc::pid_t>().ok()?);
            (sid == session && state != "Z").then_some(pgrp)
        })
        .collect();
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// The process groups of the session `session` leads that still have a live process.
#[cfg(target_os = "macos")]
fn session_groups(session: libc::pid_t) -> Vec<libc::pid_t> {
    let pid_size = size_of::<libc::pid_t>();
    // SAFETY: with no buffer, proc_listallpids only reports how many pids there are.
    let Ok(count) = usize::try_from(unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) }) else {
        return Vec::new();
    };
    // Room for processes started since.
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let Ok(bytes) = libc::c_int::try_from(pids.len() * pid_size) else {
        return Vec::new();
    };
    // SAFETY: the buffer holds `bytes` bytes of pid_t, and proc_listallpids writes at most that many.
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(listed).unwrap_or(0).min(pids.len()));
    let Ok(info_bytes) = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()) else {
        return Vec::new();
    };
    let mut groups: Vec<libc::pid_t> = pids
        .into_iter()
        // SAFETY: getsid only reads the process table.
        .filter(|&pid| pid > 0 && unsafe { libc::getsid(pid) } == session)
        .filter_map(|pid| {
            // SAFETY: proc_bsdinfo is plain integers, for which all zeroes is a valid value.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a proc_bsdinfo, the size PROC_PIDTBSDINFO writes and the size passed.
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    std::ptr::from_mut(&mut info).cast(),
                    info_bytes,
                )
            };
            (got == info_bytes && info.pbi_status != libc::SZOMB)
                .then(|| libc::pid_t::try_from(info.pbi_pgid).ok())
                .flatten()
        })
        .collect();
    groups.sort_unstable();
    groups.dedup();
    groups
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn session_groups(_session: libc::pid_t) -> Vec<libc::pid_t> {
    Vec::new()
}

/// Whether `group` is still a process group of the session `session` leads. A group whose leader has exited but whose
/// members remain keeps its id, so its id cannot have been reused.
fn in_session(group: libc::pid_t, session: libc::pid_t) -> bool {
    // SAFETY: getsid takes no pointers and only reads process state.
    match unsafe { libc::getsid(group) } {
        // SAFETY: kill with signal 0 takes no pointers and delivers nothing; it only checks that the group exists.
        -1 => std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) && unsafe { libc::kill(-group, 0) } == 0,
        sid => sid == session,
    }
}

/// Whether the (unreaped) child `pid` has not exited yet, without reaping it.
fn running(pid: libc::pid_t) -> bool {
    // SAFETY: siginfo_t is plain C data, for which all-zero bytes are a valid value.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: `info` is a live, writable siginfo_t. WNOWAIT leaves the child unreaped, so `pid` keeps naming it.
    if unsafe { libc::waitid(libc::P_PID, pid.cast_unsigned(), &raw mut info, flags) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    // SAFETY: waitid succeeded, so `info` holds either a child's SIGCHLD fields or, under WNOHANG with nothing to
    // report, the zeroed value; si_pid reads a field present in both.
    let exited = unsafe { info.si_pid() };
    #[cfg(not(target_os = "linux"))]
    let exited = info.si_pid;
    exited == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    fn session_leader() -> std::process::Child {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        // SAFETY: between fork and exec the closure only calls setsid, which is async-signal-safe and does not
        // allocate.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().unwrap()
    }

    #[test]
    fn an_exited_child_stops_running_but_stays_unreaped() {
        let mut child = session_leader();
        let pid = child.id().cast_signed();
        assert!(running(pid));
        // SAFETY: kill takes no pointers; `pid` is our unreaped child, so the id is not reused.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while running(pid) {
            assert!(Instant::now() < deadline, "child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_some(), "still there to reap");
    }

    #[test]
    fn groups_count_only_while_in_the_session() {
        let mut child = session_leader();
        let pid = child.id().cast_signed();
        assert!(in_session(pid, pid));
        // SAFETY: getsid takes no pointers; 0 means this process.
        assert!(!in_session(pid, unsafe { libc::getsid(0) }));
        // SAFETY: kill takes no pointers; `pid` is our unreaped child, so the id is not reused.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        child.wait().unwrap();
        assert!(!in_session(pid, pid), "a reaped group's id may be reused");
    }
}
