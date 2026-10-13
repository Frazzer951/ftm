use ftm_core::model::{Status, Workspace, WorkspaceId, Worktree};
use ftm_core::protocol::{Request, Response, SpawnRequest};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct State {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
}

impl State {
    pub fn handle(&mut self, request: Request) -> Response {
        match request {
            Request::Ping => Response::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Request::List => Response::Workspaces {
                workspaces: self.workspaces.values().cloned().collect(),
            },
            Request::Spawn(req) => self.spawn(req),
            Request::SetStatus { id, status } => match self.workspaces.get_mut(&id) {
                Some(ws) => {
                    ws.status = status;
                    Response::Ok
                },
                None => unknown(id),
            },
            Request::Close { id, .. } => match self.workspaces.remove(&id) {
                Some(_) => Response::Ok,
                None => unknown(id),
            },
        }
    }

    // TODO: create the worktree, run the repo setup script, and start the PTY.
    fn spawn(&mut self, req: SpawnRequest) -> Response {
        if let Some(parent) = req.parent
            && !self.workspaces.contains_key(&parent)
        {
            return unknown(parent);
        }
        let id = WorkspaceId::new_v4();
        let name = req.name.unwrap_or_else(|| {
            req.cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.to_string())
        });
        let worktree = req.worktree_branch.map(|branch| Worktree {
            repo: req.cwd.clone(),
            branch,
        });
        let workspace = Workspace {
            id,
            parent: req.parent,
            name,
            kind: req.kind,
            cwd: req.cwd,
            worktree,
            status: Status::Starting,
        };
        self.workspaces.insert(id, workspace.clone());
        Response::Spawned { workspace }
    }
}

fn unknown(id: WorkspaceId) -> Response {
    Response::Error {
        message: format!("no workspace {id}"),
    }
}
