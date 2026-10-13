//! Agent workspaces end to end, with `FTM_AGENT_CMD` pointing at a fake `claude` that drives the real `ftm` CLI.

mod common;

use common::{Conn, alive, ftm_bin, git, start_daemon, temp_root, wait_for};
use ftm_core::model::{Kind, Status, Workspace, WorkspaceId};
use ftm_core::protocol::Request;
use std::collections::HashMap;
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
} > "$out.vars"
if [ "$FTM_NAME" = leader ]; then
  ftm spawn agent --name child > "$out.child" 2>&1 || exit 1
  ftm spawn terminal --detached --name loose > "$out.detached" 2>&1 || exit 1
  ftm status working > "$out.status" 2>&1 || exit 1
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
        ["child", "detached", "status"]
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
        let now = ls(&bin, &socket).into_iter().find(|w| w.id == leader).unwrap().status;
        assert_eq!(now, status, "after {event}");
    }
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
