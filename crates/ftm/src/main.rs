mod client;

use clap::{Parser, Subcommand, ValueEnum};
use eyre::{Result, bail};
use ftm_core::ENV_WORKSPACE_ID;
use ftm_core::model::{Kind, Status, Workspace, WorkspaceId};
use ftm_core::protocol::{Request, Response, SpawnRequest};
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
                Some(p) => std::path::absolute(p)?,
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
    };

    match client::send(&request)? {
        Response::Pong { version } => println!("ftmd {version}"),
        Response::Spawned { workspace } => println!("{}", workspace.id),
        _ => {},
    }
    Ok(())
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
        "{:indent$}{} {:?} {:?} {}  {}",
        "",
        ws.name,
        ws.kind,
        ws.status,
        location,
        ws.id,
        indent = depth * 2
    );
    for child in all.iter().filter(|c| c.parent == Some(ws.id)) {
        print_tree(child, all, depth + 1);
    }
}
