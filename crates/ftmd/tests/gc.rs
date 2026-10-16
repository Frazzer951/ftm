//! `ftm gc` against a repo with a bare "origin", and worktrees made through ftmd.

mod common;

use common::{ftm_bin, git, start_daemon, temp_root};
use std::path::{Path, PathBuf};
use std::process::Command;

fn commit(dir: &Path, file: &str, msg: &str) {
    std::fs::write(dir.join(file), format!("{msg}\n")).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", msg]);
}

#[test]
fn gc_removes_only_merged_clean_closed_worktrees() {
    let bin = ftm_bin();
    let root = temp_root("gc");
    let (remote, repo, config, data) = (
        root.join("remote.git"),
        root.join("gcrepo"),
        root.join("config"),
        root.join("data"),
    );
    std::fs::create_dir_all(config.join("ftm/repos")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(config.join("ftm/repos/gcrepo.toml"), "base = \"origin/main\"\n").unwrap();

    git(&root, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "user.email", "t@t"]);
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    git(&repo, &["add", "."]);
    commit(&repo, "f", "init");
    git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&repo, &["push", "-qu", "origin", "main"]);

    let daemon = start_daemon("gc", |c| {
        c.env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data);
    });
    let ftm = |args: &[&str]| {
        let out = Command::new(&bin)
            .args(args)
            .env("FTM_SOCKET", &daemon.socket)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data)
            .env_remove("FTM_WORKSPACE_ID")
            .output()
            .unwrap();
        assert!(out.status.success(), "ftm {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let worktree = |branch: &str, keep_open: bool| -> PathBuf {
        let id = ftm(&["spawn", "terminal", "--path", repo.to_str().unwrap(), "--worktree", branch]);
        if !keep_open {
            ftm(&["close", "--id", id.trim()]);
        }
        data.join("ftm/worktrees/gcrepo").join(branch)
    };

    // Merged with a merge commit; an ignored file does not stop the removal.
    let merged = worktree("feat/merged", false);
    commit(&merged, "m", "merged work");
    std::fs::write(merged.join(".env"), "X=1\n").unwrap();
    git(&repo, &["merge", "-q", "--no-ff", "-m", "merge", "feat/merged"]);

    // Squash-merged, and its remote branch deleted.
    let squashed = worktree("squashed", false);
    commit(&squashed, "s", "squashed work");
    git(&squashed, &["push", "-qu", "origin", "squashed"]);
    git(&repo, &["merge", "-q", "--squash", "squashed"]);
    git(&repo, &["commit", "-qm", "squash"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["push", "-q", "origin", "--delete", "squashed"]);

    // Remote branch deleted (merged in a way gc cannot see) after everything was pushed, or with a commit since.
    let pushed = worktree("gone-pushed", false);
    commit(&pushed, "p", "pushed work");
    git(&pushed, &["push", "-qu", "origin", "gone-pushed"]);
    let unpushed = worktree("gone-unpushed", false);
    commit(&unpushed, "u", "pushed work");
    git(&unpushed, &["push", "-qu", "origin", "gone-unpushed"]);
    commit(&unpushed, "u2", "never pushed");
    git(&remote, &["branch", "-qD", "gone-pushed", "gone-unpushed"]);

    // Merged (no commits of their own) but not safe to remove.
    let dirty = worktree("dirty", false);
    std::fs::write(dirty.join("notes"), "wip\n").unwrap();
    let open = worktree("open", true);

    // Not merged at all: not listed.
    let active = worktree("active", false);
    commit(&active, "a", "active work");

    let listed = ftm(&["gc"]);
    for line in [
        "remove  gcrepo/feat/merged  (merged into origin/main)",
        "remove  gcrepo/squashed  (squash-merged into origin/main)",
        "remove  gcrepo/gone-pushed  (upstream branch deleted, everything pushed)",
        "skip    gcrepo/gone-unpushed  (upstream branch deleted, local commits never pushed)",
        "skip    gcrepo/dirty  (uncommitted or untracked changes)",
        "skip    gcrepo/open  (open workspace open)",
        "dry run: pass --yes to remove 3 worktree(s) and their branches",
    ] {
        assert!(listed.contains(line), "missing {line:?} in:\n{listed}");
    }
    assert!(!listed.contains("active"), "{listed}");
    for wt in [&merged, &squashed, &pushed] {
        assert!(wt.exists(), "dry run removed {}", wt.display());
    }

    let removed = ftm(&["gc", "--yes"]);
    assert_eq!(removed.matches("removed ").count(), 3, "{removed}");
    for (wt, branch) in [(&merged, "feat/merged"), (&squashed, "squashed"), (&pushed, "gone-pushed")] {
        assert!(!wt.exists(), "{} still there", wt.display());
        assert!(git(&repo, &["branch", "--list", branch]).is_empty(), "{branch} still there");
    }
    assert!(!data.join("ftm/worktrees/gcrepo/feat").exists());
    let tracking = |r: &str| git(&repo, &["for-each-ref", &format!("refs/remotes/origin/{r}")]);
    assert!(tracking("gone-pushed").is_empty(), "stale tracking ref kept");
    assert!(!tracking("gone-unpushed").is_empty());
    for wt in [&unpushed, &dirty, &open, &active] {
        assert!(wt.exists(), "{} was removed", wt.display());
    }
    let list = git(&repo, &["worktree", "list"]);
    assert!(!list.contains("squashed") && list.contains("active"), "{list}");

    drop(daemon);
}
