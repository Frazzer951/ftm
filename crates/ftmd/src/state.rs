use crate::frame::{self, Frame};
use crate::worktree;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eyre::{Result, WrapErr, bail, eyre};
use ftm_core::model::{Kind, Layout, PaneId, SplitDir, Status, Tab, TabId, Workspace, WorkspaceId, Worktree};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use ftm_core::{ENV_AGENT_CMD, ENV_NAME, ENV_PANE_ID, ENV_SOCKET, ENV_WORKSPACE_ID, paths};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub type Shared = Arc<Mutex<State>>;

const BUFFER_CAP: usize = 1 << 20;
const SCROLLBACK: usize = 10_000;
/// Trimming only once the buffer is this far over the cap keeps the per-read cost amortized.
const BUFFER_SLACK: usize = 256 << 10;

pub struct State {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    panes: HashMap<PaneId, Pane>,
    events: broadcast::Sender<Event>,
    socket: PathBuf,
    holders: PathBuf,
    state_file: PathBuf,
    /// Hook settings every agent gets via `--settings`, from `hooks::write`.
    claude_settings: Option<PathBuf>,
}

/// A pane's PTY and processes live in its holder (`ftmd pty-holder`), so they survive ftmd restarts; ftmd keeps a
/// connection to it plus its own copy of the output.
struct Pane {
    workspace: WorkspaceId,
    holder: Arc<Mutex<UnixStream>>,
    holder_socket: PathBuf,
    buffer: Vec<u8>,
    /// Headless emulator fed the same output, so the current screen and scrollback can be read back as text.
    screen: vt100::Parser<TitleWatch>,
}

/// Collects the terminal titles (OSC 0/2) a pane sets, in order, until read.
#[derive(Default)]
struct TitleWatch(Vec<String>);

impl vt100::Callbacks for TitleWatch {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.0.push(String::from_utf8_lossy(title).into_owned());
    }
}

/// Claude Code titles its terminal "<status glyph> <task summary>", with a spinner frame as the glyph while it works
/// and plain "Claude Code" before it has a task. Keep the summary only, so the name does not flicker.
fn task_title(raw: &str) -> Option<String> {
    let title = raw.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    (!title.is_empty() && title != "Claude Code").then(|| title.chars().take(80).collect())
}

#[derive(Serialize, Deserialize)]
struct Saved {
    workspaces: Vec<Workspace>,
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
}

impl State {
    pub fn new(socket: PathBuf, claude_settings: Option<PathBuf>) -> Shared {
        Arc::new(Mutex::new(State {
            workspaces: BTreeMap::new(),
            panes: HashMap::new(),
            events: broadcast::channel(1024).0,
            holders: paths::holders_dir(&socket),
            state_file: paths::state_file(&socket),
            socket,
            claude_settings,
        }))
    }

    fn list(&self) -> Vec<Workspace> {
        self.workspaces.values().cloned().collect()
    }

    /// Every change goes through here, so it is also where the tree is saved.
    fn emit_workspaces(&self) {
        let _ = self.events.send(Event::Workspaces { workspaces: self.list() });
        if let Err(e) = self.save() {
            tracing::warn!("saving {}: {e:#}", self.state_file.display());
        }
    }

    /// An empty tree leaves no file behind, so throwaway daemons (tests, smoke runs) clean up after themselves.
    fn save(&self) -> Result<()> {
        if self.workspaces.is_empty() {
            match std::fs::remove_file(&self.state_file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => return Ok(()),
            }
        }
        if let Some(dir) = self.state_file.parent() {
            paths::private_dir(dir, true)?;
        }
        let tmp = self.state_file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&Saved { workspaces: self.list() })?)?;
        std::fs::rename(&tmp, &self.state_file)?;
        Ok(())
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
        let holder_socket = self.holders.join(format!("{id}.sock"));

        let mut cmd = std::process::Command::new(std::env::current_exe().wrap_err("locating ftmd")?);
        cmd.arg("pty-holder")
            .arg(&holder_socket)
            .args(["24", "80"])
            .arg(&ws.cwd)
            .arg(&shell);
        match startup.and_then(script) {
            Some(script) => cmd.args(["-l", "-i", "-c", &script]),
            None => cmd.arg("-l"),
        };
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key == "CLAUDECODE" || key == "FTM_BRIEF" || key.starts_with("CLAUDE_CODE_") || key.starts_with("SUPACODE_") {
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
        if let Some(settings) = &self.claude_settings {
            cmd.env("FTM_CLAUDE_SETTINGS", settings);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null());
        // Its own session, so nothing aimed at ftmd (a signal to its process group, its terminal closing) reaches it.
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
                bail!("pane holder did not come up on {}", holder_socket.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        std::thread::spawn(move || holder.wait());
        self.attach_holder(shared, workspace, id, holder_socket, stream)?;
        Ok(id)
    }

    fn attach_holder(
        &mut self,
        shared: &Shared,
        workspace: WorkspaceId,
        id: PaneId,
        holder_socket: PathBuf,
        stream: UnixStream,
    ) -> Result<()> {
        let mut read = stream.try_clone()?;
        read.set_read_timeout(Some(Duration::from_secs(5)))?;
        let Some(Frame::Hello { version, rows, cols }) = frame::read(&mut read)? else {
            bail!("pane holder did not say hello");
        };
        if version != frame::VERSION {
            bail!("pane holder speaks version {version}, ftmd {}", frame::VERSION);
        }
        read.set_read_timeout(None)?;
        self.panes.insert(
            id,
            Pane {
                workspace,
                holder: Arc::new(Mutex::new(stream)),
                holder_socket,
                buffer: Vec::new(),
                screen: vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, TitleWatch::default()),
            },
        );
        let shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("pane-{id}"))
            .spawn(move || read_loop(shared, id, read))
            .wrap_err("starting pane reader")?;
        Ok(())
    }

    fn send_to_holder(&self, pane: PaneId, frame: &Frame) -> Result<()> {
        let holder = self.pane(pane)?.holder.clone();
        let mut holder = holder.lock().unwrap_or_else(PoisonError::into_inner);
        frame::write(&mut *holder, frame).wrap_err("writing to pane holder")
    }
}

/// Restores the saved tree, reconnecting each pane to its holder; a pane whose holder is gone (it exited, or the
/// machine rebooted) is dropped from its layout. Workspaces stay even when that leaves them without tabs.
pub fn load(shared: &Shared) -> Result<()> {
    let mut state = lock(shared);
    let saved: Saved = match std::fs::read(&state.state_file) {
        Ok(bytes) => serde_json::from_slice(&bytes).wrap_err_with(|| format!("parsing {}", state.state_file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for ws in saved.workspaces {
        state.workspaces.insert(ws.id, ws);
    }
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
        let holder_socket = state.holders.join(format!("{pane}.sock"));
        let attached = UnixStream::connect(&holder_socket)
            .map_err(eyre::Report::from)
            .and_then(|stream| state.attach_holder(shared, ws, pane, holder_socket.clone(), stream));
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

fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" }.to_string())
}

fn script(startup: Startup) -> Option<String> {
    let setup = startup.setup.map(|s| format!("{s}; ")).unwrap_or_default();
    if startup.agent {
        let agent = std::env::var(ENV_AGENT_CMD).ok().filter(|c| !c.trim().is_empty());
        let agent = agent.as_deref().unwrap_or("claude");
        let name = if startup.named { r#" -n "$FTM_NAME""# } else { "" };
        Some(format!(
            r#"{setup}{agent} ${{FTM_CLAUDE_SETTINGS:+--settings}} ${{FTM_CLAUDE_SETTINGS:+"$FTM_CLAUDE_SETTINGS"}}{name} ${{FTM_BRIEF:+"$FTM_BRIEF"}}; exec "$SHELL" -l"#
        ))
    } else if startup.setup.is_some() {
        Some(format!(r#"{setup}exec "$SHELL" -l"#))
    } else {
        None
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The first `Output` after (re)connecting is the holder's whole buffer, which rebuilds the screen after a restart.
/// The holder going away means the pane's processes are gone.
fn read_loop(shared: Shared, id: PaneId, mut holder: UnixStream) {
    while let Ok(Some(frame)) = frame::read(&mut holder) {
        let Frame::Output(bytes) = frame else { continue };
        let mut state = lock(&shared);
        let Some(pane) = state.panes.get_mut(&id) else { return };
        pane.buffer.extend_from_slice(&bytes);
        pane.screen.process(&bytes);
        let (workspace, titles) = (pane.workspace, std::mem::take(&mut pane.screen.callbacks_mut().0));
        if pane.buffer.len() > BUFFER_CAP + BUFFER_SLACK {
            let excess = pane.buffer.len() - BUFFER_CAP;
            pane.buffer.drain(..excess);
        }
        let _ = state.events.send(Event::Output {
            pane: id,
            data: B64.encode(&bytes),
            reset: false,
        });
        // The last task-like title in this chunk wins; placeholders and the bare name in between do not count.
        if let Some(ws) = state.workspaces.get_mut(&workspace)
            && ws.agent_pane == Some(id)
            && let Some(title) = titles.iter().rev().filter_map(|t| task_title(t)).find(|t| *t != ws.name)
            && ws.auto_title.as_deref() != Some(&title)
        {
            ws.auto_title = Some(title);
            state.emit_workspaces();
        }
    }
    close_pane(&shared, id);
}

/// Idempotent: a pane that exits on its own and a `ClosePane` for it race to get here.
fn close_pane(shared: &Shared, id: PaneId) {
    let pane = {
        let mut state = lock(shared);
        let Some(pane) = state.panes.remove(&id) else { return };
        state.detach(pane.workspace, id);
        state.emit_workspaces();
        pane
    };
    drop(reap(pane));
}

/// Asks the holder to kill everything on the pane's terminal (it does the SIGHUP, then SIGKILL after 2s), and
/// returns a handle that finishes once the holder is gone, i.e. no process of the pane is left.
fn reap(pane: Pane) -> std::thread::JoinHandle<()> {
    {
        let mut holder = pane.holder.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = frame::write(&mut *holder, &Frame::Kill);
    }
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while pane.holder_socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    })
}

/// Snapshot taken under the lock, so replaying it and then reading `rx` neither drops nor repeats output.
pub fn subscribe(shared: &Shared) -> (broadcast::Receiver<Event>, Vec<Event>) {
    let state = lock(shared);
    let rx = state.events.subscribe();
    let mut replay = vec![Event::Workspaces {
        workspaces: state.list(),
    }];
    replay.extend(state.panes.iter().map(|(id, pane)| Event::Output {
        pane: *id,
        data: B64.encode(&pane.buffer),
        reset: true,
    }));
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

pub fn handle(shared: &Shared, request: Request) -> Response {
    let result = match request {
        Request::Ping => Ok(Response::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        Request::List => Ok(Response::Workspaces {
            workspaces: lock(shared).list(),
        }),
        Request::Spawn(req) => spawn(shared, req),
        Request::SetStatus { id, status } => {
            let mut state = lock(shared);
            state.workspace_mut(id).map(|ws| ws.status = status).map(|()| {
                state.emit_workspaces();
                Response::Ok
            })
        },
        Request::Close { id, remove_worktree } => close(shared, id, remove_worktree),
        Request::Rename { id, name } => rename(shared, id, &name),
        Request::NewTab { workspace } => new_tab(shared, workspace),
        Request::Split { pane, dir } => split(shared, pane, dir),
        Request::ClosePane { pane } => {
            close_pane(shared, pane);
            Ok(Response::Ok)
        },
        Request::Input { pane, data } => input(shared, pane, &data),
        Request::Capture { pane, raw } => capture(shared, pane, raw),
        Request::Resize { pane, cols, rows } => resize(shared, pane, cols, rows),
        Request::Subscribe | Request::Attach { .. } => Err(eyre!("handled by the connection")),
    };
    result.unwrap_or_else(|e| Response::Error {
        message: format!("{e:#}"),
    })
}

fn spawn(shared: &Shared, req: SpawnRequest) -> Result<Response> {
    if let Some(parent) = req.parent
        && !lock(shared).workspaces.contains_key(&parent)
    {
        bail!("no workspace {parent}");
    }
    let (cwd, worktree, setup) = match &req.worktree_branch {
        Some(branch) => {
            let created = worktree::create(&req.cwd, branch, req.base.as_deref(), req.copy_changes)?;
            let worktree = Worktree {
                repo: created.repo,
                branch: branch.clone(),
            };
            (created.path, Some(worktree), created.setup)
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
    state.workspaces.insert(
        id,
        Workspace {
            id,
            parent: req.parent,
            name,
            title: None,
            auto_title: None,
            agent_pane: None,
            kind: req.kind,
            cwd,
            worktree,
            status: Status::Starting,
            tabs: Vec::new(),
        },
    );
    let named = req.name.is_some();
    let startup = Startup {
        agent: req.kind == Kind::Agent,
        named,
        setup: setup.as_deref(),
        brief: req.brief.as_deref(),
    };
    let pane = match state.open_pane(shared, id, Some(startup)) {
        Ok(pane) => pane,
        Err(e) => {
            state.workspaces.remove(&id);
            return Err(e);
        },
    };
    let ws = state.workspace_mut(id)?;
    ws.tabs.push(Tab {
        id: TabId::new_v4(),
        layout: Layout::Pane { id: pane },
    });
    if ws.kind == Kind::Agent && !named {
        ws.agent_pane = Some(pane);
    }
    let workspace = ws.clone();
    state.emit_workspaces();
    Ok(Response::Spawned { workspace })
}

/// Kills the workspace's panes and moves its children up to its own parent.
fn close(shared: &Shared, id: WorkspaceId, remove_worktree: bool) -> Result<Response> {
    let (ws, panes) = {
        let mut state = lock(shared);
        let ws = state.workspaces.remove(&id).ok_or_else(|| eyre!("no workspace {id}"))?;
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

fn new_tab(shared: &Shared, workspace: WorkspaceId) -> Result<Response> {
    let mut state = lock(shared);
    let pane = state.open_pane(shared, workspace, None)?;
    state.workspace_mut(workspace)?.tabs.push(Tab {
        id: TabId::new_v4(),
        layout: Layout::Pane { id: pane },
    });
    state.emit_workspaces();
    Ok(Response::Pane { pane })
}

fn split(shared: &Shared, target: PaneId, dir: SplitDir) -> Result<Response> {
    let mut state = lock(shared);
    let workspace = state.pane(target)?.workspace;
    let pane = state.open_pane(shared, workspace, None)?;
    let ws = state.workspace_mut(workspace)?;
    if !ws.tabs.iter_mut().any(|t| t.layout.split(target, pane, dir)) {
        drop(state);
        close_pane(shared, pane);
        bail!("pane {target} is not in a tab");
    }
    state.emit_workspaces();
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

fn resize(shared: &Shared, pane: PaneId, cols: u16, rows: u16) -> Result<Response> {
    let mut state = lock(shared);
    state.pane_mut(pane)?.screen.screen_mut().set_size(rows, cols);
    state.send_to_holder(pane, &Frame::Resize { rows, cols })?;
    Ok(Response::Ok)
}

/// Written outside the state lock: a pane that is not reading its input must not stall every other request.
fn input(shared: &Shared, pane: PaneId, data: &str) -> Result<Response> {
    let bytes = B64.decode(data).wrap_err("input is not base64")?;
    let holder = lock(shared).pane(pane)?.holder.clone();
    let mut holder = holder.lock().unwrap_or_else(PoisonError::into_inner);
    frame::write(&mut *holder, &Frame::Input(bytes)).wrap_err("writing to pane holder")?;
    Ok(Response::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn task_title_drops_the_status_glyph() {
        assert_eq!(task_title("✳ Fix the bounds").as_deref(), Some("Fix the bounds"));
        assert_eq!(task_title("⠂ Fix the bounds ").as_deref(), Some("Fix the bounds"));
        assert_eq!(task_title("✳ Claude Code"), None);
        assert_eq!(task_title(" ✳ "), None);
    }

    /// zsh does not word-split unquoted expansions, so each optional argument has to be its own expansion.
    #[test]
    fn agent_script_passes_the_same_argv_in_sh_and_zsh() {
        // SAFETY: no other test in this binary reads FTM_AGENT_CMD.
        unsafe { std::env::set_var(ENV_AGENT_CMD, "printf '[%s]'") };
        let unnamed = script(Startup {
            agent: true,
            named: false,
            setup: None,
            brief: None,
        })
        .unwrap();
        assert!(!unnamed.contains(" -n "));
        let line = script(Startup {
            agent: true,
            named: true,
            setup: None,
            brief: None,
        })
        .unwrap();
        let line = line.split("; exec").next().unwrap();
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"]
            .into_iter()
            .filter(|s| Path::new(s).exists())
        {
            for (settings, brief, want) in [
                (
                    Some("/tmp/s.json"),
                    Some("a \"b\" $(c)"),
                    "[--settings][/tmp/s.json][-n][n][a \"b\" $(c)]",
                ),
                (None, None, "[-n][n]"),
            ] {
                let mut cmd = std::process::Command::new(shell);
                cmd.args(["-c", line]).env("FTM_NAME", "n");
                cmd.env_remove("FTM_CLAUDE_SETTINGS").env_remove("FTM_BRIEF");
                settings.map(|s| cmd.env("FTM_CLAUDE_SETTINGS", s));
                brief.map(|b| cmd.env("FTM_BRIEF", b));
                let out = cmd.output().unwrap();
                assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{shell}");
            }
        }
    }
}
