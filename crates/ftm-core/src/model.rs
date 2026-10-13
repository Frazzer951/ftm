use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub type WorkspaceId = Uuid;
pub type TabId = Uuid;
pub type PaneId = Uuid;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Side by side.
    Row,
    /// Stacked.
    Column,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    Pane {
        id: PaneId,
    },
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Layout::Pane { id } => *id == pane,
            Layout::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        match self {
            Layout::Pane { id } => vec![*id],
            Layout::Split { first, second, .. } => {
                let mut v = first.panes();
                v.extend(second.panes());
                v
            },
        }
    }

    /// Replaces `target` with a split of `target` and `new`.
    pub fn split(&mut self, target: PaneId, new: PaneId, dir: SplitDir) -> bool {
        match self {
            Layout::Pane { id } if *id == target => {
                *self = Layout::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(Layout::Pane { id: target }),
                    second: Box::new(Layout::Pane { id: new }),
                };
                true
            },
            Layout::Pane { .. } => false,
            Layout::Split { first, second, .. } => first.split(target, new, dir) || second.split(target, new, dir),
        }
    }

    /// Removes `pane`, collapsing its parent split. Returns `None` when the layout becomes empty.
    pub fn remove(self, pane: PaneId) -> Option<Layout> {
        match self {
            Layout::Pane { id } if id == pane => None,
            Layout::Pane { .. } => Some(self),
            Layout::Split {
                dir,
                ratio,
                first,
                second,
            } => match (first.remove(pane), second.remove(pane)) {
                (Some(a), Some(b)) => Some(Layout::Split {
                    dir,
                    ratio,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub layout: Layout,
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
    pub tabs: Vec<Tab>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_then_remove_collapses() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut layout = Layout::Pane { id: a };
        assert!(layout.split(a, b, SplitDir::Row));
        assert!(layout.split(b, c, SplitDir::Column));
        assert_eq!(layout.panes(), vec![a, b, c]);

        let layout = layout.remove(b).unwrap();
        assert_eq!(layout.panes(), vec![a, c]);
        let layout = layout.remove(a).unwrap();
        assert!(matches!(layout, Layout::Pane { id } if id == c));
        assert!(layout.remove(c).is_none());
    }
}
