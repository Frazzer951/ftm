use crate::frame::{self, Frame};
use crate::worktree;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eyre::{Result, WrapErr, bail, eyre};
use ftm_core::model::{
    Folder, Kind, Layout, PaneId, SplitDir, Status, Tab, TabId, WORKSPACE_COLORS, Workspace, WorkspaceId, Worktree,
    workspace_color,
};
use ftm_core::protocol::{
    Event, FEATURE_CLAUDE_ARGS, FEATURE_SPAWN_COLOR, FEATURE_WORKSPACE_COLORS, FEATURE_WORKSPACE_ORDER, Request, Response,
    SpawnRequest,
};
use ftm_core::{ENV_AGENT_CMD, ENV_NAME, ENV_PANE_ID, ENV_SOCKET, ENV_WORKSPACE_ID, paths};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub type Shared = Arc<Mutex<State>>;

const BUFFER_CAP: usize = 1 << 20;
const SCROLLBACK: usize = 10_000;
/// The emulator panics wrapping a line on one row, or a double-width character on one column.
const MIN_ROWS: u16 = 2;
const MIN_COLS: u16 = 2;
/// Trimming only once the buffer is this far over the cap keeps the per-read cost amortized.
const BUFFER_SLACK: usize = 256 << 10;
/// Long enough that the TUI reads the narrower size before the real one comes back; two SIGWINCHs handled together
/// look like no change at all.
const REDRAW_NUDGE: Duration = Duration::from_millis(150);
/// Input is sent to a holder in frames of at most this size, far below `frame::MAX_PAYLOAD`.
const INPUT_CHUNK: usize = 64 << 10;
/// Changes within this long of each other are saved together: every tool use of every agent sets its status.
const SAVE_DELAY: Duration = Duration::from_millis(250);

pub struct State {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    /// Most recently used first.
    folders: Vec<Folder>,
    /// The sidebar's order, set by dragging: every path in `folders`, exactly once.
    folder_order: Vec<PathBuf>,
    /// The sidebar's workspace order, set by dragging; `list` puts any workspace it lacks at the end.
    workspace_order: Vec<WorkspaceId>,
    panes: HashMap<PaneId, Pane>,
    events: broadcast::Sender<Event>,
    socket: PathBuf,
    holders: PathBuf,
    state_file: PathBuf,
    saver: Arc<Saver>,
    /// Hook settings every agent gets via `--settings`, from `hooks::write`.
    claude_settings: Option<PathBuf>,
    /// The last `Event::UpdateBlocked`, replayed to every subscriber.
    update_blocked: Option<Event>,
}

/// A pane's PTY and processes live in its holder (`ftmd pty-holder`), so they survive ftmd restarts; ftmd keeps a
/// connection to it plus its own copy of the output.
struct Pane {
    workspace: WorkspaceId,
    /// Frames for the holder, written by the pane's own thread so a holder that is slow to read stalls nothing else.
    holder: mpsc::Sender<ToHolder>,
    holder_socket: PathBuf,
    buffer: Vec<u8>,
    /// Headless emulator fed the same output, so the current screen and scrollback can be read back as text.
    screen: vt100::Parser<TitleWatch>,
    /// Opened by `spawn` to run the workspace's setup and agent.
    startup: bool,
    /// Its holder is gone but it stays on screen, showing why its agent never started.
    exited: bool,
    /// Processes with an `ftm attach` stream open on the pane, once per stream. Their resizes fit the pane to their
    /// own terminal.
    attachers: Vec<i32>,
    /// The size to go back to once the last attach ends: the pane's size when the first began, or the latest any
    /// other client set since. Other clients (the app) only resize when their own size changes.
    restore: Option<(u16, u16)>,
}

enum ToHolder {
    Frame(Frame),
    /// A new connection to the holder, after it hung up on the last one.
    Connected(UnixStream),
}

/// Collects the terminal titles (OSC 0/2) a pane sets, in order, until read.
#[derive(Default)]
struct TitleWatch(Vec<String>);

impl vt100::Callbacks for TitleWatch {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.0.push(String::from_utf8_lossy(title).into_owned());
    }
}

/// Claude Code titles its terminal `<status glyph> <task summary>`, with a spinner frame as the glyph while it works
/// and plain "Claude Code" before it has a task. Keep the summary only, so the name does not flicker.
fn task_title(raw: &str) -> Option<String> {
    let title = raw.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    (!title.is_empty() && title != "Claude Code").then(|| title.chars().take(80).collect())
}

/// Writes the tree on its own thread, off the state lock: syncing a file can take a long while (F_FULLFSYNC on macOS),
/// and everything else waits on that lock.
#[derive(Default)]
struct Saver {
    /// Changes made so far. Bumped under the state lock, so a snapshot taken under it holds every change it counts.
    changes: AtomicU64,
    /// The changes the file on disk holds. Locked while writing, so an older snapshot never replaces a newer one.
    written: Mutex<u64>,
    requested: Mutex<bool>,
    wake: Condvar,
    writes: AtomicUsize,
}

struct Snapshot {
    changes: u64,
    path: PathBuf,
    /// `None` when there is nothing to keep.
    saved: Option<Saved>,
}

impl Saver {
    fn request(&self) {
        self.changes.fetch_add(1, Ordering::SeqCst);
        *guard(&self.requested) = true;
        self.wake.notify_one();
    }

    /// An empty tree leaves no file behind, so throwaway daemons (tests, smoke runs) clean up after themselves.
    fn write(&self, snapshot: Snapshot) -> Result<()> {
        let mut written = guard(&self.written);
        if snapshot.changes <= *written {
            return Ok(());
        }
        let path = &snapshot.path;
        match snapshot.saved {
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {},
            },
            Some(saved) => {
                if let Some(dir) = path.parent() {
                    paths::private_dir(dir, true)?;
                }
                let tmp = path.with_extension("json.tmp");
                // Synced before the rename, so a crash leaves the old tree or the new one, never an empty file.
                let mut file = std::fs::File::create(&tmp)?;
                std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(&saved)?)?;
                file.sync_all()?;
                std::fs::rename(&tmp, path)?;
            },
        }
        *written = snapshot.changes;
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Saves a while after each request, until the state is gone.
    fn run(&self, state: &Weak<Mutex<State>>) {
        loop {
            let mut requested = guard(&self.requested);
            while !*requested {
                if state.strong_count() == 0 {
                    return;
                }
                requested = self
                    .wake
                    .wait_timeout(requested, Duration::from_secs(1))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            drop(requested);
            std::thread::sleep(SAVE_DELAY);
            *guard(&self.requested) = false;
            let Some(shared) = state.upgrade() else { return };
            let snapshot = lock(&shared).snapshot();
            drop(shared);
            if let Err(e) = self.write(snapshot) {
                tracing::warn!("saving the workspaces: {e:#}");
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    workspaces: Vec<Workspace>,
    #[serde(default)]
    folders: Vec<Folder>,
    #[serde(default)]
    folder_order: Vec<PathBuf>,
    #[serde(default)]
    workspace_order: Vec<WorkspaceId>,
}

/// What the first command in a pane runs before handing over to a login shell.
#[derive(Clone, Copy)]
struct Startup<'a> {
    agent: bool,
    /// Spawned with an explicit name: the agent runs as `claude -n <name>`, a stable `SendMessage` address. Without
    /// one it gets no `-n`, so Claude titles its terminal with its task and the workspace takes that as `auto_title`.
    named: bool,
    setup: Option<&'a str>,
    brief: Option<&'a str>,
    command: Option<&'a str>,
    /// Extra arguments for Claude, each in its own `FTM_CLAUDE_ARG_<n>` (from 1).
    claude_args: &'a [String],
}

impl State {
    pub fn new(socket: PathBuf, claude_settings: Option<PathBuf>) -> Shared {
        let saver = Arc::new(Saver::default());
        let shared = Arc::new(Mutex::new(State {
            workspaces: BTreeMap::new(),
            folders: Vec::new(),
            folder_order: Vec::new(),
            workspace_order: Vec::new(),
            panes: HashMap::new(),
            events: broadcast::channel(1024).0,
            holders: paths::holders_dir(&socket),
            state_file: paths::state_file(&socket),
            saver: saver.clone(),
            socket,
            claude_settings,
            update_blocked: None,
        }));
        let state = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("saver".into())
            .spawn(move || saver.run(&state))
            .expect("starting the saver thread");
        shared
    }

    /// In sidebar order.
    fn list(&self) -> Vec<Workspace> {
        let order: std::collections::HashSet<&WorkspaceId> = self.workspace_order.iter().collect();
        let listed = self.workspace_order.iter().filter_map(|id| self.workspaces.get(id));
        let rest = self.workspaces.values().filter(|w| !order.contains(&w.id));
        listed.chain(rest).cloned().collect()
    }

    /// Every change goes through here, so it is also where the tree is saved.
    fn emit_workspaces(&self) {
        let _ = self.events.send(Event::Workspaces { workspaces: self.list() });
        self.saver.request();
    }

    fn folders_event(&self) -> Event {
        Event::Folders {
            folders: self.folders.clone(),
            order: self.folder_order.clone(),
        }
    }

    fn emit_folders(&mut self) {
        self.folder_order = folder_order(std::mem::take(&mut self.folder_order), &self.folders);
        let _ = self.events.send(self.folders_event());
        self.saver.request();
    }

    /// Moves `path` to the front of the remembered folders.
    fn remember_folder(&mut self, path: PathBuf) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.folders.retain(|f| f.path != path);
        self.folders.insert(0, Folder { path, last_used: now });
        self.emit_folders();
    }

    fn snapshot(&self) -> Snapshot {
        let empty = self.workspaces.is_empty() && self.folders.is_empty();
        Snapshot {
            changes: self.saver.changes.load(Ordering::SeqCst),
            path: self.state_file.clone(),
            saved: (!empty).then(|| {
                let workspaces = self.list();
                Saved {
                    workspace_order: workspaces.iter().map(|w| w.id).collect(),
                    workspaces,
                    folders: self.folders.clone(),
                    folder_order: self.folder_order.clone(),
                }
            }),
        }
    }

    /// Writes whatever the saver has not yet.
    fn save(&self) -> Result<()> {
        self.saver.write(self.snapshot())
    }

    fn workspace_mut(&mut self, id: WorkspaceId) -> Result<&mut Workspace> {
        self.workspaces.get_mut(&id).ok_or_else(|| eyre!("no workspace {id}"))
    }

    fn pane(&self, id: PaneId) -> Result<&Pane> {
        self.panes.get(&id).ok_or_else(|| eyre!("no pane {id}"))
    }

    fn pane_mut(&mut self, id: PaneId) -> Result<&mut Pane> {
        self.panes.get_mut(&id).ok_or_else(|| eyre!("no pane {id}"))
    }

    /// Drops `pane` from its tab's layout, and the tab itself once it has no panes left.
    fn detach(&mut self, workspace: WorkspaceId, pane: PaneId) {
        let Some(ws) = self.workspaces.get_mut(&workspace) else {
            return;
        };
        let Some(i) = ws.tabs.iter().position(|t| t.layout.contains(pane)) else {
            return;
        };
        match ws.tabs[i].layout.clone().remove(pane) {
            Some(layout) => ws.tabs[i].layout = layout,
            None => {
                ws.tabs.remove(i);
            },
        }
    }

    fn open_pane(&mut self, shared: &Shared, workspace: WorkspaceId, startup: Option<Startup>) -> Result<PaneId> {
        let ws = self
            .workspaces
            .get(&workspace)
            .ok_or_else(|| eyre!("no workspace {workspace}"))?;
        let id = PaneId::new_v4();
        let shell = shell();
        paths::private_dir(&self.holders, true).wrap_err_with(|| format!("creating {}", self.holders.display()))?;
        let holder_socket = paths::holder_socket(&self.holders, id);

        let mut cmd = std::process::Command::new(own_exe()?);
        cmd.arg("pty-holder")
            .arg(&holder_socket)
            .args(["24", "80"])
            .arg(&ws.cwd)
            .arg(&shell);
        let agent = std::env::var(ENV_AGENT_CMD).ok();
        match startup.and_then(|s| script(s, agent.as_deref(), self.claude_settings.is_some())) {
            Some(script) => cmd.args(["-l", "-i", "-c", &script]),
            None => cmd.arg("-l"),
        };
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key == "CLAUDECODE"
                || key == "FTM_BRIEF"
                || key == "FTM_SETUP"
                || key == "FTM_COMMAND"
                || key.starts_with(ENV_CLAUDE_ARG)
                || key.starts_with("CLAUDE_CODE_")
                || key.starts_with("SUPACODE_")
            {
                cmd.env_remove(key.as_ref());
            }
        }
        cmd.env("SHELL", &shell)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env(ENV_SOCKET, &self.socket)
            .env(ENV_WORKSPACE_ID, workspace.to_string())
            .env(ENV_PANE_ID, id.to_string())
            .env(ENV_NAME, &ws.name)
            .env("FTM_BIN", crate::hooks::ftm_binary());
        if let Some(brief) = startup.and_then(|s| s.brief) {
            cmd.env("FTM_BRIEF", brief);
        }
        if let Some(setup) = startup.and_then(|s| s.setup) {
            cmd.env("FTM_SETUP", setup);
        }
        if let Some(command) = startup.and_then(|s| s.command) {
            cmd.env("FTM_COMMAND", command);
        }
        for (i, arg) in startup.map_or(&[][..], |s| s.claude_args).iter().enumerate() {
            cmd.env(format!("{ENV_CLAUDE_ARG}{}", i + 1), arg);
        }
        if let Some(settings) = &self.claude_settings {
            cmd.env("FTM_CLAUDE_SETTINGS", settings);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        // Its own session, so nothing aimed at ftmd (a signal to its process group, its terminal closing) reaches it.
        // SAFETY: between fork and exec the closure only calls setsid, which is async-signal-safe and does not
        // allocate.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut holder = cmd.spawn().wrap_err("starting pane holder")?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            if let Ok(stream) = UnixStream::connect(&holder_socket) {
                break stream;
            }
            if let Ok(Some(status)) = holder.try_wait() {
                bail!("pane holder exited at startup ({status})");
            }
            if Instant::now() > deadline {
                let _ = holder.kill();
                let _ = holder.wait();
                bail!("pane holder did not come up on {}", holder_socket.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Before the replay goes in, so an unnamed agent's title printed before ftmd connected still names it.
        if startup.is_some_and(|s| s.agent && !s.named)
            && let Some(ws) = self.workspaces.get_mut(&workspace)
        {
            ws.agent_pane = Some(id);
        }
        if let Err(e) = self.attach_holder(shared, workspace, id, holder_socket.clone(), stream, startup.is_some()) {
            // Closing its PTY hangs up the shell it started. Killed through the unreaped child, so the pid is still its.
            let _ = holder.kill();
            let _ = holder.wait();
            let _ = std::fs::remove_file(&holder_socket);
            if e.downcast_ref::<HolderVersion>().is_some() {
                return Err(e.wrap_err(
                    "ftmd's binary was replaced by a build it cannot run panes with; restarting ftmd finishes the update but closes every pane",
                ));
            }
            return Err(e);
        }
        std::thread::spawn(move || holder.wait());
        Ok(id)
    }

    fn attach_holder(
        &mut self,
        shared: &Shared,
        workspace: WorkspaceId,
        id: PaneId,
        holder_socket: PathBuf,
        stream: UnixStream,
        startup: bool,
    ) -> Result<()> {
        let (read, (rows, cols), replay) = handshake(&stream)?;
        // The replay goes in before anyone can see the pane, so a capture or attach right after a restart has the screen.
        self.panes.insert(
            id,
            Pane {
                workspace,
                holder: holder_writer(id, stream)?,
                holder_socket,
                buffer: Vec::new(),
                screen: vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, TitleWatch::default()),
                startup,
                exited: false,
                attachers: Vec::new(),
                restore: None,
            },
        );
        // Sent once the pane is in a layout (`emit_replay`): a client drops output for panes it does not know.
        if self.pane_output(id, &replay, false) {
            let _ = nudge(shared, self, id);
        }
        let shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("pane-{id}"))
            .spawn(move || read_loop(shared, id, read))
            .wrap_err("starting pane reader")?;
        Ok(())
    }

    /// `live` output goes out to subscribers as it comes; a holder's replay is sent whole by the caller. Returns
    /// whether the emulator had to start the screen over, which leaves it blank until the pane repaints (`nudge`).
    fn pane_output(&mut self, id: PaneId, bytes: &[u8], live: bool) -> bool {
        let Some(pane) = self.panes.get_mut(&id) else { return false };
        pane.buffer.extend_from_slice(bytes);
        let restarted = !process(id, &mut pane.screen, bytes);
        let (workspace, titles) = (pane.workspace, std::mem::take(&mut pane.screen.callbacks_mut().0));
        if pane.buffer.len() > BUFFER_CAP + BUFFER_SLACK {
            let excess = pane.buffer.len() - BUFFER_CAP;
            pane.buffer.drain(..excess);
        }
        if live {
            let _ = self.events.send(Event::Output {
                pane: id,
                data: B64.encode(bytes),
                reset: false,
            });
        }
        // The last task-like title in this chunk wins; placeholders and the bare name in between do not count.
        if let Some(ws) = self.workspaces.get_mut(&workspace)
            && ws.agent_pane == Some(id)
            && let Some(title) = titles.iter().rev().filter_map(|t| task_title(t)).find(|t| *t != ws.name)
            && ws.auto_title.as_deref() != Some(&title)
        {
            ws.auto_title = Some(title);
            self.emit_workspaces();
        }
        restarted
    }

    /// The pane's whole buffer, for subscribers to redraw it from scratch.
    fn emit_replay(&self, id: PaneId) {
        if let Some(pane) = self.panes.get(&id) {
            let _ = self.events.send(Event::Output {
                pane: id,
                data: B64.encode(&pane.buffer),
                reset: true,
            });
        }
    }

    fn send_to_holder(&self, pane: PaneId, frame: Frame) -> Result<()> {
        self.pane(pane)?
            .holder
            .send(ToHolder::Frame(frame))
            .map_err(|_| eyre!("pane {pane}'s holder connection is closed"))
    }
}

/// Frames that fail to go out, because the holder hung up on ftmd, wait for the connection `reconnect` makes.
fn holder_writer(id: PaneId, stream: UnixStream) -> Result<mpsc::Sender<ToHolder>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("pane-{id}-in"))
        .spawn(move || {
            let mut stream = Some(stream);
            let mut queued = std::collections::VecDeque::new();
            for msg in rx {
                match msg {
                    ToHolder::Frame(frame) => queued.push_back(frame),
                    ToHolder::Connected(new) => stream = Some(new),
                }
                while let Some(conn) = stream.as_mut()
                    && let Some(frame) = queued.front()
                {
                    if frame::write(conn, frame).is_err() {
                        stream = None;
                        break;
                    }
                    queued.pop_front();
                }
            }
        })
        .wrap_err("starting pane writer")?;
    Ok(tx)
}

/// A new connection to a holder starts with its `Hello` and then its whole buffer. Returns the stream to read on, the
/// terminal's size, and the buffer.
fn handshake(stream: &UnixStream) -> Result<(UnixStream, (u16, u16), Vec<u8>)> {
    let mut read = stream.try_clone()?;
    read.set_read_timeout(Some(Duration::from_secs(5)))?;
    let Some(Frame::Hello { version, rows, cols }) = frame::read(&mut read)? else {
        bail!("pane holder did not say hello");
    };
    if version != frame::VERSION {
        return Err(HolderVersion(version).into());
    }
    let Some(Frame::Output(replay)) = frame::read(&mut read)? else {
        bail!("pane holder did not send its buffer");
    };
    read.set_read_timeout(None)?;
    Ok((read, (rows.max(MIN_ROWS), cols.max(MIN_COLS)), replay))
}

/// A panic in the emulator would otherwise end the pane's reader and leave the pane frozen; the screen starts over
/// instead, while the raw output (what the app shows) carries on. Returns false when it had to.
fn process(id: PaneId, screen: &mut vt100::Parser<TitleWatch>, bytes: &[u8]) -> bool {
    let processed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| screen.process(bytes)));
    if processed.is_err() {
        let (rows, cols) = screen.screen().size();
        tracing::error!("pane {id}: the terminal emulator panicked; starting its screen over");
        *screen = vt100::Parser::new_with_callbacks(rows.max(MIN_ROWS), cols.max(MIN_COLS), SCROLLBACK, TitleWatch::default());
    }
    processed.is_ok()
}

#[derive(Debug)]
struct HolderVersion(u8);

impl std::fmt::Display for HolderVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pane holder speaks version {}, ftmd {}", self.0, frame::VERSION)
    }
}

impl std::error::Error for HolderVersion {}

fn read_saved(path: &std::path::Path) -> Result<Option<Saved>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).wrap_err_with(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// What `load` would fail on, checked by an ftmd taking over before the old one lets go.
pub fn check_saved(shared: &Shared) -> Result<()> {
    let path = lock(shared).state_file.clone();
    read_saved(&path).map(drop)
}

/// For shutdown: the last changes may still be waiting for the saver.
pub fn flush(shared: &Shared) -> Result<()> {
    let state = lock(shared);
    state
        .save()
        .wrap_err_with(|| format!("saving {}", state.state_file.display()))
}

/// Saves the tree and keeps the lock, so nothing changes or saves it again while a new ftmd takes over.
pub fn freeze(shared: &Shared) -> Result<MutexGuard<'_, State>> {
    let state = lock(shared);
    state
        .save()
        .wrap_err_with(|| format!("saving {}", state.state_file.display()))?;
    Ok(state)
}

/// `order` cut down to the paths in `folders`, each once, then the folders it lacks (new ones, or all of them from a
/// state file that predates the order) by name, the order the sidebar used before it could be rearranged.
fn folder_order(order: Vec<PathBuf>, folders: &[Folder]) -> Vec<PathBuf> {
    let all = folders.iter().map(|f| f.path.clone()).collect();
    known_then_rest(order, all, |p| {
        (p.file_name().map(|n| n.to_string_lossy().to_lowercase()), p.clone())
    })
}

/// `order` cut down to the ids in `workspaces`, each once (the first time it appears), then the workspaces it lacks by
/// displayed name: all of them from a state file that predates the order.
fn workspace_order(
    order: impl IntoIterator<Item = WorkspaceId>,
    workspaces: &BTreeMap<WorkspaceId, Workspace>,
) -> Vec<WorkspaceId> {
    known_then_rest(order, workspaces.keys().copied().collect(), |id| {
        (workspaces[id].display_name().to_lowercase(), *id)
    })
}

/// The items of `order` that are in `all`, each once (the first time it appears), then the rest of `all` sorted by
/// `rest_key`.
fn known_then_rest<K: Clone + Eq + std::hash::Hash, S: Ord>(
    order: impl IntoIterator<Item = K>,
    all: Vec<K>,
    rest_key: impl FnMut(&K) -> S,
) -> Vec<K> {
    let known: std::collections::HashSet<&K> = all.iter().collect();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<K> = order
        .into_iter()
        .filter(|k| known.contains(k) && seen.insert(k.clone()))
        .collect();
    let mut rest: Vec<K> = all.iter().filter(|k| !seen.contains(*k)).cloned().collect();
    rest.sort_by_key(rest_key);
    out.extend(rest);
    out
}

pub fn update_blocked(shared: &Shared, version: Option<String>, reason: String) {
    let mut state = lock(shared);
    let event = Event::UpdateBlocked { version, reason };
    let _ = state.events.send(event.clone());
    state.update_blocked = Some(event);
}

/// Restores the saved tree, reconnecting each pane to its holder; a pane whose holder is gone (it exited, or the
/// machine rebooted) is dropped from its layout. Workspaces stay even when that leaves them without tabs.
pub fn load(shared: &Shared) -> Result<()> {
    let mut state = lock(shared);
    let saved = match read_saved(&state.state_file) {
        Ok(saved) => saved,
        Err(e) if e.downcast_ref::<serde_json::Error>().is_some() => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let aside = state.state_file.with_extension(format!("json.bad-{stamp}"));
            std::fs::rename(&state.state_file, &aside)
                .wrap_err_with(|| format!("moving {} aside", state.state_file.display()))?;
            tracing::error!("{e:#}; moved it to {} and starting with no workspaces", aside.display());
            None
        },
        Err(e) => return Err(e),
    };
    let Some(saved) = saved else {
        return Ok(());
    };
    for ws in saved.workspaces {
        state.workspaces.insert(ws.id, ws);
    }
    state.folder_order = folder_order(saved.folder_order, &saved.folders);
    state.folders = saved.folders;
    state.workspace_order = workspace_order(saved.workspace_order, &state.workspaces);
    let ids: Vec<WorkspaceId> = state.workspaces.keys().copied().collect();
    for ws in state.workspaces.values_mut() {
        if ws.parent.is_some_and(|p| !ids.contains(&p)) {
            ws.parent = None;
        }
    }
    let panes: Vec<(WorkspaceId, PaneId)> = state
        .workspaces
        .values()
        .flat_map(|w| w.tabs.iter().flat_map(|t| t.layout.panes()).map(|p| (w.id, p)))
        .collect();
    let (mut kept, mut dropped) = (0, 0);
    for (ws, pane) in panes {
        let holder_socket = [
            paths::holder_socket(&state.holders, pane),
            paths::legacy_holder_socket(&state.holders, pane),
        ]
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(|| paths::holder_socket(&state.holders, pane));
        let attached = UnixStream::connect(&holder_socket)
            .map_err(eyre::Report::from)
            .and_then(|stream| state.attach_holder(shared, ws, pane, holder_socket.clone(), stream, false));
        match attached {
            Ok(()) => kept += 1,
            Err(e) => {
                tracing::info!("dropping pane {pane}: {e:#}");
                let _ = std::fs::remove_file(&holder_socket);
                state.detach(ws, pane);
                dropped += 1;
            },
        }
    }
    tracing::info!(
        "restored {} workspaces, {kept} panes ({dropped} gone)",
        state.workspaces.len()
    );
    state.emit_workspaces();
    Ok(())
}

/// The build to start pane holders from. Once a package update renames a new file over ftmd's path, Linux reports
/// that path as `... (deleted)`, but `/proc/self/exe` still opens the build that is running.
fn own_exe() -> Result<PathBuf> {
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe().wrap_err("locating ftmd")
}

fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" }.to_string())
}

/// Prefix of the variables carrying an agent's extra Claude arguments, numbered from 1.
const ENV_CLAUDE_ARG: &str = "FTM_CLAUDE_ARG_";

/// Runs under the user's login shell, which may be fish, so it sticks to syntax fish, zsh and bash share: optional
/// arguments are decided here rather than with `${VAR:+...}`, and values come in through quoted variables. The repo's
/// setup and a terminal's command are POSIX sh whatever the login shell, so each runs in its own `sh` from
/// `$FTM_SETUP` or `$FTM_COMMAND`.
fn script(startup: Startup, agent: Option<&str>, settings: bool) -> Option<String> {
    let setup = if startup.setup.is_some() {
        r#"/bin/sh -c "$FTM_SETUP"; "#
    } else {
        ""
    };
    if startup.agent {
        let mut line = agent.filter(|c| !c.trim().is_empty()).unwrap_or("claude").to_string();
        if settings {
            line.push_str(r#" --settings "$FTM_CLAUDE_SETTINGS""#);
        }
        if startup.named {
            line.push_str(r#" -n "$FTM_NAME""#);
        }
        for i in 1..=startup.claude_args.len() {
            line.push_str(&format!(r#" "${ENV_CLAUDE_ARG}{i}""#));
        }
        if startup.brief.is_some() {
            // An option that takes several values (`--add-dir <dirs...>`) would otherwise swallow the brief.
            if !startup.claude_args.is_empty() {
                line.push_str(" --");
            }
            line.push_str(r#" "$FTM_BRIEF""#);
        }
        Some(format!(r#"{setup}{line}; exec "$SHELL" -l"#))
    } else if startup.command.is_some() {
        Some(format!(r#"{setup}/bin/sh -c "$FTM_COMMAND"; exec "$SHELL" -l"#))
    } else if startup.setup.is_some() {
        Some(format!(r#"{setup}exec "$SHELL" -l"#))
    } else {
        None
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    guard(shared)
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The first `Output` after (re)connecting is the holder's whole buffer, which rebuilds the screen after a restart.
/// A holder hangs up on an ftmd that stopped reading its output, so a lost connection is retried; the holder's socket
/// being gone means the pane's processes are.
fn read_loop(shared: Shared, id: PaneId, mut holder: UnixStream) {
    let mut reconnected: Option<Instant> = None;
    let ended = loop {
        let lost = match frame::read(&mut holder) {
            Ok(Some(Frame::Output(bytes))) => {
                let mut state = lock(&shared);
                if !state.panes.contains_key(&id) {
                    return;
                }
                if state.pane_output(id, &bytes, true) {
                    let _ = nudge(&shared, &state, id);
                }
                continue;
            },
            Ok(Some(_)) => continue,
            Ok(None) => "its holder hung up".to_string(),
            Err(e) => format!("reading from its holder failed: {e}"),
        };
        // Another ftmd taking the holder over would take it straight back.
        if reconnected.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            std::thread::sleep(Duration::from_secs(1));
        }
        match reconnect(&shared, id) {
            Ok(Some(stream)) => {
                tracing::info!("pane {id}: reconnected to its holder ({lost})");
                holder = stream;
                reconnected = Some(Instant::now());
            },
            Ok(None) => return,
            Err(e) => break format!("{lost}; reconnecting failed: {e:#}"),
        }
    };
    pane_exited(&shared, id, &ended);
}

/// `None` once the pane is closed.
fn reconnect(shared: &Shared, id: PaneId) -> Result<Option<UnixStream>> {
    let Some(socket) = lock(shared).panes.get(&id).map(|p| p.holder_socket.clone()) else {
        return Ok(None);
    };
    let stream = UnixStream::connect(&socket)?;
    let (read, (rows, cols), replay) = handshake(&stream)?;
    let mut state = lock(shared);
    let Some(pane) = state.panes.get_mut(&id) else {
        // Closed while this connection was being made, which may have replaced the one `reap` sent its Kill on.
        let _ = frame::write(&mut &stream, &Frame::Kill);
        return Ok(None);
    };
    let _ = pane.holder.send(ToHolder::Connected(stream));
    // A resize sent while the holder had hung up may not have reached it.
    let wanted = pane.screen.screen().size();
    pane.buffer.clear();
    pane.screen = vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, TitleWatch::default());
    let restarted = state.pane_output(id, &replay, false);
    if let Some(pane) = state.panes.get_mut(&id)
        && (rows, cols) != wanted
        && !pane.exited
    {
        pane.screen.screen_mut().set_size(wanted.0, wanted.1);
        let _ = pane.holder.send(ToHolder::Frame(Frame::Resize {
            rows: wanted.0,
            cols: wanted.1,
        }));
    }
    if restarted {
        let _ = nudge(shared, &state, id);
    }
    state.emit_replay(id);
    Ok(Some(read))
}

/// An agent whose startup pane exits before Claude reports any status never started (setup the shell could not
/// parse, no `claude`): the pane stays with what it printed and the workspace turns `failed`. Any other pane closes.
fn pane_exited(shared: &Shared, id: PaneId, why: &str) {
    {
        let mut guard = lock(shared);
        let state = &mut *guard;
        let Some(pane) = state.panes.get_mut(&id) else { return };
        if let Some(ws) = state.workspaces.get_mut(&pane.workspace)
            && pane.startup
            && ws.kind == Kind::Agent
            && ws.status == Status::Starting
        {
            tracing::warn!(
                "agent {} exited before starting ({why}):\n{}",
                ws.name,
                screen_text(&mut pane.screen)
            );
            const NOTE: &[u8] = b"\r\n\x1b[31m[ftm] the agent exited before it started\x1b[0m\r\n";
            pane.exited = true;
            pane.buffer.extend_from_slice(NOTE);
            let _ = process(id, &mut pane.screen, NOTE);
            set_status(ws, Status::Failed, 0);
            let _ = state.events.send(Event::Output {
                pane: id,
                data: B64.encode(NOTE),
                reset: false,
            });
            state.emit_workspaces();
            return;
        }
    }
    close_pane(shared, id, why);
}

/// Idempotent: a pane that exits on its own and a `ClosePane` for it race to get here.
fn close_pane(shared: &Shared, id: PaneId, why: &str) {
    let pane = {
        let mut state = lock(shared);
        let Some(pane) = state.panes.remove(&id) else { return };
        let name = state.workspaces.get(&pane.workspace).map_or("?", |w| w.name.as_str());
        tracing::info!("closing pane {id} of {name}: {why}");
        state.detach(pane.workspace, id);
        state.emit_workspaces();
        pane
    };
    drop(reap(pane));
}

/// Asks the holder to kill everything on the pane's terminal (it does the SIGHUP, then SIGKILL after 2s), and
/// returns a handle that finishes once the holder is gone, i.e. no process of the pane is left.
///
/// The Kill also goes over a connection of its own: the pane's may be one the holder already hung up on, with
/// `read_loop` yet to reconnect. A new connection replaces the old one and is read until the holder exits, so the
/// replay and output it gets meanwhile never block the holder.
fn reap(pane: Pane) -> std::thread::JoinHandle<()> {
    let _ = pane.holder.send(ToHolder::Frame(Frame::Kill));
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        if let Ok(mut stream) = UnixStream::connect(&pane.holder_socket)
            && frame::write(&mut stream, &Frame::Kill).is_ok()
        {
            let mut buf = vec![0u8; 64 << 10];
            while let Some(left) = deadline.checked_duration_since(Instant::now())
                && stream.set_read_timeout(Some(left.max(Duration::from_millis(1)))).is_ok()
                && matches!(std::io::Read::read(&mut stream, &mut buf), Ok(n) if n > 0)
            {}
        }
        while pane.holder_socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    })
}

/// Snapshot taken under the lock, so replaying it and then reading `rx` neither drops nor repeats output.
pub fn subscribe(shared: &Shared) -> (broadcast::Receiver<Event>, Vec<Event>) {
    let state = lock(shared);
    let rx = state.events.subscribe();
    let mut replay = vec![
        Event::Workspaces {
            workspaces: state.list(),
        },
        state.folders_event(),
    ];
    replay.extend(state.panes.iter().map(|(id, pane)| Event::Output {
        pane: *id,
        data: B64.encode(&pane.buffer),
        reset: true,
    }));
    replay.extend(state.update_blocked.clone());
    (rx, replay)
}

/// For `ftm attach`: the pane's current screen as escape sequences that redraw it, taken under the lock like
/// `subscribe`.
pub fn attach(shared: &Shared, pane: PaneId) -> Result<(broadcast::Receiver<Event>, Vec<u8>)> {
    let state = lock(shared);
    let p = state.pane(pane)?;
    let mut screen = b"\x1b[H\x1b[2J".to_vec();
    screen.extend(p.screen.screen().state_formatted());
    Ok((state.events.subscribe(), screen))
}

/// Registers an `ftm attach` stream from `pid`, until the returned guard drops.
pub fn attached(shared: &Shared, pane: PaneId, pid: i32) -> Attached {
    if let Ok(p) = lock(shared).pane_mut(pane) {
        if p.attachers.is_empty() {
            p.restore = Some(p.screen.screen().size());
        }
        p.attachers.push(pid);
    }
    Attached {
        shared: shared.clone(),
        pane,
        pid,
    }
}

pub struct Attached {
    shared: Shared,
    pane: PaneId,
    pid: i32,
}

impl Drop for Attached {
    fn drop(&mut self) {
        let restore = {
            let mut state = lock(&self.shared);
            let Ok(p) = state.pane_mut(self.pane) else { return };
            if let Some(i) = p.attachers.iter().position(|&pid| pid == self.pid) {
                p.attachers.remove(i);
            }
            if !p.attachers.is_empty() {
                return;
            }
            p.restore.take()
        };
        if let Some((rows, cols)) = restore {
            let _ = resize(&self.shared, self.pane, cols, rows, None);
        }
    }
}

pub fn pane_alive(workspaces: &[Workspace], pane: PaneId) -> bool {
    workspaces.iter().any(|w| w.tabs.iter().any(|t| t.layout.contains(pane)))
}

/// Scrollback lines oldest first, then the visible screen, with trailing blanks trimmed.
fn screen_text(parser: &mut vt100::Parser<TitleWatch>) -> String {
    let screen = parser.screen_mut();
    let cols = screen.size().1;
    screen.set_scrollback(usize::MAX);
    let mut lines = Vec::new();
    for offset in (1..=screen.scrollback()).rev() {
        screen.set_scrollback(offset);
        lines.extend(screen.rows(0, cols).next());
    }
    screen.set_scrollback(0);
    lines.extend(screen.rows(0, cols));
    let mut text = lines.iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n");
    text.truncate(text.trim_end().len());
    text.push('\n');
    text
}

/// `peer` is the pid of the process that sent `request`, when known.
pub fn handle(shared: &Shared, request: Request, peer: Option<i32>) -> Response {
    let result = match request {
        Request::Ping => Ok(Response::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: [
                FEATURE_CLAUDE_ARGS,
                FEATURE_WORKSPACE_COLORS,
                FEATURE_WORKSPACE_ORDER,
                FEATURE_SPAWN_COLOR,
            ]
            .map(String::from)
            .to_vec(),
        }),
        Request::List => Ok(Response::Workspaces {
            workspaces: lock(shared).list(),
        }),
        Request::Spawn(req) => spawn(shared, req),
        Request::SetStatus {
            id,
            status,
            background_agents,
        } if status != Status::Unknown => {
            let mut state = lock(shared);
            state
                .workspace_mut(id)
                .map(|ws| {
                    let before = (ws.status, ws.unseen, ws.background_agents);
                    set_status(ws, status, background_agents);
                    before != (ws.status, ws.unseen, ws.background_agents)
                })
                .map(|changed| {
                    if changed {
                        state.emit_workspaces();
                    }
                    Response::Ok
                })
        },
        Request::Close { id, remove_worktree } => close(shared, id, remove_worktree),
        Request::Rename { id, name } => rename(shared, id, &name),
        Request::Pin { id, pinned } => pin(shared, id, pinned),
        Request::SetColor { id, color } => set_color(shared, id, color.as_deref()),
        Request::MarkSeen { id } => {
            let mut state = lock(shared);
            state
                .workspace_mut(id)
                .map(|ws| std::mem::replace(&mut ws.unseen, false))
                .map(|was| {
                    if was {
                        state.emit_workspaces();
                    }
                    Response::Ok
                })
        },
        Request::Folders => {
            let state = lock(shared);
            Ok(Response::Folders {
                folders: state.folders.clone(),
                order: state.folder_order.clone(),
            })
        },
        Request::RemoveFolder { path } => {
            let mut state = lock(shared);
            state.folders.retain(|f| f.path != path);
            state.emit_folders();
            Ok(Response::Ok)
        },
        Request::ReorderFolders { paths } => {
            let mut state = lock(shared);
            state.folder_order = paths;
            state.emit_folders();
            Ok(Response::Ok)
        },
        Request::ReorderWorkspaces { ids } => {
            let mut state = lock(shared);
            let current: Vec<WorkspaceId> = state.list().iter().map(|w| w.id).collect();
            state.workspace_order = workspace_order(ids.into_iter().chain(current), &state.workspaces);
            state.emit_workspaces();
            Ok(Response::Ok)
        },
        Request::NewTab { workspace } => new_tab(shared, workspace),
        Request::Split { pane, dir } => split(shared, pane, dir),
        Request::ClosePane { pane } => {
            close_pane(shared, pane, "ClosePane requested");
            Ok(Response::Ok)
        },
        Request::Input { pane, data } => input(shared, pane, &data),
        Request::Capture { pane, raw } => capture(shared, pane, raw),
        Request::Resize { pane, cols, rows } => resize(shared, pane, cols, rows, peer),
        Request::SetStatus { .. } => Err(eyre!("unknown status")),
        Request::Subscribe | Request::Attach { .. } => Err(eyre!("handled by the connection")),
    };
    result.unwrap_or_else(|e| Response::Error {
        message: format!("{e:#}"),
    })
}

fn set_status(ws: &mut Workspace, status: Status, background_agents: u32) {
    if ws.status.completes(status) {
        ws.unseen = true;
    } else if matches!(status, Status::Starting | Status::Working | Status::NeedsInput) {
        ws.unseen = false;
    }
    ws.status = status;
    ws.background_agents = background_agents;
}

fn spawn(shared: &Shared, mut req: SpawnRequest) -> Result<Response> {
    if req.kind == Kind::Unknown {
        bail!("unknown workspace kind");
    }
    if req.command.is_some() && req.kind == Kind::Agent {
        bail!("a command only applies to terminals");
    }
    if !req.claude_args.is_empty() && req.kind != Kind::Agent {
        bail!("claude arguments only apply to agents");
    }
    req.color = parse_color(req.color.as_deref())?;
    if let Some(parent) = req.parent
        && !lock(shared).workspaces.contains_key(&parent)
    {
        bail!("no workspace {parent}");
    }
    let created = match &req.worktree_branch {
        Some(branch) => Some(worktree::create(&req.cwd, branch, req.base.as_deref(), req.copy_changes)?),
        None => None,
    };
    let result = open_spawned(shared, &req, created.as_ref());
    if result.is_err()
        && let Some(created) = &created
    {
        created.discard();
    }
    result
}

fn open_spawned(shared: &Shared, req: &SpawnRequest, created: Option<&worktree::Created>) -> Result<Response> {
    let (cwd, worktree, setup) = match created {
        Some(created) => {
            let worktree = Worktree {
                repo: created.repo.clone(),
                branch: created.branch.clone(),
            };
            (created.path.clone(), Some(worktree), created.setup.clone())
        },
        None => (req.cwd.clone(), None, None),
    };
    if !cwd.is_dir() {
        bail!("{} is not a directory", cwd.display());
    }

    let id = WorkspaceId::new_v4();
    let name = req
        .name
        .clone()
        .or_else(|| worktree.as_ref().map(|w| w.branch.clone()))
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| id.to_string());
    let mut state = lock(shared);
    if let Some(parent) = req.parent
        && !state.workspaces.contains_key(&parent)
    {
        bail!("no workspace {parent}");
    }
    state.workspace_order.push(id);
    state.workspaces.insert(
        id,
        Workspace {
            id,
            parent: req.parent,
            name,
            title: None,
            auto_title: None,
            agent_pane: None,
            pinned: false,
            color: req.color.clone(),
            kind: req.kind,
            cwd,
            worktree,
            status: Status::Starting,
            unseen: false,
            background_agents: 0,
            tabs: Vec::new(),
        },
    );
    let named = req.name.is_some();
    let startup = Startup {
        agent: req.kind == Kind::Agent,
        named,
        setup: setup.as_deref(),
        brief: req.brief.as_deref(),
        command: req.command.as_deref(),
        claude_args: &req.claude_args,
    };
    let pane = match state.open_pane(shared, id, Some(startup)) {
        Ok(pane) => pane,
        Err(e) => {
            state.workspaces.remove(&id);
            state.workspace_order.retain(|w| *w != id);
            return Err(e);
        },
    };
    let hooked = state.claude_settings.is_some();
    let ws = state.workspace_mut(id)?;
    // Without its hooks the agent never reports, so there is nothing to wait for.
    if !hooked && req.kind == Kind::Agent {
        ws.status = Status::Idle;
    }
    ws.tabs.push(Tab {
        id: TabId::new_v4(),
        layout: Layout::Pane { id: pane },
    });
    let workspace = ws.clone();
    // The folder the user picked, not the worktree made from it. A one-off command is not a pick.
    if req.parent.is_none() && req.command.is_none() {
        state.remember_folder(req.cwd.clone());
    }
    state.emit_workspaces();
    state.emit_replay(pane);
    Ok(Response::Spawned {
        workspace: Box::new(workspace),
    })
}

/// Kills the workspace's panes and moves its children up to its own parent.
fn close(shared: &Shared, id: WorkspaceId, remove_worktree: bool) -> Result<Response> {
    let (ws, panes) = {
        let mut state = lock(shared);
        let ws = state.workspaces.remove(&id).ok_or_else(|| eyre!("no workspace {id}"))?;
        state.workspace_order.retain(|w| *w != id);
        for child in state.workspaces.values_mut().filter(|w| w.parent == Some(id)) {
            child.parent = ws.parent;
        }
        let ids: Vec<PaneId> = state
            .panes
            .iter()
            .filter(|(_, p)| p.workspace == id)
            .map(|(k, _)| *k)
            .collect();
        let panes: Vec<Pane> = ids.iter().filter_map(|k| state.panes.remove(k)).collect();
        tracing::info!("closing workspace {} ({id}) and its panes {ids:?}", ws.name);
        state.emit_workspaces();
        (ws, panes)
    };
    let reaping: Vec<_> = panes.into_iter().map(reap).collect();
    if remove_worktree && let Some(wt) = &ws.worktree {
        // Nothing may still be running in the worktree when it is deleted.
        reaping.into_iter().for_each(|h| drop(h.join()));
        worktree::remove(&wt.repo, &ws.cwd, &wt.branch)?;
    }
    Ok(Response::Ok)
}

fn rename(shared: &Shared, id: WorkspaceId, name: &str) -> Result<Response> {
    let title = match name.trim() {
        "" if !name.is_empty() => bail!("name is blank"),
        "" => None,
        trimmed => Some(trimmed.to_string()),
    };
    let mut state = lock(shared);
    state.workspace_mut(id)?.title = title;
    state.emit_workspaces();
    Ok(Response::Ok)
}

/// Only top-level workspaces can be pinned: a child stays under its parent in the sidebar while that parent exists.
fn pin(shared: &Shared, id: WorkspaceId, pinned: bool) -> Result<Response> {
    let mut state = lock(shared);
    let parent = state.workspace_mut(id)?.parent;
    if pinned && parent.is_some_and(|p| state.workspaces.contains_key(&p)) {
        bail!("workspace {id} is nested under another workspace; only top-level workspaces can be pinned");
    }
    state.workspace_mut(id)?.pinned = pinned;
    state.emit_workspaces();
    Ok(Response::Ok)
}

/// `color` in the form `Workspace::color` keeps; an empty one is `None`.
fn parse_color(color: Option<&str>) -> Result<Option<String>> {
    match color.map(str::trim).filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) => Ok(Some(workspace_color(c).ok_or_else(|| {
            eyre!("unknown color {c:?}: use {} or #rrggbb", WORKSPACE_COLORS.join(", "))
        })?)),
    }
}

/// An empty `color` clears it, like `None`.
fn set_color(shared: &Shared, id: WorkspaceId, color: Option<&str>) -> Result<Response> {
    let color = parse_color(color)?;
    let mut state = lock(shared);
    state.workspace_mut(id)?.color = color;
    state.emit_workspaces();
    Ok(Response::Ok)
}

fn new_tab(shared: &Shared, workspace: WorkspaceId) -> Result<Response> {
    let mut state = lock(shared);
    let pane = state.open_pane(shared, workspace, None)?;
    state.workspace_mut(workspace)?.tabs.push(Tab {
        id: TabId::new_v4(),
        layout: Layout::Pane { id: pane },
    });
    state.emit_workspaces();
    state.emit_replay(pane);
    Ok(Response::Pane { pane })
}

fn split(shared: &Shared, target: PaneId, dir: SplitDir) -> Result<Response> {
    let mut state = lock(shared);
    let workspace = state.pane(target)?.workspace;
    let pane = state.open_pane(shared, workspace, None)?;
    let ws = state.workspace_mut(workspace)?;
    if !ws.tabs.iter_mut().any(|t| t.layout.split(target, pane, dir)) {
        drop(state);
        close_pane(shared, pane, "its split had no tab");
        bail!("pane {target} is not in a tab");
    }
    state.emit_workspaces();
    state.emit_replay(pane);
    Ok(Response::Pane { pane })
}

fn capture(shared: &Shared, pane: PaneId, raw: bool) -> Result<Response> {
    let mut state = lock(shared);
    let pane = state.pane_mut(pane)?;
    let data = if raw {
        B64.encode(&pane.buffer)
    } else {
        B64.encode(screen_text(&mut pane.screen))
    };
    Ok(Response::Output { data })
}

/// A resize to the size the pane already has comes from a terminal that just attached (the app relaunched, `ftm
/// attach`), which drew the replay at some other size. The kernel sends no SIGWINCH for an unchanged size, and Node
/// ignores one that does not change it, so the pane is nudged one column narrower and back to make its TUI repaint.
fn resize(shared: &Shared, pane: PaneId, cols: u16, rows: u16, peer: Option<i32>) -> Result<Response> {
    // A collapsed view can report no size at all.
    let (rows, cols) = (rows.max(MIN_ROWS), cols.max(MIN_COLS));
    let mut state = lock(shared);
    let p = state.pane_mut(pane)?;
    if !p.attachers.is_empty() && !peer.is_some_and(|pid| p.attachers.contains(&pid)) {
        p.restore = Some((rows, cols));
    }
    let unchanged = p.screen.screen().size() == (rows, cols);
    p.screen.screen_mut().set_size(rows, cols);
    if p.exited {
        return Ok(Response::Ok);
    }
    if unchanged {
        nudge(shared, &state, pane)?;
    } else {
        state.send_to_holder(pane, Frame::Resize { rows, cols })?;
    }
    Ok(Response::Ok)
}

/// Makes the pane's TUI repaint: resizes its terminal by a column, and back to the pane's size a moment later.
fn nudge(shared: &Shared, state: &State, pane: PaneId) -> Result<()> {
    let p = state.pane(pane)?;
    if p.exited {
        return Ok(());
    }
    let (rows, cols) = p.screen.screen().size();
    let cols = if cols > MIN_COLS { cols - 1 } else { cols + 1 };
    state.send_to_holder(pane, Frame::Resize { rows, cols })?;
    let shared = shared.clone();
    std::thread::spawn(move || {
        std::thread::sleep(REDRAW_NUDGE);
        let state = lock(&shared);
        if let Ok(p) = state.pane(pane)
            && !p.exited
        {
            let (rows, cols) = p.screen.screen().size();
            let _ = state.send_to_holder(pane, Frame::Resize { rows, cols });
        }
    });
    Ok(())
}

fn input(shared: &Shared, pane: PaneId, data: &str) -> Result<Response> {
    let bytes = B64.decode(data).wrap_err("input is not base64")?;
    let state = lock(shared);
    if state.pane(pane)?.exited {
        return Ok(Response::Ok);
    }
    for chunk in bytes.chunks(INPUT_CHUNK) {
        state.send_to_holder(pane, Frame::Input(chunk.to_vec()))?;
    }
    Ok(Response::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn folder(path: &str) -> Folder {
        Folder {
            path: path.into(),
            last_used: 0,
        }
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn folder_order_keeps_known_paths_and_appends_the_rest_by_name() {
        let folders = [folder("/x/zeta"), folder("/y/Alpha"), folder("/x/mid")];
        assert_eq!(folder_order(vec![], &folders), paths(&["/y/Alpha", "/x/mid", "/x/zeta"]));
        assert_eq!(
            folder_order(paths(&["/x/zeta", "/gone", "/x/zeta", "/y/Alpha"]), &folders),
            paths(&["/x/zeta", "/y/Alpha", "/x/mid"]),
        );
    }

    #[test]
    fn folder_order_survives_mru_bumps_and_puts_new_folders_last() {
        let dir = std::env::temp_dir().join(format!("ftmd-order-{}", std::process::id()));
        let shared = State::new(dir.join("ftmd.sock"), None);
        let mut state = lock(&shared);
        state.state_file = dir.join("state.json");
        for p in ["/b", "/a", "/c"] {
            state.remember_folder(p.into());
        }
        assert_eq!(state.folder_order, paths(&["/b", "/a", "/c"]));
        state.folder_order = paths(&["/c", "/a"]);
        state.emit_folders();
        assert_eq!(state.folder_order, paths(&["/c", "/a", "/b"]));
        state.remember_folder("/b".into());
        state.remember_folder("/0".into());
        assert_eq!(state.folders[0].path, PathBuf::from("/0"));
        assert_eq!(state.folder_order, paths(&["/c", "/a", "/b", "/0"]));
        state.folders.retain(|f| f.path != Path::new("/a"));
        state.emit_folders();
        assert_eq!(state.folder_order, paths(&["/c", "/b", "/0"]));

        state.save().unwrap();
        let saved: Saved = serde_json::from_slice(&std::fs::read(&state.state_file).unwrap()).unwrap();
        assert_eq!(saved.folder_order, state.folder_order);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_burst_of_changes_is_saved_once() {
        let dir = std::env::temp_dir().join(format!("ftmd-burst-{}", std::process::id()));
        let shared = State::new(dir.join("ftmd.sock"), None);
        lock(&shared).state_file = dir.join("state.json");
        for i in 0..100 {
            lock(&shared).remember_folder(format!("/{i}").into());
        }
        let saver = lock(&shared).saver.clone();
        let deadline = Instant::now() + Duration::from_secs(10);
        while *guard(&saver.written) < saver.changes.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the burst was never saved");
            std::thread::sleep(Duration::from_millis(20));
        }
        let saved: Saved = serde_json::from_slice(&std::fs::read(dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(saved.folders.len(), 100);
        assert!(
            saver.writes.load(Ordering::Relaxed) <= 2,
            "{} writes",
            saver.writes.load(Ordering::Relaxed)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_files_without_a_folder_order_still_load() {
        let saved: Saved = serde_json::from_str(r#"{"workspaces":[],"folders":[{"path":"/b","last_used":1}]}"#).unwrap();
        assert!(saved.folder_order.is_empty());
        assert_eq!(folder_order(saved.folder_order, &saved.folders), paths(&["/b"]));
    }

    #[test]
    fn the_smallest_screen_takes_any_output() {
        let mut parser = vt100::Parser::new_with_callbacks(MIN_ROWS, MIN_COLS, SCROLLBACK, TitleWatch::default());
        let stress = "一二三\tabcdefgh\r\n\x1b[2;1H一x一\n\n\x1b[1;2r一一一\x1b[r\x1b[999C一\x1b[999B一\n";
        for _ in 0..3 {
            parser.process(stress.as_bytes());
        }
    }

    #[test]
    fn an_emulator_panic_starts_the_screen_over() {
        let mut screen = vt100::Parser::new_with_callbacks(1, 1, SCROLLBACK, TitleWatch::default());
        assert!(!process(PaneId::nil(), &mut screen, "一\n一\n".as_bytes()));
        assert_eq!(screen.screen().size(), (MIN_ROWS, MIN_COLS));
        assert!(process(PaneId::nil(), &mut screen, b"ok"));
        assert!(screen.screen().contents().contains("ok"));
    }

    #[test]
    fn workspace_order_keeps_known_ids_and_appends_the_rest_by_shown_name() {
        let ws = |name: &str, title: Option<&str>| -> Workspace {
            serde_json::from_value(serde_json::json!({
                "id": WorkspaceId::new_v4(), "parent": null, "name": name, "title": title, "kind": "terminal",
                "cwd": "/", "worktree": null, "status": "idle", "tabs": [],
            }))
            .unwrap()
        };
        let (zeta, alpha, mid) = (ws("zeta", None), ws("zz", Some("Alpha")), ws("mid", None));
        let ids = [zeta.id, alpha.id, mid.id];
        let all: BTreeMap<WorkspaceId, Workspace> = [zeta, alpha, mid].into_iter().map(|w| (w.id, w)).collect();
        // A state file from before the order: by the name the sidebar shows.
        assert_eq!(workspace_order([], &all), vec![ids[1], ids[2], ids[0]]);
        assert_eq!(
            workspace_order([ids[0], WorkspaceId::new_v4(), ids[0], ids[2]], &all),
            vec![ids[0], ids[2], ids[1]],
        );
    }

    #[test]
    fn closing_or_failing_to_open_a_workspace_drops_it_from_the_order() {
        let dir = std::env::temp_dir().join(format!("ftmd-ws-order-{}", std::process::id()));
        let shared = State::new(dir.join("ftmd.sock"), None);
        let ws: Workspace = serde_json::from_value(serde_json::json!({
            "id": WorkspaceId::new_v4(), "parent": null, "name": "a", "kind": "terminal",
            "cwd": "/", "worktree": null, "status": "idle", "tabs": [],
        }))
        .unwrap();
        let id = ws.id;
        {
            let mut state = lock(&shared);
            state.state_file = dir.join("state.json");
            state.workspaces.insert(id, ws);
            state.workspace_order.push(id);
        }
        close(&shared, id, false).unwrap();
        assert!(lock(&shared).workspace_order.is_empty());

        // Holders cannot live under a file, so the first pane never opens.
        lock(&shared).holders = PathBuf::from("/dev/null/holders");
        let req = SpawnRequest {
            kind: Kind::Terminal,
            name: None,
            parent: None,
            cwd: "/".into(),
            worktree_branch: None,
            base: None,
            copy_changes: false,
            brief: None,
            command: None,
            claude_args: Vec::new(),
            color: None,
        };
        assert!(open_spawned(&shared, &req, None).is_err());
        let state = lock(&shared);
        assert!(state.workspaces.is_empty());
        assert!(state.workspace_order.is_empty());
        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn task_title_drops_the_status_glyph() {
        assert_eq!(task_title("✳ Fix the bounds").as_deref(), Some("Fix the bounds"));
        assert_eq!(task_title("⠂ Fix the bounds ").as_deref(), Some("Fix the bounds"));
        assert_eq!(task_title("✳ Claude Code"), None);
        assert_eq!(task_title(" ✳ "), None);
    }

    #[test]
    fn agent_script_passes_the_same_argv_in_every_shell() {
        let startup = |named, brief, claude_args| Startup {
            agent: true,
            named,
            setup: None,
            brief,
            command: None,
            claude_args,
        };
        let agent = Some("printf '[%s]'");
        assert!(!script(startup(false, None, &[]), agent, false).unwrap().contains(" -n "));
        let args = [
            "--add-dir".to_string(),
            "a b; $(c) 'd' \"e\" (f) *".to_string(),
            String::new(),
        ];
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/fish", "/usr/bin/fish"]
            .into_iter()
            .filter(|s| Path::new(s).exists())
        {
            for (settings, brief, claude_args, want) in [
                (
                    Some("/tmp/s.json"),
                    Some("a \"b\" $(c) (d) ' e"),
                    &[][..],
                    "[--settings][/tmp/s.json][-n][n][a \"b\" $(c) (d) ' e]",
                ),
                (None, None, &[], "[-n][n]"),
                (
                    None,
                    Some("go"),
                    &args[..],
                    "[-n][n][--add-dir][a b; $(c) 'd' \"e\" (f) *][][--][go]",
                ),
                (None, None, &args[..2], "[-n][n][--add-dir][a b; $(c) 'd' \"e\" (f) *]"),
            ] {
                let line = script(startup(true, brief, claude_args), agent, settings.is_some()).unwrap();
                let line = line.split("; exec").next().unwrap().to_string();
                let mut cmd = std::process::Command::new(shell);
                cmd.args(["-c", &line]).env("FTM_NAME", "n");
                cmd.env_remove("FTM_CLAUDE_SETTINGS").env_remove("FTM_BRIEF");
                settings.map(|s| cmd.env("FTM_CLAUDE_SETTINGS", s));
                brief.map(|b| cmd.env("FTM_BRIEF", b));
                for (i, arg) in claude_args.iter().enumerate() {
                    cmd.env(format!("{ENV_CLAUDE_ARG}{}", i + 1), arg);
                }
                let out = cmd.output().unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{shell}");
            }
        }
    }
}
