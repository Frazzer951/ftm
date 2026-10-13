//! `ftmd pty-holder <socket> <rows> <cols> <cwd> <program> [args...]`: owns one pane's PTY and process tree so they
//! outlive ftmd (dtach-style). It serves one ftmd at a time on its own socket: every new connection gets `Hello`, the
//! buffered output, then live output, and replaces the previous connection. It exits once the terminal's processes
//! are gone, removing its socket.

use crate::frame::{self, Frame};
use eyre::{Result, WrapErr, bail, eyre};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const BUFFER_CAP: usize = 1 << 20;
const BUFFER_SLACK: usize = 256 << 10;

struct Shared {
    /// A `Kill` in progress. The terminal hits end of stream as soon as the shell dies (macOS revokes it), while a
    /// SIGHUP-ignoring job may still need its SIGKILL, so exiting waits for this.
    killing: Option<std::thread::JoinHandle<()>>,
    buffer: Vec<u8>,
    client: Option<UnixStream>,
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
    let socket = PathBuf::from(socket);
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
    let reader = pair.master.try_clone_reader().map_err(|e| eyre!("{e}"))?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(|e| eyre!("{e}"))?));
    let master = Arc::new(Mutex::new(pair.master));
    let child = Arc::new(Mutex::new(child));
    let shared = Arc::new(Mutex::new(Shared {
        killing: None,
        buffer: Vec::new(),
        client: None,
        rows,
        cols,
    }));

    {
        let (shared, child, socket) = (shared.clone(), child.clone(), socket.clone());
        std::thread::spawn(move || pump_output(reader, &shared, &child, &socket));
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
            if let Some(old) = s.client.replace(out) {
                let _ = old.shutdown(std::net::Shutdown::Both);
            }
        }
        let (shared, writer, master, child) = (shared.clone(), writer.clone(), master.clone(), child.clone());
        std::thread::spawn(move || serve(conn, &shared, &writer, &master, &child));
    }
    Ok(())
}

/// Copies PTY output into the buffer and to the current client, under one lock so a new client's replay and the
/// live stream neither overlap nor gap. Draining continues while processes are being killed, so none of them block on
/// a full terminal. At end of stream every process on the terminal is gone: reap and exit.
fn pump_output(
    mut reader: Box<dyn Read + Send>,
    shared: &Mutex<Shared>,
    child: &Mutex<Box<dyn Child + Send + Sync>>,
    socket: &PathBuf,
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
        if let Some(client) = s.client.as_mut()
            && frame::write(client, &Frame::Output(buf[..n].to_vec())).is_err()
        {
            s.client = None;
        }
    }
    let killing = lock(shared).killing.take();
    if let Some(killing) = killing {
        let _ = killing.join();
    }
    let _ = std::fs::remove_file(socket);
    let _ = lock(child).wait();
    std::process::exit(0);
}

fn serve(
    mut conn: UnixStream,
    shared: &Mutex<Shared>,
    writer: &Mutex<Box<dyn Write + Send>>,
    master: &Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: &Arc<Mutex<Box<dyn Child + Send + Sync>>>,
) {
    while let Ok(Some(frame)) = frame::read(&mut conn) {
        match frame {
            Frame::Input(bytes) => {
                let mut w = lock(writer);
                let _ = w.write_all(&bytes).and_then(|()| w.flush());
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
                    let (master, child) = (master.clone(), child.clone());
                    s.killing = Some(std::thread::spawn(move || terminate(&master, &child)));
                }
            },
            Frame::Hello { .. } | Frame::Output(_) => {},
        }
    }
}

/// Hangs up the shell's process group and the terminal's foreground group, then SIGKILLs whatever is left. An agent
/// started by an interactive shell runs in its own group and Claude Code ignores SIGHUP, so signalling the shell alone
/// leaves it running.
fn terminate(master: &Mutex<Box<dyn MasterPty + Send>>, child: &Mutex<Box<dyn Child + Send + Sync>>) {
    let mut groups: Vec<libc::pid_t> = lock(child).process_id().map(|pid| pid as libc::pid_t).into_iter().collect();
    if let Some(fg) = lock(master).process_group_leader()
        && fg > 0
        && !groups.contains(&fg)
    {
        groups.push(fg);
    }
    signal_groups(&groups, libc::SIGHUP);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = lock(child).try_wait();
        if !groups.iter().any(|&g| unsafe { libc::kill(-g, 0) } == 0) {
            break;
        }
        if Instant::now() > deadline {
            signal_groups(&groups, libc::SIGKILL);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn signal_groups(groups: &[libc::pid_t], signal: libc::c_int) {
    for &group in groups {
        unsafe { libc::kill(-group, signal) };
    }
}
