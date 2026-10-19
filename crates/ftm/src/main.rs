mod attach;
mod client;
mod gc;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use clap::{Parser, Subcommand, ValueEnum};
use eyre::{Result, WrapErr, bail};
use ftm_core::ENV_WORKSPACE_ID;
use ftm_core::model::{Kind, PaneId, RANDOM_COLOR, Status, WORKSPACE_COLORS, Workspace, WorkspaceId, workspace_color};
use ftm_core::protocol::{FEATURE_CLAUDE_ARGS, FEATURE_SPAWN_COLOR, Request, Response, SpawnRequest};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "Frazzer's Terminal Manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that ftmd is running; prints its version and, when it reports any, its features
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
        /// Sidebar color to start with, as `ftm color` takes it: red, orange, yellow, green, cyan, blue, purple, pink,
        /// gray, random or #rrggbb. Without it, a workspace under a colored parent gets a color near the parent's
        #[arg(long)]
        color: Option<String>,
        /// Extra arguments for Claude, after `--` (e.g. `-- --model <id> --permission-mode auto`, or `-- --resume
        /// <session id>`). Each is passed as its own argument after `-n <name>` and before the brief; agents only. Refused when ftmd is too old to pass them on.
        #[arg(last = true, value_name = "CLAUDE_ARGS")]
        claude_args: Vec<String>,
    },
    /// Report agent status (called from Claude Code hooks)
    Status {
        #[arg(value_enum)]
        status: StatusArg,
        #[arg(long, env = ENV_WORKSPACE_ID)]
        id: WorkspaceId,
        /// Read the Claude Code hook's JSON input from stdin
        #[arg(long)]
        hook: bool,
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
    /// Pin a top-level workspace to the sidebar's Pinned section
    Pin {
        #[arg(env = ENV_WORKSPACE_ID)]
        id: WorkspaceId,
    },
    /// Remove a workspace from the sidebar's Pinned section
    Unpin {
        #[arg(env = ENV_WORKSPACE_ID)]
        id: WorkspaceId,
    },
    /// Color the workspace's sidebar row: red, orange, yellow, green, cyan, blue, purple, pink, gray, #rrggbb, or
    /// random for a random #rrggbb; --clear removes it
    Color {
        #[arg(required_unless_present = "clear")]
        color: Option<String>,
        #[arg(long, conflicts_with = "color")]
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
    /// List FTM worktrees whose branch is merged into the repo's base; --yes removes them and their branches
    Gc {
        /// Remove them; without it, only list
        #[arg(long)]
        yes: bool,
        /// Use the last fetch of the base instead of fetching it
        #[arg(long)]
        no_fetch: bool,
        /// Also remove worktrees whose upstream branch was deleted after everything was pushed, which may be a PR
        /// closed unmerged. Seeing the deletion needs a fetch, so not with --no-fetch
        #[arg(long, conflicts_with = "no_fetch")]
        include_upstream_deleted: bool,
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
            color,
            claude_args,
        } => {
            let cwd = match path {
                Some(p) => std::fs::canonicalize(&p).wrap_err_with(|| format!("{}", p.display()))?,
                None => std::env::current_dir()?,
            };
            if brief.is_some() && matches!(kind, KindArg::Terminal) {
                bail!("--brief only applies to agents");
            }
            if !claude_args.is_empty() {
                if matches!(kind, KindArg::Terminal) {
                    bail!("claude arguments only apply to agents");
                }
                // An older ftmd would drop them and start a plain agent, which for `--resume` means a new session.
                require_feature(FEATURE_CLAUDE_ARGS, "claude arguments")?;
            }
            let color = color
                .map(|c| {
                    if c.trim().eq_ignore_ascii_case(RANDOM_COLOR) {
                        return Ok(RANDOM_COLOR.to_string());
                    }
                    workspace_color(&c).ok_or_else(|| {
                        eyre::eyre!(
                            "unknown color {c:?}: use {}, {RANDOM_COLOR} or #rrggbb",
                            WORKSPACE_COLORS.join(", ")
                        )
                    })
                })
                .transpose()?;
            if color.is_some() {
                require_feature(FEATURE_SPAWN_COLOR, "--color")?;
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
                brief: brief
                    .map(|p| std::fs::read_to_string(&p).wrap_err_with(|| format!("reading {}", p.display())))
                    .transpose()?,
                command: None,
                claude_args,
                color,
            })
        },
        Command::Status { status, id, hook } => Request::SetStatus {
            id,
            background_agents: if hook {
                running_subagents(&std::io::read_to_string(std::io::stdin()).unwrap_or_default())
            } else {
                0
            },
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
        Command::Pin { id } => Request::Pin { id, pinned: true },
        Command::Unpin { id } => Request::Pin { id, pinned: false },
        Command::Color { color, id, .. } => Request::SetColor {
            id: id
                .ok_or_else(|| eyre::eyre!("no workspace: pass --id or run inside an FTM workspace (${ENV_WORKSPACE_ID})"))?,
            color,
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
        Command::Gc {
            yes,
            no_fetch,
            include_upstream_deleted,
        } => return gc::run(yes, !no_fetch, include_upstream_deleted),
        Command::Send { target, text, enter } => return send(resolve_pane(target)?, &text, enter),
    };

    match client::send(&request)? {
        Response::Pong { version, features } if features.is_empty() => println!("ftmd {version}"),
        Response::Pong { version, features } => println!("ftmd {version} (features: {})", features.join(", ")),
        Response::Spawned { workspace } => println!("{}", workspace.id),
        _ => {},
    }
    Ok(())
}

/// Claude Code takes a longer burst of input as a paste, CR included, so Enter goes in its own write once the TUI has
/// had time to take in the text.
const ENTER_DELAY: Duration = Duration::from_millis(100);

fn send(pane: PaneId, text: &str, enter: bool) -> Result<()> {
    let input = |data: &str| {
        client::send(&Request::Input {
            pane,
            data: B64.encode(data),
        })
    };
    if !text.is_empty() {
        input(text)?;
        if enter {
            std::thread::sleep(ENTER_DELAY);
        }
    }
    if enter {
        input("\r")?;
    }
    Ok(())
}

/// Background subagents still running, from a Claude Code hook's input. Background shells never count: they can run
/// for good (a dev server, a log watcher).
fn running_subagents(hook_input: &str) -> u32 {
    let input: serde_json::Value = serde_json::from_str(hook_input).unwrap_or_default();
    let running = input["background_tasks"].as_array().map_or(0, |tasks| {
        tasks
            .iter()
            .filter(|t| t["type"] == "subagent" && t["status"] == "running")
            .count()
    });
    u32::try_from(running).unwrap_or(u32::MAX)
}

/// Refuses when ftmd lacks `feature`, which an older one would otherwise silently drop.
fn require_feature(feature: &str, what: &str) -> Result<()> {
    let Response::Pong { version, features } = client::send(&Request::Ping)? else {
        bail!("unexpected response from ftmd");
    };
    if !features.iter().any(|f| f == feature) {
        bail!("ftmd {version} does not support {what}; restart it on this build of ftm");
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
        "{:indent$}{} {:?} {:?}{}{}{} {}  {}",
        "",
        match ws.display_name() {
            shown if shown != ws.name => format!("{shown} ({})", ws.name),
            _ => ws.name.clone(),
        },
        ws.kind,
        ws.status,
        if ws.unseen { " (unseen)" } else { "" },
        if ws.pinned { " (pinned)" } else { "" },
        ws.color.as_ref().map(|c| format!(" (color {c})")).unwrap_or_default(),
        location,
        ws.id,
        indent = depth * 2
    );
    for child in all.iter().filter(|c| c.parent == Some(ws.id)) {
        print_tree(child, all, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::running_subagents;

    #[test]
    fn counts_running_background_subagents_only() {
        let input = r#"{"background_tasks":[{"type":"subagent","status":"running"},{"type":"subagent","status":"running"},
            {"type":"subagent","status":"completed"},{"type":"shell","status":"running"}]}"#;
        assert_eq!(running_subagents(input), 2);
        assert_eq!(running_subagents(r#"{"background_tasks":[]}"#), 0);
        assert_eq!(running_subagents(r#"{"hook_event_name":"Stop"}"#), 0);
        assert_eq!(running_subagents(""), 0);
    }
}
