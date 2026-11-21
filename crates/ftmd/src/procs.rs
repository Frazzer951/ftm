//! Reading the process table: from `/proc` on Linux, from libproc and sysctl on macOS. Elsewhere it reads as empty.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// A process that has not exited (zombies are left out).
#[derive(Debug)]
pub struct Proc {
    pub pid: libc::pid_t,
    pub ppid: libc::pid_t,
    pub pgrp: libc::pid_t,
    pub sid: libc::pid_t,
    pub comm: String,
}

#[cfg(target_os = "linux")]
pub fn list() -> Vec<Proc> {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    procs
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            // pid (comm) state ppid pgrp session ...; comm may contain anything, including ") ".
            let (head, rest) = stat.rsplit_once(')')?;
            let comm = head.split_once('(')?.1.to_string();
            let mut fields = rest.split_whitespace();
            if fields.next()? == "Z" {
                return None;
            }
            let mut next = || fields.next()?.parse::<libc::pid_t>().ok();
            Some(Proc {
                pid,
                ppid: next()?,
                pgrp: next()?,
                sid: next()?,
                comm,
            })
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub fn list() -> Vec<Proc> {
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
    pids.into_iter()
        .filter(|&pid| pid > 0)
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
            if got != info_bytes || info.pbi_status == libc::SZOMB {
                return None;
            }
            // SAFETY: getsid only reads the process table.
            let sid = unsafe { libc::getsid(pid) };
            let comm: Vec<u8> = info
                .pbi_comm
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c.cast_unsigned())
                .collect();
            Some(Proc {
                pid,
                ppid: libc::pid_t::try_from(info.pbi_ppid).ok()?,
                pgrp: libc::pid_t::try_from(info.pbi_pgid).ok()?,
                sid,
                comm: String::from_utf8_lossy(&comm).into_owned(),
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn list() -> Vec<Proc> {
    Vec::new()
}

/// The first arguments of `pid`, as many as `want`.
#[cfg(target_os = "linux")]
fn argv(pid: libc::pid_t, want: usize) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|&b| b == 0)
        .take(want)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The first arguments of `pid`, as many as `want`.
#[cfg(target_os = "macos")]
fn argv(pid: libc::pid_t, want: usize) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: with a null buffer, sysctl only writes the size it needs to `size`.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || size == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` holds `size` writable bytes and sysctl writes at most that many, updating `size`.
    let got = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if got != 0 {
        return Vec::new();
    }
    buf.truncate(size);
    // argc, the executable's path, NUL padding, then argv.
    let Some((argc, rest)) = buf.split_first_chunk::<4>() else {
        return Vec::new();
    };
    let argc = usize::try_from(i32::from_ne_bytes(*argc)).unwrap_or(0);
    let Some(path_end) = rest.iter().position(|&b| b == 0) else {
        return Vec::new();
    };
    let rest = &rest[path_end..];
    let start = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
    rest[start..]
        .split(|&b| b == 0)
        .take(argc.min(want))
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn argv(_pid: libc::pid_t, _want: usize) -> Vec<String> {
    Vec::new()
}

/// The pid of the process at the other end of a connected Unix socket: for a socket we connected, the one that listens.
pub fn peer_pid(stream: &UnixStream) -> Option<libc::pid_t> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: ucred is plain integers, for which all zeroes is a valid value.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = libc::socklen_t::try_from(size_of::<libc::ucred>()).ok()?;
        // SAFETY: `cred` is a writable ucred and `len` its size, which is what SO_PEERCRED writes.
        let got = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                std::ptr::from_mut(&mut cred).cast(),
                &raw mut len,
            )
        };
        (got == 0 && cred.pid > 0).then_some(cred.pid)
    }
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut len = libc::socklen_t::try_from(size_of::<libc::pid_t>()).ok()?;
        // SAFETY: `pid` is a writable pid_t and `len` its size, which is what LOCAL_PEERPID writes.
        let got = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                std::ptr::from_mut(&mut pid).cast(),
                &raw mut len,
            )
        };
        (got == 0 && pid > 0).then_some(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = stream.as_raw_fd();
        None
    }
}

fn named_claude(arg: &str) -> bool {
    Path::new(arg).file_name().is_some_and(|n| n == "claude")
}

/// Whether Claude Code runs in the pane whose holder is `holder`, i.e. in the session of the shell the holder started.
/// A native install shows up as `claude`; an npm one may be `node` running a `claude` script.
pub fn claude_in_pane(holder: libc::pid_t) -> bool {
    let procs = list();
    let shells: Vec<libc::pid_t> = procs
        .iter()
        .filter(|p| p.ppid == holder && p.sid == p.pid)
        .map(|p| p.pid)
        .collect();
    claude_in_sessions(&procs, &shells)
}

fn claude_in_sessions(procs: &[Proc], sessions: &[libc::pid_t]) -> bool {
    procs
        .iter()
        .filter(|p| sessions.contains(&p.sid))
        .any(|p| p.comm == "claude" || argv(p.pid, 2).iter().any(|a| named_claude(a)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Tests that run a claude under this process, which would show up in each other's checks.
    static CLAUDES: Mutex<()> = Mutex::new(());

    fn this() -> libc::pid_t {
        // SAFETY: getpid takes no arguments and cannot fail.
        unsafe { libc::getpid() }
    }

    /// `program args` as a child of this process that leads its own session, as a pane's shell does under its holder.
    fn shell(program: &Path, args: &[&str]) -> Child {
        let mut cmd = Command::new(program);
        cmd.args(args);
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

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ftmd-procs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sleep_bin() -> std::path::PathBuf {
        ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.exists())
            .unwrap()
    }

    /// A copy of bash, which runs under any name: multi-call coreutils (uutils) refuse to run as `claude`, and macOS's
    /// /bin/sh only starts another shell.
    fn fake_claude(dir: &Path) -> std::path::PathBuf {
        let claude = dir.join("claude");
        std::fs::copy("/bin/bash", &claude).unwrap();
        claude
    }

    fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn lists_this_process() {
        let me = list().into_iter().find(|p| p.pid == this()).expect("this process is listed");
        // SAFETY: getppid and getsid take no pointers.
        assert_eq!((me.ppid, me.sid), (unsafe { libc::getppid() }, unsafe { libc::getsid(0) }));
    }

    #[test]
    fn claude_counts_only_while_it_runs_in_a_shell_of_ours() {
        let _serial = CLAUDES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("native");
        let claude = fake_claude(&dir);
        // The shell runs claude as a child, like `claude --resume` typed at a prompt.
        let script = format!("'{}' -c 'sleep 30; :'; exit 0", claude.display());
        let mut child = shell(Path::new("/bin/sh"), &["-c", &script]);
        wait_until("claude to start", || claude_in_pane(this()));
        assert!(!claude_in_pane(child.id().cast_signed()), "only the holder's shells count");
        // SAFETY: kill takes no pointers; the group is our unreaped child's session.
        unsafe { libc::kill(-child.id().cast_signed(), libc::SIGKILL) };
        child.wait().unwrap();
        wait_until("claude to be gone", || !claude_in_pane(this()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_claude_script_counts_through_its_arguments() {
        let _serial = CLAUDES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("script");
        let claude = dir.join("claude");
        std::fs::write(&claude, "sleep 30; exit 0\n").unwrap();
        // The interpreter's name is sh, but claude is its first argument, as for `node .../bin/claude`.
        let mut child = shell(Path::new("/bin/sh"), &[claude.to_str().unwrap()]);
        let pid = child.id().cast_signed();
        wait_until("sh to run claude", || claude_in_pane(this()));
        assert!(list().iter().all(|p| p.pid != pid || p.comm != "claude"));
        // SAFETY: kill takes no pointers; the group is our unreaped child's session.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        child.wait().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_shell_without_claude_does_not_count() {
        let mut child = shell(&sleep_bin(), &["30"]);
        let pid = child.id().cast_signed();
        wait_until("the shell to start", || list().iter().any(|p| p.pid == pid && p.sid == pid));
        // Other tests may have their own claude running under this process, so only this shell's session is checked.
        assert!(!claude_in_sessions(&list(), &[pid]));
        // SAFETY: kill takes no pointers; `pid` is our unreaped child.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        child.wait().unwrap();
    }

    #[test]
    fn peer_pid_names_the_listener() {
        let dir = scratch("peer");
        let path = dir.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let stream = UnixStream::connect(&path).unwrap();
        let _accepted = listener.accept().unwrap();
        assert_eq!(peer_pid(&stream), Some(this()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
