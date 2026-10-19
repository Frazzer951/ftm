use ftm_core::paths;
use ftm_core::protocol::{Event, Request, Response};
use std::cell::Cell;
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

type Reply = oneshot::Sender<Result<Response, String>>;

/// How long ftmd gets to come up after a launch.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// After an ftmd launch dies at startup, requests fail at once for this long rather than each launching another.
const LAUNCH_BACKOFF: Duration = Duration::from_secs(10);

/// Where ftmd listens and how to start it. The request worker and the event thread share one, so only one of them
/// launches ftmd at a time.
#[derive(Clone)]
pub struct Daemon(Arc<Endpoint>);

struct Endpoint {
    socket: PathBuf,
    binary: PathBuf,
    launches: Mutex<Launches>,
}

#[derive(Default)]
struct Launches {
    /// Launched ftmds not yet reaped. ftmd exits on its own (a failed start, or after handing over to a new build),
    /// and each one left unwaited is a zombie.
    children: Vec<Child>,
    /// The last launch that died before ftmd came up, and why.
    failed: Option<(Instant, String)>,
}

impl Daemon {
    /// ftmd from next to this executable, else from `PATH`.
    pub fn new() -> Self {
        let sibling = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("ftmd")));
        let binary = sibling.filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("ftmd"));
        Self::at(paths::socket_path(), binary)
    }

    fn at(socket: PathBuf, binary: PathBuf) -> Self {
        Daemon(Arc::new(Endpoint {
            socket,
            binary,
            launches: Mutex::default(),
        }))
    }

    fn log_path(&self) -> PathBuf {
        self.0.socket.with_extension("log")
    }

    fn connect(&self) -> Result<UnixStream, String> {
        let socket = &self.0.socket;
        let mut launches = self.0.launches.lock().unwrap_or_else(PoisonError::into_inner);
        launches.children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
        if let Some((at, why)) = &launches.failed
            && at.elapsed() < LAUNCH_BACKOFF
        {
            return Err(why.clone());
        }
        launches.failed = None;
        let mut child = self.launch()?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            let exited = child.try_wait().ok().flatten();
            match UnixStream::connect(socket) {
                Ok(stream) => {
                    if exited.is_none() {
                        launches.children.push(child);
                    }
                    return Ok(stream);
                },
                Err(e) if exited.is_some() || Instant::now() > deadline => {
                    let why = match exited {
                        Some(status) => format!("ftmd exited at startup ({status})"),
                        None => format!("ftmd did not come up on {}: {e}", socket.display()),
                    };
                    let why = match log_tail(&self.log_path()) {
                        Some(tail) => format!("{why}; {}:\n{tail}", self.log_path().display()),
                        None => why,
                    };
                    if exited.is_some() {
                        launches.failed = Some((Instant::now(), why.clone()));
                    } else {
                        launches.children.push(child);
                    }
                    return Err(why);
                },
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Starts ftmd detached in its own process group so it outlives the app.
    fn launch(&self) -> Result<Child, String> {
        let binary = &self.0.binary;
        let log_path = self.log_path();
        if let Some(dir) = log_path.parent() {
            paths::private_dir(dir, paths::socket_dir_is_ours(dir)).map_err(|e| e.to_string())?;
        }
        crate::crash_log::rotate(&log_path, crate::crash_log::MAX_LOG_BYTES);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("opening {}: {e}", log_path.display()))?;
        Command::new(binary)
            .env(ftm_core::ENV_SOCKET, &self.0.socket)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|e| format!("starting {}: {e}", binary.display()))
    }
}

/// The last few lines of ftmd.log, which say why a launch died.
fn log_tail(path: &Path) -> Option<String> {
    const TAIL_BYTES: u64 = 2048;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(5)..].join("\n");
    (!tail.is_empty()).then_some(tail)
}

/// The one request connection lives on a worker thread; requests reach it over a channel, so they hit the daemon in
/// the order they were sent (keystrokes stay ordered) and a slow request never blocks the UI thread.
pub struct Client {
    tx: mpsc::Sender<(Request, Option<Reply>)>,
}

impl Client {
    pub fn start(daemon: Daemon) -> Self {
        let (tx, rx) = mpsc::channel::<(Request, Option<Reply>)>();
        std::thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            for (request, reply) in rx {
                let result = send(&daemon, &mut conn, &request);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                } else if let Err(e) = result {
                    eprintln!("ftmd request failed: {e}");
                }
            }
        });
        Client { tx }
    }

    pub fn fire(&self, request: Request) {
        let _ = self.tx.send((request, None));
    }

    pub async fn request(&self, request: Request) -> Result<Response, String> {
        let (reply, rx) = oneshot::channel();
        self.tx.send((request, Some(reply))).map_err(|e| e.to_string())?;
        rx.await.map_err(|e| e.to_string())?
    }
}

struct Conn {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

enum Failure {
    /// The request never fully reached ftmd.
    Write(std::io::Error),
    /// ftmd may have acted on the request.
    Read(std::io::Error),
}

impl Conn {
    fn open(daemon: &Daemon) -> Result<Self, String> {
        let write = daemon.connect()?;
        let read = BufReader::new(write.try_clone().map_err(|e| e.to_string())?);
        Ok(Conn { write, read })
    }

    /// macOS refuses this with EINVAL once ftmd has closed its end. A read then cannot block anyway: it returns what
    /// ftmd sent before closing, then end of file.
    fn set_read_timeout(&self, timeout: Option<Duration>) {
        let _ = self.write.set_read_timeout(timeout);
    }

    fn roundtrip(&mut self, request: &Request, timeout: Option<Duration>) -> Result<String, Failure> {
        let mut out = serde_json::to_vec(request).map_err(|e| Failure::Write(e.into()))?;
        out.push(b'\n');
        self.set_read_timeout(timeout);
        self.write.write_all(&out).map_err(Failure::Write)?;
        let mut line = String::new();
        match self.read.read_line(&mut line) {
            Ok(0) => Err(Failure::Read(ErrorKind::UnexpectedEof.into())),
            Ok(_) => Ok(line),
            Err(e) => Err(Failure::Read(e)),
        }
    }
}

impl Failure {
    fn into_string(self) -> String {
        match self {
            Failure::Write(e) => e.to_string(),
            Failure::Read(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                "ftmd did not answer in time".into()
            },
            Failure::Read(e) => e.to_string(),
        }
    }
}

/// Sending these twice does no harm, so they are resent when the reply is lost.
fn idempotent(request: &Request) -> bool {
    matches!(
        request,
        Request::Ping
            | Request::List
            | Request::Folders
            | Request::Capture { .. }
            | Request::Resize { .. }
            | Request::SetStatus { .. }
            | Request::Rename { .. }
            | Request::Pin { .. }
            | Request::MarkSeen { .. }
            | Request::RemoveFolder { .. }
            | Request::SetFolderColor { .. }
            | Request::ReorderFolders { .. }
    )
}

/// Reconnects (starting ftmd if needed) and retries once when the connection has dropped: always if the request could
/// not be written, but after a lost reply only if sending it again is harmless.
fn send(daemon: &Daemon, conn: &mut Option<Conn>, request: &Request) -> Result<Response, String> {
    for attempt in 0..2 {
        let open = match conn {
            Some(open) => open,
            None => conn.insert(Conn::open(daemon)?),
        };
        match open.roundtrip(request, Some(request.reply_timeout())) {
            Ok(line) => {
                return match serde_json::from_str(&line).map_err(|e| e.to_string())? {
                    Response::Error { message } => Err(message),
                    response => Ok(response),
                };
            },
            Err(failure) => {
                *conn = None;
                let retry = match &failure {
                    Failure::Write(_) => true,
                    Failure::Read(e) => {
                        idempotent(request) && !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    },
                };
                if attempt == 1 || !retry {
                    return Err(failure.into_string());
                }
            },
        }
    }
    unreachable!()
}

const FAILURE_REPEAT: Duration = Duration::from_secs(30);

/// Forwards daemon events to the frontend. The page starts it once it is listening, and again after a reload, so
/// each start gets a fresh replay; an older stream notices the generation change and stops. Each (re)connect first
/// reports the daemon's version to `connected`: ftmd moves itself onto a new build after an upgrade, which drops the
/// stream, and the app may then be older than the daemon it talks to. When ftmd cannot be reached or started,
/// `failed` hears why, though not the same failure again within `FAILURE_REPEAT`, and not a stream dropped after it
/// connected: that is ftmd moving to a new build, and the reconnect follows.
pub struct Events {
    daemon: Daemon,
    generation: Arc<AtomicU64>,
    /// The live generation's stream, so a restart can end it.
    stream: Arc<Mutex<Option<(u64, UnixStream)>>>,
}

impl Events {
    pub fn new(daemon: Daemon) -> Self {
        Events {
            daemon,
            generation: Arc::default(),
            stream: Arc::default(),
        }
    }

    pub fn restart(
        &self,
        emit: impl Fn(Event) + Send + 'static,
        connected: impl Fn(String) + Send + 'static,
        failed: impl Fn(String) + Send + 'static,
    ) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((_, old)) = self.stream.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = old.shutdown(Shutdown::Both);
        }
        let (daemon, current, slot) = (self.daemon.clone(), self.generation.clone(), self.stream.clone());
        // Runs on the main thread (from the `subscribe` command), where a panic takes the whole app down.
        let spawned = std::thread::Builder::new().name("ftmd-events".into()).spawn(move || {
            let live = || current.load(Ordering::SeqCst) == generation;
            let mut reported: Option<(Instant, String)> = None;
            while live() {
                let reached = Cell::new(false);
                let connected = |version| {
                    reached.set(true);
                    connected(version);
                };
                let result = subscribe(&daemon, generation, &emit, &connected, &slot, &live);
                if reached.get() {
                    reported = None;
                }
                if let Err(e) = result {
                    eprintln!("ftmd event stream: {e}");
                    let repeat = reported
                        .as_ref()
                        .is_some_and(|(at, last)| *last == e && at.elapsed() < FAILURE_REPEAT);
                    if !reached.get() && !repeat && live() {
                        failed(e.clone());
                        reported = Some((Instant::now(), e));
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        if let Err(e) = spawned {
            eprintln!("starting the ftmd event stream: {e}");
        }
    }
}

fn subscribe(
    daemon: &Daemon,
    generation: u64,
    emit: &impl Fn(Event),
    connected: &impl Fn(String),
    slot: &Mutex<Option<(u64, UnixStream)>>,
    live: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut conn = Conn::open(daemon)?;
    {
        // Checked under the lock `restart` takes after bumping the generation, so a stale thread never parks its
        // stream where the restart cannot shut it down.
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if !live() {
            return Ok(());
        }
        *slot = conn.write.try_clone().ok().map(|s| (generation, s));
    }
    let pong = conn
        .roundtrip(&Request::Ping, Some(Request::Ping.reply_timeout()))
        .map_err(Failure::into_string)?;
    if let Ok(Response::Pong { version, .. }) = serde_json::from_str(&pong) {
        connected(version);
    }
    conn.roundtrip(&Request::Subscribe, Some(Request::Ping.reply_timeout()))
        .map_err(Failure::into_string)?;
    conn.set_read_timeout(None);
    let mut line = String::new();
    while live() {
        line.clear();
        if conn.read.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("ftmd closed the connection".into());
        }
        if !live() {
            break;
        }
        match serde_json::from_str::<Event>(&line) {
            // From a newer ftmd; the page would not know it either.
            Ok(Event::Unknown) => {},
            Ok(event) => emit(event),
            Err(e) => eprintln!("bad event from ftmd: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::AtomicUsize;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ftm-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn line<T: serde::Serialize>(value: &T) -> Vec<u8> {
        let mut out = serde_json::to_vec(value).unwrap();
        out.push(b'\n');
        out
    }

    /// A fake ftmd that drops the connection after reading the first request, and answers every later one `Ok`.
    fn loses_first_reply(socket: &Path) -> Arc<AtomicUsize> {
        let listener = UnixListener::bind(socket).unwrap();
        let received = Arc::new(AtomicUsize::new(0));
        let count = received.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut read = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                while read.read_line(&mut request).unwrap_or(0) > 0 {
                    request.clear();
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        break;
                    }
                    stream.write_all(&line(&Response::Ok)).unwrap();
                }
            }
        });
        received
    }

    /// An ftmd that logs why it failed and exits at startup, appending a line to `launched` each time it runs.
    fn dying_ftmd(dir: &Path, launched: &Path) -> Daemon {
        let binary = dir.join("ftmd");
        let script = format!(
            "#!/bin/sh\necho x >> '{}'\necho 'bind failed: boom' >&2\nexit 3\n",
            launched.display()
        );
        // Written by a child: a fork from another test thread would inherit a writable fd held here, and exec refuses
        // a file open for writing (ETXTBSY).
        let mut writer = Command::new("/bin/sh")
            .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
            .arg(&binary)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        writer.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        assert!(writer.wait().unwrap().success());
        Daemon::at(dir.join("ftmd.sock"), binary)
    }

    fn no_ftmd(dir: &Path) -> Daemon {
        Daemon::at(dir.join("ftmd.sock"), dir.join("no-such-ftmd"))
    }

    #[test]
    fn a_lost_reply_is_not_resent_for_input() {
        let dir = scratch("lost-input");
        let daemon = no_ftmd(&dir);
        let received = loses_first_reply(&daemon.0.socket);
        let input =
            serde_json::from_str(r#"{"op":"input","pane":"00000000-0000-0000-0000-000000000001","data":"Cg=="}"#).unwrap();
        assert!(send(&daemon, &mut None, &input).is_err());
        assert_eq!(received.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lost_reply_is_resent_for_an_idempotent_request() {
        let dir = scratch("lost-list");
        let daemon = no_ftmd(&dir);
        let received = loses_first_reply(&daemon.0.socket);
        assert!(matches!(send(&daemon, &mut None, &Request::List), Ok(Response::Ok)));
        assert_eq!(received.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reply_sent_just_before_ftmd_closes_is_read() {
        let dir = scratch("reply-close");
        let daemon = no_ftmd(&dir);
        let listener = UnixListener::bind(&daemon.0.socket).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut request).unwrap();
            stream.write_all(&line(&Response::Ok)).unwrap();
        });
        let input =
            serde_json::from_str(r#"{"op":"input","pane":"00000000-0000-0000-0000-000000000001","data":"Cg=="}"#).unwrap();
        assert!(matches!(send(&daemon, &mut None, &input), Ok(Response::Ok)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hung_ftmd_times_out() {
        let dir = scratch("hung");
        let daemon = no_ftmd(&dir);
        let listener = UnixListener::bind(&daemon.0.socket).unwrap();
        let mut conn = Some(Conn::open(&daemon).unwrap());
        let _server = listener.accept().unwrap();
        let started = Instant::now();
        let failure = conn
            .as_mut()
            .unwrap()
            .roundtrip(&Request::List, Some(Duration::from_millis(100)));
        assert!(matches!(failure, Err(Failure::Read(_))));
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_reconnect_after_ftmd_drops_the_stream() {
        let dir = scratch("events");
        let daemon = no_ftmd(&dir);
        let listener = UnixListener::bind(&daemon.0.socket).unwrap();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let mut stream = stream.unwrap();
                let mut read = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                read.read_line(&mut request).unwrap();
                stream
                    .write_all(&line(&Response::Pong {
                        version: format!("v{n}"),
                        features: Vec::new(),
                    }))
                    .unwrap();
                read.read_line(&mut request).unwrap();
                stream.write_all(&line(&Response::Ok)).unwrap();
                let event = Event::UpdateBlocked {
                    version: None,
                    reason: format!("stream {n}"),
                };
                stream.write_all(b"{\"event\":\"from_the_future\"}\n").unwrap();
                stream.write_all(&line(&event)).unwrap();
                if n > 0 {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
        });
        let events = Events::new(daemon);
        let (tx, rx) = mpsc::channel();
        let versions = tx.clone();
        events.restart(
            move |event| {
                let _ = tx.send(match event {
                    Event::UpdateBlocked { reason, .. } => reason,
                    other => format!("{other:?}"),
                });
            },
            move |version| {
                let _ = versions.send(version);
            },
            |e| panic!("{e}"),
        );
        let got: Vec<String> = (0..4).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(got, ["v0", "stream 0", "v1", "stream 1"]);
        events.restart(|_| {}, |_| {}, |_| {});
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launch_that_dies_fails_fast_and_is_reaped() {
        let dir = scratch("dies");
        let launched = dir.join("launched");
        let daemon = dying_ftmd(&dir, &launched);
        let started = Instant::now();
        let err = daemon.connect().unwrap_err();
        assert!(started.elapsed() < START_TIMEOUT, "{err}");
        assert!(
            err.contains("exited at startup") && err.contains("bind failed: boom"),
            "{err}"
        );
        assert!(daemon.0.launches.lock().unwrap().children.is_empty());
        // Within the backoff, the next request reports the same failure without launching again.
        assert_eq!(daemon.connect().unwrap_err(), err);
        assert_eq!(std::fs::read_to_string(&launched).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_report_a_launch_that_dies_once() {
        let dir = scratch("events-dies");
        let launched = dir.join("launched");
        let events = Events::new(dying_ftmd(&dir, &launched));
        let (tx, rx) = mpsc::channel();
        events.restart(
            |event| panic!("{event:?}"),
            |version| panic!("{version}"),
            move |e| {
                let _ = tx.send(e);
            },
        );
        let err = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            err.contains("exited at startup") && err.contains("bind failed: boom"),
            "{err}"
        );
        // The stream keeps retrying every 500 ms, but the same failure is not reported again.
        assert!(rx.recv_timeout(Duration::from_millis(1500)).is_err());
        events.restart(|_| {}, |_| {}, |_| {});
        let _ = std::fs::remove_dir_all(&dir);
    }
}
