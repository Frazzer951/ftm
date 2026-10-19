//! Newline-delimited JSON over the daemon's Unix socket: one `Request` per line in, one `Response` per line out.
//! After a `Subscribe` request is answered, that connection only carries `Event` lines.

use crate::model::{Folder, Kind, PaneId, SplitDir, Status, Workspace, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    List,
    Spawn(SpawnRequest),
    SetStatus {
        id: WorkspaceId,
        status: Status,
        /// Background subagents the agent still has running (from Claude Code's `Stop` hook input).
        #[serde(default)]
        background_agents: u32,
    },
    Close {
        id: WorkspaceId,
        remove_worktree: bool,
    },
    /// Changes the workspace's displayed title; its `name` (and `FTM_NAME`) stay as spawned. An empty `name` clears the
    /// title; one that is only whitespace is rejected.
    Rename {
        id: WorkspaceId,
        name: String,
    },
    Pin {
        id: WorkspaceId,
        pinned: bool,
    },
    /// Sets the workspace's sidebar color, a name from `WORKSPACE_COLORS` or `#rrggbb`; `None` clears it.
    SetColor {
        id: WorkspaceId,
        #[serde(default)]
        color: Option<String>,
    },
    /// Clears the workspace's `unseen` flag: someone has looked at it.
    MarkSeen {
        id: WorkspaceId,
    },
    /// Remembered folders, most recently used first, as `Response::Folders`.
    Folders,
    /// Forgets a folder; its workspaces are untouched.
    RemoveFolder {
        path: PathBuf,
    },
    /// Sets the sidebar order of the remembered folders. Paths it leaves out keep their place after the listed ones.
    ReorderFolders {
        paths: Vec<PathBuf>,
    },
    /// Sets the sidebar order of the workspaces, which `List` and `Event::Workspaces` then follow. Ids it leaves out
    /// keep their order after the listed ones.
    ReorderWorkspaces {
        ids: Vec<WorkspaceId>,
    },
    NewTab {
        workspace: WorkspaceId,
    },
    Split {
        pane: PaneId,
        dir: SplitDir,
    },
    ClosePane {
        pane: PaneId,
    },
    /// `data` is base64.
    Input {
        pane: PaneId,
        data: String,
    },
    Resize {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    /// The pane's screen and scrollback as plain text, or with `raw` its buffered output bytes, as `Response::Output`.
    Capture {
        pane: PaneId,
        #[serde(default)]
        raw: bool,
    },
    Subscribe,
    /// For `ftm attach`: after `Ok`, the connection carries only `Event::Output` for this pane, starting with a redraw
    /// of its current screen (`reset` set), and ends when the pane does.
    Attach {
        pane: PaneId,
    },
}

impl Request {
    /// How long a client waits for ftmd to answer, so a hung ftmd never blocks it forever. Making a worktree copies
    /// files and removing one waits for its panes to exit, so those get longer.
    pub fn reply_timeout(&self) -> Duration {
        match self {
            Request::Spawn(spawn) if spawn.worktree_branch.is_some() => Duration::from_secs(600),
            Request::Close {
                remove_worktree: true, ..
            } => Duration::from_secs(120),
            _ => Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub kind: Kind,
    pub name: Option<String>,
    pub parent: Option<WorkspaceId>,
    pub cwd: PathBuf,
    pub worktree_branch: Option<String>,
    pub base: Option<String>,
    pub copy_changes: bool,
    pub brief: Option<String>,
    /// For a terminal: a POSIX sh script its first pane runs before handing over to a login shell. An ftmd from
    /// before the field ignores it and opens a plain shell.
    #[serde(default)]
    pub command: Option<String>,
    /// For an agent: extra arguments for Claude, each passed as its own argv entry after `-n <name>` and before the
    /// brief. An ftmd from before the field ignores them and starts the agent without.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claude_args: Vec<String>,
    /// The sidebar color to start with, as `Request::SetColor` takes it. An ftmd from before the field ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// `SpawnRequest.claude_args` is honoured.
pub const FEATURE_CLAUDE_ARGS: &str = "claude_args";
/// `SpawnRequest.color` is honoured.
pub const FEATURE_SPAWN_COLOR: &str = "spawn_color";
/// `Request::SetColor` and `Workspace.color`.
pub const FEATURE_WORKSPACE_COLORS: &str = "workspace_colors";
/// `Request::ReorderWorkspaces`, and `List`/`Event::Workspaces` in that order.
pub const FEATURE_WORKSPACE_ORDER: &str = "workspace_order";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong {
        version: String,
        /// What this ftmd supports beyond the base protocol (see `FEATURE_*`), so a client can refuse a request an
        /// older ftmd would quietly ignore part of. Empty from daemons that predate it.
        #[serde(default)]
        features: Vec<String>,
    },
    Workspaces {
        workspaces: Vec<Workspace>,
    },
    Folders {
        folders: Vec<Folder>,
        /// Every folder's path in sidebar order; empty from daemons that predate it.
        #[serde(default)]
        order: Vec<PathBuf>,
    },
    Spawned {
        workspace: Box<Workspace>,
    },
    Pane {
        pane: PaneId,
    },
    /// `data` is base64.
    Output {
        data: String,
    },
    Ok,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Full tree, sent on subscribe and after every change.
    Workspaces { workspaces: Vec<Workspace> },
    /// Remembered folders, most recently used first; sent on subscribe and after every change.
    /// `order` is every folder's path in sidebar order; empty from daemons that predate it.
    Folders {
        folders: Vec<Folder>,
        #[serde(default)]
        order: Vec<PathBuf>,
    },
    /// `data` is base64. On subscribe (and after the subscriber falls behind) each live pane's buffered output
    /// arrives with `reset` set: clear the terminal before writing it.
    Output {
        pane: PaneId,
        data: String,
        #[serde(default)]
        reset: bool,
    },
    /// ftmd's binary was replaced, but ftmd cannot move onto it without losing the running panes (the new build
    /// speaks another holder protocol, or does not start). Sent on subscribe until ftmd restarts. `version` is the
    /// new build's, when it could say.
    UpdateBlocked { version: Option<String>, reason: String },
    /// An event from a newer ftmd. Never sent.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_slow_requests_wait_long() {
        let spawn = |worktree_branch: Option<&str>| {
            Request::Spawn(SpawnRequest {
                kind: Kind::Agent,
                name: None,
                parent: None,
                cwd: "/".into(),
                worktree_branch: worktree_branch.map(Into::into),
                base: None,
                copy_changes: false,
                brief: None,
                command: None,
                claude_args: Vec::new(),
                color: None,
            })
        };
        let close = |remove_worktree| Request::Close {
            id: WorkspaceId::nil(),
            remove_worktree,
        };
        let secs = |request: Request| request.reply_timeout().as_secs();
        assert_eq!(secs(Request::Ping), 30);
        assert_eq!(
            secs(Request::Input {
                pane: PaneId::nil(),
                data: String::new()
            }),
            30
        );
        assert_eq!(secs(spawn(None)), 30);
        assert_eq!(secs(close(false)), 30);
        assert_eq!(secs(spawn(Some("b"))), 600);
        assert_eq!(secs(close(true)), 120);
    }

    /// Old and new clients and daemons talk to each other across an upgrade, so fields a side does not know are
    /// ignored and fields it lacks default.
    #[test]
    fn spawn_tolerates_fields_across_versions() {
        let old = r#"{"op":"spawn","kind":"terminal","name":null,"parent":null,"cwd":"/","worktree_branch":null,"base":null,"copy_changes":false,"brief":null}"#;
        let Request::Spawn(req) = serde_json::from_str(old).unwrap() else {
            panic!()
        };
        assert_eq!(req.command, None);
        assert!(req.claude_args.is_empty());
        assert_eq!(req.color, None);
        // Without extra arguments or a color the request is what an older ftmd expects.
        let json = serde_json::to_string(&Request::Spawn(req)).unwrap();
        assert!(!json.contains("claude_args") && !json.contains("color"), "{json}");
        let newer = old.replace(
            r#""brief":null"#,
            r##""brief":null,"command":"true","claude_args":["--model","m"],"color":"#a0b1c2","future":1"##,
        );
        let Request::Spawn(req) = serde_json::from_str(&newer).unwrap() else {
            panic!()
        };
        assert_eq!(req.command.as_deref(), Some("true"));
        assert_eq!(req.claude_args, ["--model", "m"]);
        assert_eq!(req.color.as_deref(), Some("#a0b1c2"));
    }

    /// An app keeps running across a hot upgrade of ftmd, so it must survive events, kinds and statuses added since.
    #[test]
    fn events_from_a_newer_ftmd_still_parse() {
        let event: Event = serde_json::from_str(r#"{"event":"weather","sunny":true}"#).unwrap();
        assert!(matches!(event, Event::Unknown));
        let ws = r#"{"id":"00000000-0000-0000-0000-000000000001","parent":null,"name":"n","kind":"browser","cwd":"/","worktree":null,"status":"pondering","tabs":[]}"#;
        let Event::Workspaces { workspaces } =
            serde_json::from_str(&format!(r#"{{"event":"workspaces","workspaces":[{ws}]}}"#)).unwrap()
        else {
            panic!()
        };
        assert_eq!((workspaces[0].kind, workspaces[0].status), (Kind::Unknown, Status::Unknown));
    }

    #[test]
    fn folders_without_an_order_still_parse() {
        let old = r#"{"event":"folders","folders":[{"path":"/a","last_used":1}]}"#;
        let Event::Folders { folders, order } = serde_json::from_str(old).unwrap() else {
            panic!()
        };
        assert_eq!(folders.len(), 1);
        assert!(order.is_empty());
        let req: Request = serde_json::from_str(r#"{"op":"reorder_folders","paths":["/b","/a"]}"#).unwrap();
        assert!(matches!(req, Request::ReorderFolders { paths } if paths.len() == 2));
    }
}
