mod attach;
mod client;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use clap::{Parser, Subcommand, ValueEnum};
use eyre::{Result, WrapErr, bail};
use ftm_core::ENV_WORKSPACE_ID;
use ftm_core::model::{Kind, PaneId, Status, Workspace, WorkspaceId};
use ftm_core::protocol::{Request, Response, SpawnRequest};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Frazzer's Terminal Manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that ftmd is running
    Ping,
    /// List workspaces as a parent/child tree
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Open a terminal or agent workspace
    Spawn {
        #[arg(value_enum)]
        kind: KindArg,
        /// Folder or repo to open (defaults to the current directory)
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        /// Create a git worktree on this branch instead of using the folder directly
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long, requires = "worktree")]
        base: Option<String>,
        /// Carry staged and unstaged changes from the source checkout into the worktree
        #[arg(long, requires = "worktree")]
        copy_changes: bool,
        /// File whose contents become the agent's first prompt
        #[arg(long)]
        brief: Option<PathBuf>,
        /// Parent workspace; defaults to the workspace this command runs in
        #[arg(long, env = ENV_WORKSPACE_ID)]
        parent: Option<WorkspaceId>,
        /// Open at the top level even when run inside a workspace (overrides --parent)
        #[arg(long)]
        detached: bool,
    },
    /// Report agent status (called from Claude Code hooks)
    Status {
        #[arg(value_enum)]
        status: StatusArg,
        #[arg(long, env = ENV_WORKSPACE_ID)]
        id: WorkspaceId,
    },
    /// Close a workspace
    Close {
        #[arg(long, env = ENV_WORKSPACE_ID)]
        id: WorkspaceId,
        #[arg(long)]
        remove_worktree: bool,
    },
    /// Set the workspace's displayed name (its FTM_NAME and agent session name stay as spawned); "" or --clear
    /// goes back to the spawn name
    Rename {
        #[arg(required_unless_present = "clear")]
        name: Option<String>,
        #[arg(long, conflicts_with = "name")]
        clear: bool,
        #[arg(long, env = ENV_WORKSPACE_ID)]
        id: Option<WorkspaceId>,
    },
    /// Print a pane's screen and scrollback as text, or with --raw its buffered output bytes (last 1 MiB)
    Capture {
        /// Pane id, or a workspace id for the first pane of its first tab
        target: PaneId,
        #[arg(long)]
        raw: bool,
    },
    /// Attach this terminal to a pane; detach with Ctrl+] then d
    Attach {
        /// Pane id, or a workspace id for the first pane of its first tab
        target: PaneId,
    },
    /// Type text into a pane
    Send {
        /// Pane id, or a workspace id for the first pane of its first tab
        target: PaneId,
        text: String,
        /// Press Enter (CR) after the text
        #[arg(long)]
        enter: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum KindArg {
    Terminal,
    Agent,
}

#[derive(Clone, Copy, ValueEnum)]
enum StatusArg {
    Working,
    NeedsInput,
    Idle,
    Done,
}

fn main() -> Result<()> {
    let request = match Cli::parse().command {
        Command::Ping => Request::Ping,
        Command::Ls { json } => return list(json),
        Command::Spawn {
            kind,
            path,
            name,
            worktree,
            base,
            copy_changes,
            brief,
            parent,
            detached,
        } => {
            let cwd = match path {
                Some(p) => std::fs::canonicalize(&p).wrap_err_with(|| format!("{}", p.display()))?,
                None => std::env::current_dir()?,
            };
            if brief.is_some() && matches!(kind, KindArg::Terminal) {
                bail!("--brief only applies to agents");
            }
            Request::Spawn(SpawnRequest {
                kind: match kind {
                    KindArg::Terminal => Kind::Terminal,
                    KindArg::Agent => Kind::Agent,
                },
                name,
                parent: if detached { None } else { parent },
                cwd,
                worktree_branch: worktree,
                base,
                copy_changes,
                brief: brief.map(std::fs::read_to_string).transpose()?,
            })
        },
        Command::Status { status, id } => Request::SetStatus {
            id,
            status: match status {
                StatusArg::Working => Status::Working,
                StatusArg::NeedsInput => Status::NeedsInput,
                StatusArg::Idle => Status::Idle,
                StatusArg::Done => Status::Done,
            },
        },
        Command::Close { id, remove_worktree } => Request::Close { id, remove_worktree },
        Command::Rename { name, id, .. } => Request::Rename {
            id: id
                .ok_or_else(|| eyre::eyre!("no workspace: pass --id or run inside an FTM workspace (${ENV_WORKSPACE_ID})"))?,
            name: name.unwrap_or_default(),
        },
        Command::Capture { target, raw } => {
            let pane = resolve_pane(target)?;
            let Response::Output { data } = client::send(&Request::Capture { pane, raw })? else {
                bail!("unexpected response from ftmd");
            };
            std::io::stdout().lock().write_all(&B64.decode(data)?)?;
            return Ok(());
        },
        Command::Attach { target } => return attach::run(resolve_pane(target)?),
        Command::Send { target, text, enter } => {
            let text = if enter { text + "\r" } else { text };
            Request::Input {
                pane: resolve_pane(target)?,
                data: B64.encode(text),
            }
        },
    };

    match client::send(&request)? {
        Response::Pong { version } => println!("ftmd {version}"),
        Response::Spawned { workspace } => println!("{}", workspace.id),
        _ => {},
    }
    Ok(())
}

fn resolve_pane(target: PaneId) -> Result<PaneId> {
    let Response::Workspaces { workspaces } = client::send(&Request::List)? else {
        bail!("unexpected response from ftmd");
    };
    match workspaces.iter().find(|w| w.id == target) {
        Some(ws) => match ws.tabs.first() {
            Some(tab) => Ok(tab.layout.panes()[0]),
            None => bail!("workspace {target} has no panes"),
        },
        None => Ok(target),
    }
}

fn list(json: bool) -> Result<()> {
    let Response::Workspaces { workspaces } = client::send(&Request::List)? else {
        bail!("unexpected response from ftmd");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&workspaces)?);
        return Ok(());
    }
    for root in workspaces.iter().filter(|w| w.parent.is_none()) {
        print_tree(root, &workspaces, 0);
    }
    Ok(())
}

fn print_tree(ws: &Workspace, all: &[Workspace], depth: usize) {
    let location = match &ws.worktree {
        Some(wt) => format!("{} @ {}", ws.cwd.display(), wt.branch),
        None => ws.cwd.display().to_string(),
    };
    println!(
        "{:indent$}{} {:?} {:?}{} {}  {}",
        "",
        match ws.display_name() {
            shown if shown != ws.name => format!("{shown} ({})", ws.name),
            _ => ws.name.clone(),
        },
        ws.kind,
        ws.status,
        if ws.unseen { " (unseen)" } else { "" },
        location,
        ws.id,
        indent = depth * 2
    );
    for child in all.iter().filter(|c| c.parent == Some(ws.id)) {
        print_tree(child, all, depth + 1);
    }
}
