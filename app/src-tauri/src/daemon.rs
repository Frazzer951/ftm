use ftm_core::paths;
use ftm_core::protocol::{Event, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

type Reply = oneshot::Sender<Result<Response, String>>;

/// The one request connection lives on a worker thread; requests reach it over a channel, so they hit the daemon in
/// the order they were sent (keystrokes stay ordered) and a slow request never blocks the UI thread.
pub struct Client {
    tx: mpsc::Sender<(Request, Option<Reply>)>,
}

impl Client {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel::<(Request, Option<Reply>)>();
        std::thread::spawn(move || {
            let mut conn: Option<Conn> = None;
            for (request, reply) in rx {
                let result = send(&mut conn, &request);
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

impl Conn {
    fn open() -> Result<Self, String> {
        let write = connect()?;
        let read = BufReader::new(write.try_clone().map_err(|e| e.to_string())?);
        Ok(Conn { write, read })
    }

    fn roundtrip(&mut self, request: &Request) -> std::io::Result<String> {
        let mut out = serde_json::to_vec(request)?;
        out.push(b'\n');
        self.write.write_all(&out)?;
        let mut line = String::new();
        if self.read.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(line)
    }
}

/// Reconnects (starting ftmd if needed) and retries once when the connection has dropped.
fn send(conn: &mut Option<Conn>, request: &Request) -> Result<Response, String> {
    for attempt in 0..2 {
        let open = match conn {
            Some(open) => open,
            None => conn.insert(Conn::open()?),
        };
        match open.roundtrip(request) {
            Ok(line) => {
                return match serde_json::from_str(&line).map_err(|e| e.to_string())? {
                    Response::Error { message } => Err(message),
                    response => Ok(response),
                };
            },
            Err(e) => {
                *conn = None;
                if attempt == 1 {
                    return Err(e.to_string());
                }
            },
        }
    }
    unreachable!()
}

/// Forwards daemon events to the frontend. The page starts it once it is listening, and again after a reload, so
/// each start gets a fresh replay; an older stream notices the generation change and stops.
#[derive(Default)]
pub struct Events {
    generation: Arc<AtomicU64>,
    stream: Arc<Mutex<Option<UnixStream>>>,
}

impl Events {
    pub fn restart(&self, emit: impl Fn(Event) + Send + 'static) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(old) = self.stream.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = old.shutdown(Shutdown::Both);
        }
        let (current, slot) = (self.generation.clone(), self.stream.clone());
        // Runs on the main thread (from the `subscribe` command), where a panic takes the whole app down.
        let spawned = std::thread::Builder::new().name("ftmd-events".into()).spawn(move || {
            let live = || current.load(Ordering::SeqCst) == generation;
            while live() {
                if let Err(e) = subscribe(&emit, &slot, &live) {
                    eprintln!("ftmd event stream: {e}");
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        if let Err(e) = spawned {
            eprintln!("starting the ftmd event stream: {e}");
        }
    }
}

fn subscribe(emit: &impl Fn(Event), slot: &Mutex<Option<UnixStream>>, live: &impl Fn() -> bool) -> Result<(), String> {
    let mut conn = Conn::open()?;
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = conn.write.try_clone().ok();
    conn.roundtrip(&Request::Subscribe).map_err(|e| e.to_string())?;
    let mut line = String::new();
    while live() {
        line.clear();
        if conn.read.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("ftmd closed the connection".into());
        }
        match serde_json::from_str::<Event>(&line) {
            Ok(event) => emit(event),
            Err(e) => eprintln!("bad event from ftmd: {e}"),
        }
    }
    Ok(())
}

fn connect() -> Result<UnixStream, String> {
    let socket = paths::socket_path();
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    // The request worker and the event thread both connect at startup; only one of them may launch ftmd.
    static LAUNCH: Mutex<()> = Mutex::new(());
    let _guard = LAUNCH.lock().unwrap_or_else(PoisonError::into_inner);
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }
    launch()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() > deadline => return Err(format!("ftmd did not come up on {}: {e}", socket.display())),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Starts ftmd detached in its own process group so it outlives the app.
fn launch() -> Result<(), String> {
    let sibling = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("ftmd")));
    let binary = sibling.filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("ftmd"));
    let log_path = paths::socket_path().with_extension("log");
    if let Some(dir) = log_path.parent() {
        paths::private_dir(dir, paths::socket_dir_is_ours(dir)).map_err(|e| e.to_string())?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("opening {}: {e}", log_path.display()))?;
    Command::new(&binary)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("starting {}: {e}", binary.display()))?;
    Ok(())
}
