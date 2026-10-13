use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ftm_core::model::{Kind, Layout, PaneId, SplitDir, Workspace};
use ftm_core::protocol::{Event, Request, Response, SpawnRequest};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// macOS caps socket paths at ~104 bytes, so this stays in /tmp rather than the (long) per-user temp dir.
fn start_daemon(name: &str, data_home: &Path) -> Daemon {
    let socket = PathBuf::from(format!("/tmp/ftmd-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let child = Command::new(env!("CARGO_BIN_EXE_ftmd"))
        .env("FTM_SOCKET", &socket)
        .env("SHELL", "/bin/sh")
        .env("XDG_DATA_HOME", data_home)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "ftmd did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    Daemon { child, socket }
}

struct Conn {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Conn {
    fn open(socket: &Path) -> Self {
        let write = UnixStream::connect(socket).unwrap();
        write.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Conn { write, read }
    }

    fn line<T: serde::de::DeserializeOwned>(&mut self) -> T {
        let mut line = String::new();
        self.read.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    }

    fn send(&mut self, request: &Request) -> Response {
        let mut out = serde_json::to_vec(request).unwrap();
        out.push(b'\n');
        self.write.write_all(&out).unwrap();
        match self.line() {
            Response::Error { message } => panic!("{message}"),
            r => r,
        }
    }

    fn type_in(&mut self, pane: PaneId, text: &str) {
        self.send(&Request::Input {
            pane,
            data: B64.encode(text),
        });
    }

    fn list(&mut self) -> Vec<Workspace> {
        match self.send(&Request::List) {
            Response::Workspaces { workspaces } => workspaces,
            r => panic!("{r:?}"),
        }
    }
}

/// Reads events until `pane`'s accumulated output contains `needle`.
fn wait_for_output(events: &mut Conn, pane: PaneId, needle: &str) -> String {
    let mut seen = String::new();
    loop {
        if let Event::Output { pane: p, data, reset } = events.line() {
            if p != pane {
                continue;
            }
            if reset {
                seen.clear();
            }
            seen.push_str(&String::from_utf8_lossy(&B64.decode(data).unwrap()));
            if seen.contains(needle) {
                return seen;
            }
        }
    }
}

fn wait_until(conn: &mut Conn, what: &str, ok: impl Fn(&[Workspace]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok(&conn.list()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn pty_streams_and_accepts_input() {
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let daemon = start_daemon("pty", &dir);
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// An agent runs as the foreground job of an interactive shell, in its own process group, and Claude Code ignores
/// SIGHUP. Closing with `remove_worktree` must still stop it before deleting the worktree, then drop the merged branch
/// and the empty `prefix/` directory.
#[test]
fn close_kills_foreground_job_and_removes_worktree() {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("ftmd-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "a").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);

    let daemon = start_daemon("wt", &root.join("data"));
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(Instant::now() < deadline, "foreground job {pid} survived close");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!wt.exists());
    assert!(!root.join("data/ftm/worktrees/repo/chore").exists());
    assert!(git(&repo, &["branch", "--list", "chore/wt"]).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}
