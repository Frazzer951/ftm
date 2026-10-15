//! Agent workspaces end to end, with `FTM_AGENT_CMD` pointing at a fake `claude` that drives the real `ftm` CLI.

mod common;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use common::{Conn, alive, ftm_bin, git, start_daemon, temp_root, wait_for, wait_until};
use ftm_core::model::{Kind, Status, Workspace, WorkspaceId};
use ftm_core::protocol::{Request, Response};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Records what it was started with, then (as the leader) spawns a nested agent and a detached terminal and reports a
/// status, all through `ftm`. Like Claude Code, it ignores SIGHUP.
const FAKE_AGENT: &str = r#"#!/bin/sh
PATH="$FTM_BIN_DIR:$PATH"
out="$FAKE_OUT/$FTM_NAME"
if [ "$1" = --settings ]; then printf '%s' "$2" > "$out.settings"; shift 2; fi
: > "$out.argv"
for arg in "$@"; do printf '%s\0' "$arg" >> "$out.argv"; done
{
  echo "FTM_NAME=$FTM_NAME"
  echo "FTM_WORKSPACE_ID=$FTM_WORKSPACE_ID"
  echo "FTM_PANE_ID=$FTM_PANE_ID"
  echo "FTM_SOCKET=$FTM_SOCKET"
  echo "PWD=$(pwd -P)"
  if [ -e "$FAKE_OUT/setup-ran" ]; then echo "SETUP=yes"; else echo "SETUP=no"; fi
  echo "PID=$$"
  echo "FTM_BIN=$FTM_BIN"
} > "$out.vars"
# Like Claude Code: a placeholder title, "<glyph> <task>", then a title that is just the name (as with -n).
printf '\033]0;\342\234\263 Claude Code\007\033]2;\342\240\202 Task of %s\007\033]2;\342\234\263 %s\007' "$FTM_NAME" "$FTM_NAME"
if [ "$FTM_NAME" = leader ]; then
  ftm spawn agent --name child > "$out.child" 2>&1 || exit 1
  ftm spawn terminal --detached --name loose > "$out.detached" 2>&1 || exit 1
  ftm status working > "$out.status" 2>&1 || exit 1
  ftm rename "  fix the bug  " > "$out.rename" 2>&1 || exit 1
fi
trap '' HUP
touch "$out.ready"
exec sleep 600
"#;

const BRIEF: &str = "Fix \"the bug\" in $(whoami)'s code;\nthen `stop`.";

fn ftm(bin: &Path, socket: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .env("FTM_SOCKET", socket)
        .env_remove("FTM_WORKSPACE_ID")
        .output()
        .unwrap();
    assert!(out.status.success(), "ftm {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn ls(bin: &Path, socket: &Path) -> Vec<Workspace> {
    serde_json::from_str(&ftm(bin, socket, &["ls", "--json"])).unwrap()
}

fn vars(out: &Path, name: &str) -> HashMap<String, String> {
    std::fs::read_to_string(out.join(format!("{name}.vars")))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn argv(out: &Path, name: &str) -> Vec<String> {
    let raw = std::fs::read(out.join(format!("{name}.argv"))).unwrap();
    raw.split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

fn read_id(out: &Path, file: &str) -> WorkspaceId {
    let text = std::fs::read_to_string(out.join(file)).unwrap();
    text.trim().parse().unwrap_or_else(|e| panic!("{file}: {e}: {text}"))
}

#[test]
fn agents_nest_report_status_and_clean_up() {
    let bin = ftm_bin();
    let root = temp_root("agents");
    let (out, repo, config) = (root.join("out"), root.join("agentrepo"), root.join("config"));
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(config.join("ftm/repos")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();

    let fake = root.join("fake-claude");
    std::fs::write(&fake, FAKE_AGENT).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        config.join("ftm/repos/agentrepo.toml"),
        format!(
            "setup = \"touch '{}'\"\ncopy_ignored = [\".env\"]\n",
            out.join("setup-ran").display()
        ),
    )
    .unwrap();

    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(repo.join("f"), "committed\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    std::fs::write(repo.join("f"), "edited\n").unwrap();
    std::fs::write(repo.join("untracked"), "new\n").unwrap();
    std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(root.join("brief.md"), BRIEF).unwrap();

    let daemon = start_daemon("agents", |c| {
        c.env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", root.join("data"))
            .env("FTM_AGENT_CMD", &fake)
            .env("FTM_BIN_DIR", bin.parent().unwrap())
            .env("FAKE_OUT", &out);
    });
    let socket = daemon.socket.clone();

    let leader: WorkspaceId = ftm(
        &bin,
        &socket,
        &[
            "spawn",
            "agent",
            "--name",
            "leader",
            "--path",
            repo.to_str().unwrap(),
            "--worktree",
            "feat/leader",
            "--copy-changes",
            "--brief",
            root.join("brief.md").to_str().unwrap(),
        ],
    )
    .parse()
    .unwrap();
    let report = || {
        ["child", "detached", "status", "rename"]
            .map(|f| std::fs::read_to_string(out.join(format!("leader.{f}"))).unwrap_or_default())
            .join(" | ")
    };
    wait_for(&format!("leader to finish ({})", report()), Duration::from_secs(20), || {
        out.join("leader.ready").exists()
    });
    wait_for("child to start", Duration::from_secs(20), || out.join("child.ready").exists());

    let workspaces = ls(&bin, &socket);
    let by_id = |id: WorkspaceId| workspaces.iter().find(|w| w.id == id).unwrap().clone();
    let leader_ws = by_id(leader);
    let wt = leader_ws.cwd.clone();
    assert_eq!(wt, root.join("data/ftm/worktrees/agentrepo/feat/leader"));
    assert_eq!(leader_ws.worktree.as_ref().unwrap().branch, "feat/leader");

    // Name, brief and FTM_* env reach the agent; the repo setup ran before it.
    assert_eq!(argv(&out, "leader"), ["-n", "leader", BRIEF]);
    let lv = vars(&out, "leader");
    assert_eq!(lv["FTM_NAME"], "leader");
    assert_eq!(lv["FTM_WORKSPACE_ID"], leader.to_string());
    assert_eq!(lv["FTM_PANE_ID"], leader_ws.tabs[0].layout.panes()[0].to_string());
    assert_eq!(lv["FTM_SOCKET"], socket.to_str().unwrap());
    assert_eq!(Path::new(&lv["PWD"]), wt);
    assert_eq!(lv["SETUP"], "yes");
    assert_eq!(Path::new(&lv["FTM_BIN"]), bin);

    // The worktree carries tracked edits, untracked files and the configured ignored files.
    assert_eq!(std::fs::read_to_string(wt.join("f")).unwrap(), "edited\n");
    assert_eq!(std::fs::read_to_string(wt.join("untracked")).unwrap(), "new\n");
    assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "SECRET=1\n");

    // `ftm spawn` inside the agent nests under it unless --detached; `ftm status` reports for it.
    let child = read_id(&out, "leader.child");
    let loose = read_id(&out, "leader.detached");
    let child_ws = by_id(child);
    assert_eq!(child_ws.parent, Some(leader));
    assert_eq!(child_ws.kind, Kind::Agent);
    assert_eq!(child_ws.cwd, wt);
    assert_eq!(by_id(loose).parent, None);
    assert_eq!(by_id(loose).kind, Kind::Terminal);
    assert_eq!(leader_ws.status, Status::Working);
    assert_eq!(child_ws.status, Status::Starting);
    assert_eq!(argv(&out, "child"), ["-n", "child"]);

    // A named agent keeps its name (its SendMessage address) and is not retitled by its terminal. An unnamed one
    // runs without -n and is named by the task title its terminal gets, minus the status glyph, ignoring the
    // placeholder and a title that is just its name.
    assert_eq!(child_ws.auto_title, None);
    assert_eq!(leader_ws.auto_title, None);
    let loose_dir = root.join("unnamed-dir");
    std::fs::create_dir_all(&loose_dir).unwrap();
    let unnamed: WorkspaceId = ftm(&bin, &socket, &["spawn", "agent", "--path", loose_dir.to_str().unwrap()])
        .parse()
        .unwrap();
    wait_for("unnamed agent to start", Duration::from_secs(20), || {
        out.join("unnamed-dir.ready").exists()
    });
    assert_eq!(argv(&out, "unnamed-dir"), Vec::<String>::new());
    let titled = |id: WorkspaceId| ls(&bin, &socket).into_iter().find(|w| w.id == id).unwrap();
    wait_for("auto title", Duration::from_secs(10), || titled(unnamed).auto_title.is_some());
    let unnamed_ws = titled(unnamed);
    assert_eq!(unnamed_ws.auto_title.as_deref(), Some("Task of unnamed-dir"));
    assert_eq!(unnamed_ws.display_name(), "Task of unnamed-dir");
    assert!(ftm(&bin, &socket, &["ls"]).contains("Task of unnamed-dir (unnamed-dir)"));
    ftm(&bin, &socket, &["close", "--id", &unnamed.to_string()]);

    // `ftm rename` inside the agent retitles its own workspace; the spawn name stays for FTM_NAME and `claude -n`.
    assert_eq!(leader_ws.title.as_deref(), Some("fix the bug"));
    assert_eq!(leader_ws.name, "leader");
    assert!(ftm(&bin, &socket, &["ls"]).contains("fix the bug (leader)"));
    let cv = vars(&out, "child");
    assert_eq!(cv["FTM_WORKSPACE_ID"], child.to_string());

    // Each agent gets ftmd's hook settings; running each hook as Claude would drives the leader's status.
    let settings_path = std::fs::read_to_string(out.join("leader.settings")).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("child.settings")).unwrap(), settings_path);
    let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    for (event, matcher, status) in [
        ("PermissionRequest", None, Status::NeedsInput),
        ("PostToolUse", None, Status::Working),
        ("PreToolUse", Some("AskUserQuestion"), Status::NeedsInput),
        ("UserPromptSubmit", None, Status::Working),
        ("Notification", Some("permission_prompt"), Status::NeedsInput),
        ("Stop", None, Status::Idle),
        ("SessionEnd", None, Status::Done),
    ] {
        let group = settings["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("no {event} hook"))
            .iter()
            .find(|g| matcher.is_none_or(|m| g["matcher"].as_str().is_some_and(|re| re.split('|').any(|p| p == m))))
            .unwrap_or_else(|| panic!("no {event} hook for {matcher:?}"));
        let command = group["hooks"][0]["command"].as_str().unwrap();
        let ran = Command::new("/bin/sh")
            .args(["-c", command])
            .env("FTM_SOCKET", &socket)
            .env("FTM_WORKSPACE_ID", leader.to_string())
            .output()
            .unwrap();
        assert!(ran.status.success() && ran.stdout.is_empty(), "{event} hook: {ran:?}");
        let now = ls(&bin, &socket).into_iter().find(|w| w.id == leader).unwrap();
        assert_eq!(now.status, status, "after {event}");
        assert_eq!(now.unseen, matches!(event, "Stop" | "SessionEnd"), "unseen after {event}");
    }
    // The Stop hook reads Claude's input: running background subagents are kept, finished ones and shells are not.
    let stop = settings["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
    let stop_with = |input: &str| {
        let mut child = Command::new("/bin/sh")
            .args(["-c", stop])
            .env("FTM_SOCKET", &socket)
            .env("FTM_WORKSPACE_ID", leader.to_string())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        ls(&bin, &socket).into_iter().find(|w| w.id == leader).unwrap()
    };
    let tasks = r#"{"hook_event_name":"Stop","background_tasks":[
        {"id":"a","type":"subagent","status":"running"},{"id":"b","type":"subagent","status":"running"},
        {"id":"c","type":"subagent","status":"completed"},{"id":"d","type":"shell","status":"running"}]}"#;
    let now = stop_with(tasks);
    assert_eq!((now.status, now.background_agents), (Status::Idle, 2));
    assert_eq!(stop_with(r#"{"background_tasks":[]}"#).background_agents, 0);
    assert_eq!(stop_with(tasks).background_agents, 2);
    assert_eq!(
        stop_with("not json").background_agents,
        0,
        "unreadable input still reports idle"
    );
    let idle_prompt = settings["hooks"]["Notification"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["matcher"].as_str().is_none_or(|m| m.split('|').any(|p| p == "idle_prompt")));
    assert!(!idle_prompt, "the 60s idle reminder must not mark an agent as needing input");
    // A hook outside ftm (no workspace) is a silent no-op.
    let stray = Command::new("/bin/sh")
        .args(["-c", settings["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap()])
        .env("FTM_SOCKET", "/tmp/ftm-nothing-here.sock")
        .env_remove("FTM_WORKSPACE_ID")
        .output()
        .unwrap();
    assert!(stray.status.success() && stray.stdout.is_empty() && stray.stderr.is_empty());

    // Closing the leader stops its HUP-ignoring agent, reparents the child, and removes a clean worktree and its
    // merged branch.
    let leader_pid: i32 = lv["PID"].parse().unwrap();
    let child_pid: i32 = cv["PID"].parse().unwrap();
    assert!(alive(leader_pid));
    git(&wt, &["checkout", "--", "."]);
    git(&wt, &["clean", "-fdqx"]);
    ftm(&bin, &socket, &["close", "--id", &leader.to_string(), "--remove-worktree"]);
    wait_for("leader agent to die", Duration::from_secs(10), || !alive(leader_pid));
    assert!(!wt.exists());
    assert!(git(&repo, &["branch", "--list", "feat/leader"]).is_empty());
    let after = ls(&bin, &socket);
    assert!(after.iter().all(|w| w.id != leader));
    assert_eq!(after.iter().find(|w| w.id == child).unwrap().parent, None);
    assert!(alive(child_pid));

    // Stopping ftmd right after a close still finishes killing the HUP-ignoring agent.
    let mut conn = Conn::open(&socket);
    for id in [child, loose] {
        conn.send(&Request::Close {
            id,
            remove_worktree: false,
        });
    }
    assert!(ls(&bin, &socket).is_empty());
    drop(daemon);
    wait_for("child agent to die with ftmd", Duration::from_secs(1), || !alive(child_pid));
    let _ = std::fs::remove_dir_all(&root);
}

/// The repo's setup is POSIX sh even when the login shell is fish, and an agent that exits before it ever reports a
/// status leaves its pane on screen and its workspace `failed` rather than an empty sidebar row.
#[test]
fn setup_runs_in_sh_and_an_agent_that_never_starts_fails_visibly() {
    let bin = ftm_bin();
    let root = temp_root("failed");
    let (out, repo, config) = (root.join("out"), root.join("failrepo"), root.join("config"));
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(config.join("ftm/repos")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(
        config.join("ftm/repos/failrepo.toml"),
        format!(
            "setup = '''if [ -n \"$FTM_NAME\" ]; then touch \"{}\"; fi'''\n",
            out.join("setup-ran").display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );

    let shell = ["/usr/bin/fish", "/bin/fish", "/opt/homebrew/bin/fish"]
        .into_iter()
        .find(|s| Path::new(s).exists())
        .unwrap_or("/bin/sh");
    let daemon = start_daemon("failed", |c| {
        c.env("HOME", root.join("home"))
            .env("SHELL", shell)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", root.join("data"))
            .env("FTM_AGENT_CMD", "/bin/sh -c 'echo no-claude-here; kill -9 $PPID'");
    });
    let socket = daemon.socket.clone();

    let id: WorkspaceId = ftm(
        &bin,
        &socket,
        &[
            "spawn",
            "agent",
            "--name",
            "doomed",
            "--path",
            repo.to_str().unwrap(),
            "--worktree",
            "feat/doomed",
        ],
    )
    .parse()
    .unwrap();
    let mut conn = Conn::open(&socket);
    wait_until(&mut conn, "agent to fail", |all| {
        all.iter().any(|w| w.id == id && w.status == Status::Failed)
    });
    assert!(out.join("setup-ran").exists(), "setup did not run under {shell}");

    let ws = conn.list().into_iter().find(|w| w.id == id).unwrap();
    assert_eq!(ws.tabs.len(), 1);
    let pane = ws.tabs[0].layout.panes()[0];
    let Response::Output { data } = conn.send(&Request::Capture { pane, raw: false }) else {
        panic!("capture failed")
    };
    let screen = String::from_utf8(B64.decode(data).unwrap()).unwrap();
    assert!(screen.contains("no-claude-here"), "{screen}");
    assert!(screen.contains("exited before it started"), "{screen}");

    // The dead pane takes typing and resizes without errors, and closes like any other.
    conn.type_in(pane, "hello\r");
    assert!(matches!(
        conn.send(&Request::Resize {
            pane,
            cols: 100,
            rows: 30
        }),
        Response::Ok
    ));
    conn.send(&Request::ClosePane { pane });
    wait_until(&mut conn, "pane to close", |all| {
        all.iter().any(|w| w.id == id && w.tabs.is_empty())
    });
    conn.send(&Request::Close {
        id,
        remove_worktree: false,
    });
    drop(daemon);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pin_only_top_level_workspaces() {
    let dir = temp_root("pin");
    let daemon = start_daemon("pin", |_| {});
    let bin = ftm_bin();
    let socket = &daemon.socket;
    let path = dir.to_str().unwrap();
    let parent = ftm(&bin, socket, &["spawn", "terminal", "--path", path, "--name", "parent"]);
    let child = ftm(
        &bin,
        socket,
        &["spawn", "terminal", "--path", path, "--name", "child", "--parent", &parent],
    );
    let pinned = |id: &str| ls(&bin, socket).into_iter().find(|w| w.id.to_string() == id).unwrap().pinned;

    ftm(&bin, socket, &["pin", &parent]);
    assert!(pinned(&parent));
    assert!(
        ftm(&bin, socket, &["ls"])
            .lines()
            .any(|l| l.starts_with("parent ") && l.contains("(pinned)"))
    );

    let refused = Command::new(&bin)
        .args(["pin"])
        .env("FTM_SOCKET", socket)
        .env("FTM_WORKSPACE_ID", &child)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("top-level"));
    assert!(!pinned(&child));

    ftm(&bin, socket, &["unpin", &parent]);
    assert!(!pinned(&parent));

    // Once its parent is gone the child is top-level and can be pinned.
    ftm(&bin, socket, &["close", "--id", &parent]);
    ftm(&bin, socket, &["pin", &child]);
    assert!(pinned(&child));
}

/// New worktrees get the source checkout's gitignored files, minus big rebuildable directories at any depth and the
/// repo's own excludes; an explicit `copy_ignored` list copies only what it names, and `false` copies nothing.
#[test]
fn worktrees_copy_ignored_files() {
    let bin = ftm_bin();
    let root = temp_root("ignored");
    let (repo, config) = (root.join("ignrepo"), root.join("config"));
    std::fs::create_dir_all(config.join("ftm/repos")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::fs::write(root.join("outside/secret"), "outside\n").unwrap();

    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join(".gitignore"),
        ".env\n.claude/\n.venv/\nnode_modules/\ntarget/\n__pycache__/\ndbt_profiles/\nbig/\nlink\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("pkg")).unwrap();
    std::fs::write(repo.join("pkg/index.js"), "tracked\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    let write = |rel: &str, body: &str, mode: u32| {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write(".env", "SECRET=1\n", 0o644);
    write(".claude/settings.local.json", "{}\n", 0o644);
    write(".claude/node_modules/dep.js", "x\n", 0o644);
    write(".claude/worktrees/other/.git", "gitdir: elsewhere\n", 0o644);
    write(".venv/bin/python", "x\n", 0o755);
    write("pkg/node_modules/dep.js", "x\n", 0o644);
    write("pkg/__pycache__/m.pyc", "x\n", 0o644);
    write("target/debug/bin", "x\n", 0o755);
    write("dbt_profiles/profiles.yml", "dev: {}\n", 0o600);
    write("dbt_profiles/run.sh", "#!/bin/sh\n", 0o755);
    write("big/data.bin", "lots\n", 0o644);
    std::os::unix::fs::symlink("../outside/secret", repo.join("link")).unwrap();
    write(".claude/locked", "x\n", 0o000);
    write(".claude/sealed/inner", "x\n", 0o644);
    std::fs::set_permissions(repo.join(".claude/sealed"), std::fs::Permissions::from_mode(0o000)).unwrap();

    let daemon = start_daemon("ignored", |c| {
        c.env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", root.join("data"));
    });
    let socket = daemon.socket.clone();
    let spawn = |branch: &str, repo_config: &str| {
        std::fs::write(config.join("ftm/repos/ignrepo.toml"), repo_config).unwrap();
        let id = ftm(
            &bin,
            &socket,
            &["spawn", "terminal", "--path", repo.to_str().unwrap(), "--worktree", branch],
        );
        ftm(&bin, &socket, &["close", "--id", &id]);
        root.join("data/ftm/worktrees/ignrepo").join(branch)
    };
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    let wt = spawn("default", "");
    assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "SECRET=1\n");
    assert!(wt.join(".claude/settings.local.json").is_file());
    assert_eq!(mode(&wt.join("dbt_profiles/profiles.yml")), 0o600);
    assert_eq!(mode(&wt.join("dbt_profiles/run.sh")), 0o755);
    assert!(wt.join("big/data.bin").is_file());
    assert!(wt.join("link").symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_link(wt.join("link")).unwrap(), Path::new("../outside/secret"));
    for skipped in [
        ".venv",
        "target",
        "pkg/node_modules",
        "pkg/__pycache__",
        ".claude/node_modules",
        ".claude/worktrees/other",
    ] {
        assert!(!wt.join(skipped).exists(), "{skipped} was copied");
    }
    assert!(wt.join("pkg/index.js").is_file());
    // Unreadable paths are logged and left out; the rest still comes over.
    assert!(!wt.join(".claude/locked").exists() && !wt.join(".claude/sealed/inner").exists());

    let wt = spawn("exclude", "copy_ignored_exclude = [\"big\", \"*.sh\"]\n");
    assert!(wt.join(".env").is_file() && wt.join("dbt_profiles/profiles.yml").is_file());
    assert!(!wt.join("big").exists() && !wt.join("dbt_profiles/run.sh").exists());
    assert!(!wt.join(".venv").exists());

    let wt = spawn("explicit", "copy_ignored = [\".env\", \"target\"]\n");
    assert!(wt.join(".env").is_file() && wt.join("target/debug/bin").is_file());
    assert!(!wt.join(".claude").exists() && !wt.join("dbt_profiles").exists());

    let wt = spawn("disabled", "copy_ignored = false\n");
    assert!(wt.join("pkg/index.js").is_file());
    assert!(!wt.join(".env").exists() && !wt.join(".claude").exists());

    drop(daemon);
    std::fs::set_permissions(repo.join(".claude/sealed"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
