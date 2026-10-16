//! A pane holder driven directly over its socket, without an ftmd.

mod common;

use common::{alive, temp_root, wait_for};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

const OUTPUT: u8 = 2;
const KILL: u8 = 5;

fn start_holder(socket: &Path, cwd: &Path, script: &str) -> (Child, UnixStream) {
    let _ = std::fs::remove_file(socket);
    let child = Command::new(env!("CARGO_BIN_EXE_ftmd"))
        .arg("pty-holder")
        .arg(socket)
        .args(["24", "80"])
        .arg(cwd)
        .args(["/bin/sh", "-c", script])
        .spawn()
        .unwrap();
    let mut conn = None;
    wait_for("the holder's socket", Duration::from_secs(10), || {
        conn = UnixStream::connect(socket).ok();
        conn.is_some()
    });
    (child, conn.unwrap())
}

fn read_frame(conn: &mut UnixStream) -> (u8, Vec<u8>) {
    let mut head = [0u8; 5];
    conn.read_exact(&mut head).unwrap();
    let mut payload = vec![0u8; u32::from_be_bytes(head[1..].try_into().unwrap()) as usize];
    conn.read_exact(&mut payload).unwrap();
    (head[0], payload)
}

fn read_pid(path: &Path) -> i32 {
    let mut pid = None;
    wait_for("the background job's pid", Duration::from_secs(10), || {
        pid = std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok());
        pid.is_some()
    });
    pid.unwrap()
}

/// SIGKILLs what a failed test would otherwise leave running.
struct Reap(Vec<i32>);

impl Drop for Reap {
    fn drop(&mut self) {
        for &pid in &self.0 {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn socket_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/ftmd-{}-{name}.sock", std::process::id()))
}

/// Once the shell has died of the hangup, a job that ignores SIGHUP still holds the terminal open: Kill must SIGKILL
/// it and the holder must exit rather than wait for an end of stream that never comes.
#[test]
fn kill_leaves_nothing_behind_once_the_shell_is_gone() {
    let dir = temp_root("holder-kill");
    let socket = socket_path("holder-kill");
    let pidfile = dir.join("job");
    let script = format!("(trap '' HUP; exec sleep 300) & echo $! > {}; sleep 300", pidfile.display());
    let (mut holder, mut conn) = start_holder(&socket, &dir, &script);
    assert_eq!(read_frame(&mut conn).0, 1);
    assert_eq!(read_frame(&mut conn).0, OUTPUT);
    let job = read_pid(&pidfile);
    let _reap = Reap(vec![holder.id().cast_signed(), job]);

    conn.write_all(&[KILL, 0, 0, 0, 0]).unwrap();
    wait_for("the holder to exit", Duration::from_secs(10), || {
        holder.try_wait().unwrap().is_some()
    });
    wait_for("the background job to die", Duration::from_secs(5), || !alive(job));
    assert!(!socket.exists());
}
