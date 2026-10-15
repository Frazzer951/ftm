//! Newline-delimited JSON over the daemon's Unix socket: one `Request` per line in, one `Response` per line out.
//! After a `Subscribe` request is answered, that connection only carries `Event` lines.

use crate::model::{Folder, Kind, PaneId, SplitDir, Status, Workspace, WorkspaceId};
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
    Folders {
        folders: Vec<Folder>,
        /// Every folder's path in sidebar order; empty from daemons that predate it.
        #[serde(default)]
        order: Vec<PathBuf>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Old and new clients and daemons talk to each other across an upgrade, so fields a side does not know are
    /// ignored and fields it lacks default.
    #[test]
    fn spawn_tolerates_fields_across_versions() {
        let old = r#"{"op":"spawn","kind":"terminal","name":null,"parent":null,"cwd":"/","worktree_branch":null,"base":null,"copy_changes":false,"brief":null}"#;
        let Request::Spawn(req) = serde_json::from_str(old).unwrap() else {
            panic!()
        };
        assert_eq!(req.command, None);
        let newer = old.replace(r#""brief":null"#, r#""brief":null,"command":"true","future":1"#);
        let Request::Spawn(req) = serde_json::from_str(&newer).unwrap() else {
            panic!()
        };
        assert_eq!(req.command.as_deref(), Some("true"));
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
