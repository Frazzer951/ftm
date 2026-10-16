//! `ftm attach`: this terminal becomes a view of one pane until Ctrl+] then d, or until the pane exits.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eyre::{Result, WrapErr, bail};
use ftm_core::model::PaneId;
use ftm_core::paths;
use ftm_core::protocol::{Event, Request, Response};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
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
        if reply.is_empty() {
            bail!("ftmd closed the connection without replying");
        }
        match serde_json::from_str(&reply)? {
            Response::Error { message } => bail!(message),
            response => Ok(response),
        }
    }
}

/// The terminal settings to put back, for the signal handlers.
static ORIGINAL: OnceLock<libc::termios> = OnceLock::new();

const FATAL_SIGNALS: [libc::c_int; 4] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT];

extern "C" fn on_fatal(sig: libc::c_int) {
    // SAFETY: tcsetattr, signal and raise are async-signal-safe, and ORIGINAL.get is an atomic load. ORIGINAL is set
    // before this handler is installed, to the termios tcgetattr filled.
    unsafe {
        if let Some(original) = ORIGINAL.get() {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original);
        }
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Puts stdin in raw mode and restores it on drop, however attach ends, or on a signal that kills it.
struct RawMode(libc::termios);

impl RawMode {
    fn enable() -> Result<Self> {
        // SAFETY: termios is plain C data, so all-zero bytes are a valid value. Each call gets a pointer to a live
        // local, and tcgetattr has filled `original` before it is stored or copied. on_fatal only makes
        // async-signal-safe calls.
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &raw mut original) != 0 {
                bail!("stdin is not a terminal");
            }
            let _ = ORIGINAL.set(original);
            for sig in FATAL_SIGNALS {
                libc::signal(sig, on_fatal as *const () as libc::sighandler_t);
            }
            let mut raw = original;
            libc::cfmakeraw(&raw mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const raw) != 0 {
                bail!("putting the terminal in raw mode: {}", std::io::Error::last_os_error());
            }
            Ok(RawMode(original))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the termios that tcgetattr filled in enable().
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const self.0) };
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    // SAFETY: winsize is plain C data, so all-zero bytes are a valid value; TIOCGWINSZ writes one winsize to the live
    // `ws`.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        (libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0)
            .then_some(ws)
            .and_then(usable_size)
    }
}

/// Rows and columns, unless the terminal reports no size.
fn usable_size(ws: libc::winsize) -> Option<(u16, u16)> {
    (ws.ws_row > 0 && ws.ws_col > 0).then_some((ws.ws_row, ws.ws_col))
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
    // SAFETY: on_winch only stores to an atomic, which is async-signal-safe.
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
        let bytes = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(bytes) => bytes,
            Err(RecvTimeoutError::Timeout) => continue,
            // Stdin is gone, so nothing can be typed or detach this view.
            Err(RecvTimeoutError::Disconnected) => break true,
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
    // Stdout may be gone with stdin.
    let _ = writeln!(
        std::io::stdout(),
        "\r\n[{} pane {pane}]",
        if detached { "detached from" } else { "exited:" }
    );
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_without_a_size_is_not_sent() {
        let size = |rows, cols| libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(usable_size(size(24, 80)), Some((24, 80)));
        assert_eq!(usable_size(size(0, 80)), None);
        assert_eq!(usable_size(size(24, 0)), None);
    }
}
