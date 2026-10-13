use crate::worktree;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eyre::{Result, WrapErr, bail, eyre};
use ftm_core::model::{Kind, Layout, PaneId, SplitDir, Status, Tab, TabId, Workspace, WorkspaceId, Worktree};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use ftm_core::{ENV_NAME, ENV_PANE_ID, ENV_SOCKET, ENV_WORKSPACE_ID};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub type Shared = Arc<Mutex<State>>;

const BUFFER_CAP: usize = 1 << 20;
/// Trimming only once the buffer is this far over the cap keeps the per-read cost amortized.
const BUFFER_SLACK: usize = 256 << 10;

pub struct State {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    panes: HashMap<PaneId, Pane>,
    events: broadcast::Sender<Event>,
    socket: PathBuf,
}

struct Pane {
    workspace: WorkspaceId,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    buffer: Vec<u8>,
}

/// What the first command in a pane runs before handing over to a login shell.
#[derive(Clone, Copy)]
struct Startup<'a> {
    agent: bool,
    setup: Option<&'a str>,
    brief: Option<&'a str>,
}

impl State {
    pub fn new(socket: PathBuf) -> Shared {
        Arc::new(Mutex::new(State {
            workspaces: BTreeMap::new(),
            panes: HashMap::new(),
            events: broadcast::channel(1024).0,
            socket,
        }))
    }

    fn list(&self) -> Vec<Workspace> {
        self.workspaces.values().cloned().collect()
    }

    fn emit_workspaces(&self) {
        let _ = self.events.send(Event::Workspaces { workspaces: self.list() });
    }

    fn workspace_mut(&mut self, id: WorkspaceId) -> Result<&mut Workspace> {
        self.workspaces.get_mut(&id).ok_or_else(|| eyre!("no workspace {id}"))
    }

    fn pane(&self, id: PaneId) -> Result<&Pane> {
        self.panes.get(&id).ok_or_else(|| eyre!("no pane {id}"))
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

        let mut cmd = CommandBuilder::new(&shell);
        match startup.and_then(script) {
            Some(script) => cmd.args(["-l", "-i", "-c", &script]),
            None => cmd.arg("-l"),
        }
        cmd.cwd(&ws.cwd);
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key == "CLAUDECODE" || key == "FTM_BRIEF" || key.starts_with("CLAUDE_CODE_") || key.starts_with("SUPACODE_") {
                cmd.env_remove(key.as_ref());
            }
        }
        cmd.env("SHELL", &shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env(ENV_SOCKET, &self.socket);
        cmd.env(ENV_WORKSPACE_ID, workspace.to_string());
        cmd.env(ENV_PANE_ID, id.to_string());
        cmd.env(ENV_NAME, &ws.name);
        if let Some(brief) = startup.and_then(|s| s.brief) {
            cmd.env("FTM_BRIEF", brief);
        }

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| eyre!("opening pty: {e}"))?;
        let child = pair.slave.spawn_command(cmd).map_err(|e| eyre!("starting {shell}: {e}"))?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| eyre!("{e}"))?;
        let writer = pair.master.take_writer().map_err(|e| eyre!("{e}"))?;

        self.panes.insert(
            id,
            Pane {
                workspace,
                master: pair.master,
                writer: Arc::new(Mutex::new(writer)),
                child,
                buffer: Vec::new(),
            },
        );
        let shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("pane-{id}"))
            .spawn(move || read_loop(shared, id, reader))
            .wrap_err("starting pane reader")?;
        Ok(id)
    }
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
        Some(format!(
            r#"{setup}claude -n "$FTM_NAME" ${{FTM_BRIEF:+"$FTM_BRIEF"}}; exec "$SHELL" -l"#
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

fn read_loop(shared: Shared, id: PaneId, mut reader: Box<dyn Read + Send>) {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let mut state = lock(&shared);
        // A closed pane keeps being drained until its processes are gone, so none of them block writing to the tty.
        let Some(pane) = state.panes.get_mut(&id) else { continue };
        pane.buffer.extend_from_slice(&buf[..n]);
        if pane.buffer.len() > BUFFER_CAP + BUFFER_SLACK {
            let excess = pane.buffer.len() - BUFFER_CAP;
            pane.buffer.drain(..excess);
        }
        let _ = state.events.send(Event::Output {
            pane: id,
            data: B64.encode(&buf[..n]),
            reset: false,
        });
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
    reap(pane);
}

/// Killing waits on the child, so it runs off the lock and off the request path.
fn reap(pane: Pane) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || terminate(pane))
}

/// Hangs up the shell's process group and the terminal's foreground group, then SIGKILLs whatever is left. An agent
/// started by an interactive shell runs in its own group, so signalling the shell alone (portable-pty's `kill`) leaves
/// it running.
fn terminate(mut pane: Pane) {
    let mut groups: Vec<libc::pid_t> = pane.child.process_id().map(|pid| pid as libc::pid_t).into_iter().collect();
    if let Some(fg) = pane.master.process_group_leader()
        && fg > 0
        && !groups.contains(&fg)
    {
        groups.push(fg);
    }
    signal_groups(&groups, libc::SIGHUP);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = pane.child.try_wait();
        if !groups.iter().any(|&g| unsafe { libc::kill(-g, 0) } == 0) {
            break;
        }
        if Instant::now() > deadline {
            signal_groups(&groups, libc::SIGKILL);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = pane.child.wait();
    drop(pane.master);
}

fn signal_groups(groups: &[libc::pid_t], signal: libc::c_int) {
    for &group in groups {
        unsafe { libc::kill(-group, signal) };
    }
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
        Request::NewTab { workspace } => new_tab(shared, workspace),
        Request::Split { pane, dir } => split(shared, pane, dir),
        Request::ClosePane { pane } => {
            close_pane(shared, pane);
            Ok(Response::Ok)
        },
        Request::Input { pane, data } => input(shared, pane, &data),
        Request::Resize { pane, cols, rows } => lock(shared)
            .pane(pane)
            .and_then(|p| {
                p.master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|e| eyre!("resizing: {e}"))
            })
            .map(|()| Response::Ok),
        Request::Subscribe => Err(eyre!("subscribe is handled by the connection")),
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
            kind: req.kind,
            cwd,
            worktree,
            status: Status::Starting,
            tabs: Vec::new(),
        },
    );
    let startup = Startup {
        agent: req.kind == Kind::Agent,
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

fn input(shared: &Shared, pane: PaneId, data: &str) -> Result<Response> {
    let bytes = B64.decode(data).wrap_err("input is not base64")?;
    let writer = lock(shared).pane(pane)?.writer.clone();
    let mut writer = writer.lock().unwrap_or_else(PoisonError::into_inner);
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(Response::Ok)
}
