use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub type WorkspaceId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Terminal,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Working,
    NeedsInput,
    Idle,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub repo: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub parent: Option<WorkspaceId>,
    pub name: String,
    pub kind: Kind,
    pub cwd: PathBuf,
    pub worktree: Option<Worktree>,
    pub status: Status,
}
