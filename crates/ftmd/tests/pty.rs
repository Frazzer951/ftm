mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, alive, ftm_bin, git, start_daemon, temp_root, wait_for, wait_for_output, wait_until};
use ftm_core::model::{Kind, Layout, SplitDir};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use std::process::Command;
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
        command: None,
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

    // Capture renders the screen plus scrollback as text; raw returns the output bytes.
    req.type_in(pane, "seq 1 60 | sed 's/^/n/'; echo end-$((1+1))\n");
    wait_for_output(&mut events, pane, "end-2");
    let mut capture = |raw| match req.send(&Request::Capture { pane, raw }) {
        Response::Output { data } => String::from_utf8(B64.decode(data).unwrap()).unwrap(),
        r => panic!("{r:?}"),
    };
    let text = capture(false);
    assert!(text.lines().any(|l| l == "n1"), "scrollback lost: {text}");
    assert!(text.lines().any(|l| l == "n60") && text.contains("end-2"));
    assert!(!text.contains('\x1b') && !text.contains('\r'));
    assert!(capture(true).contains("n60\r\n"));

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
        command: None,
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

/// A relaunched app resizes to the size the pane already has, which on its own would not make a TUI repaint over the
/// replay it drew at another size. The pane gets a narrower size and then its own back, each a real SIGWINCH.
#[test]
fn resize_to_the_same_size_makes_the_pane_repaint() {
    let dir = temp_root("redraw");
    let daemon = start_daemon("redraw", |c| {
        c.env("XDG_DATA_HOME", &dir);
    });
    let mut req = Conn::open(&daemon.socket);
    let Response::Spawned { workspace } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: None,
        parent: None,
        cwd: dir.clone(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: None,
    })) else {
        panic!("expected spawned")
    };
    let Layout::Pane { id: pane } = workspace.tabs[0].layout else {
        panic!("expected one pane")
    };
    let mut events = Conn::open(&daemon.socket);
    events.send(&Request::Subscribe);
    let resize = |req: &mut Conn| {
        assert!(matches!(
            req.send(&Request::Resize {
                pane,
                cols: 120,
                rows: 40
            }),
            Response::Ok
        ));
    };
    resize(&mut req);

    req.type_in(
        pane,
        "sh -c 'trap \"echo w\\$(stty size)w\" WINCH; echo ready-$((1+1)); while :; do sleep 0.02; done'\n",
    );
    wait_for_output(&mut events, pane, "ready-2");
    resize(&mut req);
    let out = wait_for_output(&mut events, pane, "w40 120w");
    assert!(out.contains("w40 119w"), "no narrower size first: {out:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Enter has to reach the pane in a read of its own, or Claude Code takes it as part of a pasted burst.
#[test]
fn send_enter_presses_enter_in_its_own_read() {
    let dir = temp_root("send-enter");
    let daemon = start_daemon("send-enter", |c| {
        c.env("XDG_DATA_HOME", &dir);
    });
    let mut req = Conn::open(&daemon.socket);
    let Response::Spawned { workspace } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: None,
        parent: None,
        cwd: dir.clone(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: None,
    })) else {
        panic!("expected spawned")
    };
    let Layout::Pane { id: pane } = workspace.tabs[0].layout else {
        panic!("expected one pane")
    };

    // Saves each read the pane's program gets as reads/<n>.
    std::fs::write(
        dir.join("record.sh"),
        "mkdir reads; stty raw -echo; touch ready; n=0\n\
         while :; do dd bs=4096 count=1 of=part 2>/dev/null; n=$((n+1)); mv part reads/$n; done\n",
    )
    .unwrap();
    req.type_in(pane, "sh record.sh\n");
    wait_for("recorder", Duration::from_secs(10), || dir.join("ready").exists());

    let bin = ftm_bin();
    let send = |args: &[&str]| {
        let out = Command::new(&bin)
            .arg("send")
            .arg(workspace.id.to_string())
            .args(args)
            .env("FTM_SOCKET", &daemon.socket)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ftm send {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let reads = |n: usize| {
        wait_for("reads", Duration::from_secs(10), || dir.join(format!("reads/{n}")).exists());
        (1..=n)
            .map(|i| std::fs::read_to_string(dir.join(format!("reads/{i}"))).unwrap())
            .collect::<Vec<_>>()
    };

    send(&["hello world", "--enter"]);
    assert_eq!(reads(2), ["hello world", "\r"]);
    send(&["", "--enter"]);
    assert_eq!(reads(3), ["hello world", "\r", "\r"]);
    send(&["no enter"]);
    assert_eq!(reads(4)[3], "no enter");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A terminal's command runs under `sh` whatever the login shell, then the pane stays open on a shell.
#[test]
fn terminal_command_runs_then_leaves_a_shell() {
    let dir = temp_root("cmd");
    let daemon = start_daemon("cmd", |c| {
        c.env("XDG_DATA_HOME", &dir);
    });
    let mut req = Conn::open(&daemon.socket);
    let mut events = Conn::open(&daemon.socket);
    assert!(matches!(events.send(&Request::Subscribe), Response::Ok));

    let Response::Spawned { workspace } = req.send(&Request::Spawn(SpawnRequest {
        kind: Kind::Terminal,
        name: Some("update".into()),
        parent: None,
        cwd: dir.clone(),
        worktree_branch: None,
        base: None,
        copy_changes: false,
        brief: None,
        command: Some("d=$(echo cmd); echo \"ran-$d-$((1+1))\"".into()),
    })) else {
        panic!("expected spawned")
    };
    let Layout::Pane { id: pane } = workspace.tabs[0].layout else {
        panic!("expected one pane")
    };
    wait_for_output(&mut events, pane, "ran-cmd-2");
    req.type_in(pane, "echo after-$((2+1))\n");
    wait_for_output(&mut events, pane, "after-3");

    let Response::Folders { folders, .. } = req.send(&Request::Folders) else {
        panic!("expected folders")
    };
    assert!(folders.is_empty(), "{folders:?}");
}
