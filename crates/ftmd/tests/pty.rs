mod common;

use common::{Conn, alive, git, start_daemon, temp_root, wait_for, wait_for_output, wait_until};
use ftm_core::model::{Kind, Layout, SplitDir};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use std::time::Duration;

#[test]
fn pty_streams_and_accepts_input() {
    let dir = temp_root("pty");
    let daemon = start_daemon("pty", |c| {
        c.env("XDG_DATA_HOME", &dir);
    });
    let mut req = Conn::open(&daemon.socket);

    let Response::Spawned { workspace } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: Some("t".into()),
        parent: None,
        cwd: dir.clone(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
    })) else {
        panic!("expected spawned")
    };
    let Layout::Pane { id: pane } = workspace.tabs[0].layout else {
        panic!("expected one pane")
    };

    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));
    assert!(matches!(events.line(), Event::Workspaces { .. }));

    req.type_in(pane, "echo \"ftm-$((40+2)) $FTM_PANE_ID $FTM_NAME\"; pwd\n");
    wait_for_output(&mut events, pane, &format!("ftm-42 {pane} t"));
    assert!(matches!(
        req.send(&Request::Resize {
            pane,
            cols: 120,
            rows: 40
        }),
        Response::Ok
    ));

    // A late subscriber gets the buffered output replayed.
    let mut late = Conn::open(&daemon.socket);
    late.send(&Request::Subscribe);
    wait_for_output(&mut late, pane, &format!("ftm-42 {pane} t"));

    let Response::Pane { pane: right } = req.send(&Request::Split {
        pane,
        dir: SplitDir::Row,
    }) else {
        panic!()
    };
    let Response::Pane { pane: tab2 } = req.send(&Request::NewTab { workspace: workspace.id }) else {
        panic!()
    };
    let ws = &req.list()[0];
    assert_eq!(ws.tabs.len(), 2);
    assert_eq!(ws.tabs[0].layout.panes(), vec![pane, right]);

    req.type_in(right, "exit\n");
    wait_until(&mut req, "split pane to exit", |w| w[0].tabs[0].layout.panes() == vec![pane]);
    req.send(&Request::ClosePane { pane: tab2 });
    req.send(&Request::ClosePane { pane: tab2 });
    assert_eq!(req.list()[0].tabs.len(), 1);

    req.send(&Request::Close {
        id: workspace.id,
        remove_worktree: false,
    });
    assert!(req.list().is_empty());
}

/// An agent runs as the foreground job of an interactive shell, in its own process group, and Claude Code ignores
/// SIGHUP. Closing with `remove_worktree` must still stop it before deleting the worktree, then drop the merged branch
/// and the empty `prefix/` directory.
#[test]
fn close_kills_foreground_job_and_removes_worktree() {
    let root = temp_root("wt");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "a").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);

    let daemon = start_daemon("wt", |c| {
        c.env("XDG_DATA_HOME", root.join("data"));
    });
    let mut req = Conn::open(&daemon.socket);
    let Response::Spawned { workspace } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: None,
        parent: None,
        cwd: repo.clone(),
        worktree_branch: Some("chore/wt".into()),
        base: None,
        copy_changes: false,
        brief: None,
    })) else {
        panic!("expected spawned")
    };
    let wt = workspace.cwd.clone();
    assert_eq!(wt, root.join("data/ftm/worktrees/repo/chore/wt"));
    let Layout::Pane { id: pane } = workspace.tabs[0].layout else {
        panic!("expected one pane")
    };

    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    req.type_in(pane, "sh -c 'trap \"\" HUP; echo PID=$$.; exec sleep 600'\n");
    let out = wait_for_output(&mut events, pane, ".\r\n");
    let pid: i32 = out.split("PID=").last().unwrap().split('.').next().unwrap().parse().unwrap();
    assert!(alive(pid));

    req.send(&Request::Close {
        id: workspace.id,
        remove_worktree: true,
    });
    wait_for(&format!("foreground job {pid} to die"), Duration::from_secs(10), || {
        !alive(pid)
    });
    assert!(!wt.exists());
    assert!(!root.join("data/ftm/worktrees/repo/chore").exists());
    assert!(git(&repo, &["branch", "--list", "chore/wt"]).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
