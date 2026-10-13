//! Newline-delimited JSON over the daemon's Unix socket: one `Request` per line in, one `Response` per line out.
//! After a `Subscribe` request is answered, that connection only carries `Event` lines.

use crate::model::{Kind, PaneId, SplitDir, Status, Workspace, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    List,
    Spawn(SpawnRequest),
    SetStatus {
        id: WorkspaceId,
        status: Status,
    },
    Close {
        id: WorkspaceId,
        remove_worktree: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong {
        version: String,
    },
    Workspaces {
        workspaces: Vec<Workspace>,
    },
    Spawned {
        workspace: Workspace,
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
    /// `data` is base64. On subscribe (and after the subscriber falls behind) each live pane's buffered output
    /// arrives with `reset` set: clear the terminal before writing it.
    Output {
        pane: PaneId,
        data: String,
        #[serde(default)]
        reset: bool,
    },
}
