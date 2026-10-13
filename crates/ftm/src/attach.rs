//! `ftm attach`: this terminal becomes a view of one pane until Ctrl+] then d, or until the pane exits.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eyre::{Result, WrapErr, bail};
use ftm_core::model::PaneId;
use ftm_core::paths;
use ftm_core::protocol::{Event, Request, Response};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Ctrl+]. Followed by `d` it detaches; pressed twice it sends one to the pane.
const ESCAPE: u8 = 0x1d;

static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}

struct Conn {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Conn {
    fn open() -> Result<Self> {
        let socket = paths::socket_path();
        let write = UnixStream::connect(&socket)
            .wrap_err_with(|| format!("connecting to ftmd at {} (is it running?)", socket.display()))?;
        let read = BufReader::new(write.try_clone()?);
        Ok(Conn { write, read })
    }

    fn send(&mut self, request: &Request) -> Result<Response> {
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        self.write.write_all(&line)?;
        let mut reply = String::new();
        self.read.read_line(&mut reply)?;
        match serde_json::from_str(&reply)? {
            Response::Error { message } => bail!(message),
            response => Ok(response),
        }
    }
}

/// Puts stdin in raw mode and restores it on drop, however attach ends.
struct RawMode(libc::termios);

impl RawMode {
    fn enable() -> Result<Self> {
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut original) != 0 {
                bail!("stdin is not a terminal");
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
            Ok(RawMode(original))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        (libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0).then_some((ws.ws_row, ws.ws_col))
    }
}

pub fn run(pane: PaneId) -> Result<()> {
    let mut events = Conn::open()?;
    events.send(&Request::Attach { pane })?;
    let mut requests = Conn::open()?;
    let resize = |requests: &mut Conn| -> Result<()> {
        if let Some((rows, cols)) = terminal_size() {
            requests.send(&Request::Resize { pane, cols, rows })?;
        }
        Ok(())
    };

    let raw = RawMode::enable()?;
    unsafe { libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t) };
    resize(&mut requests)?;

    // Pane output goes straight to this terminal; the stream ending means the pane exited.
    let output = std::thread::spawn(move || {
        let mut stdout = std::io::stdout();
        let mut line = String::new();
        loop {
            line.clear();
            if !matches!(events.read.read_line(&mut line), Ok(n) if n > 0) {
                return;
            }
            if let Ok(Event::Output { data, .. }) = serde_json::from_str(&line)
                && let Ok(bytes) = B64.decode(data)
            {
                let _ = stdout.write_all(&bytes).and_then(|()| stdout.flush());
            }
        }
    });

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });

    let mut escaped = false;
    let detached = loop {
        if output.is_finished() {
            break false;
        }
        if WINCH.swap(false, Ordering::Relaxed) {
            resize(&mut requests)?;
        }
        let Ok(bytes) = rx.recv_timeout(Duration::from_millis(50)) else {
            continue;
        };
        let mut send = Vec::with_capacity(bytes.len());
        let mut detach = false;
        for b in bytes {
            match (escaped, b) {
                (true, b'd') => detach = true,
                (true, ESCAPE) => send.push(ESCAPE),
                (true, other) => send.extend([ESCAPE, other]),
                (false, ESCAPE) => {
                    escaped = true;
                    continue;
                },
                (false, other) => send.push(other),
            }
            escaped = false;
        }
        if !send.is_empty() {
            requests.send(&Request::Input {
                pane,
                data: B64.encode(&send),
            })?;
        }
        if detach {
            break true;
        }
    };
    drop(raw);
    println!("\r\n[{} pane {pane}]", if detached { "detached from" } else { "exited:" });
    std::process::exit(0);
}
