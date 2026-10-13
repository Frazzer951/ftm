//! Newline-delimited JSON over the daemon's Unix socket: one `Request` per line in, one `Response` per line out.

use crate::model::{Kind, Status, Workspace, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    List,
    Spawn(SpawnRequest),
    SetStatus { id: WorkspaceId, status: Status },
    Close { id: WorkspaceId, remove_worktree: bool },
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
    Pong { version: String },
    Workspaces { workspaces: Vec<Workspace> },
    Spawned { workspace: Workspace },
    Ok,
    Error { message: String },
}
