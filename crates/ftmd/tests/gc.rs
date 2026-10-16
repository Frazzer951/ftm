//! `ftm gc` against a repo with a bare "origin", and worktrees made through ftmd.

mod common;

use common::{ftm_bin, git, start_daemon, temp_root, without_user_git_config};
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
        let out = without_user_git_config(&mut Command::new(&bin))
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
        "skip    gcrepo/gone-pushed  (upstream branch deleted, everything pushed; --include-upstream-deleted removes it)",
        "skip    gcrepo/gone-unpushed  (upstream branch deleted, local commits never pushed)",
        "skip    gcrepo/dirty  (uncommitted or untracked changes)",
        "skip    gcrepo/open  (open workspace open)",
        "dry run: pass --yes to remove 2 worktree(s) and their branches",
    ] {
        assert!(listed.contains(line), "missing {line:?} in:\n{listed}");
    }
    assert!(!listed.contains("active"), "{listed}");
    for wt in [&merged, &squashed, &pushed] {
        assert!(wt.exists(), "dry run removed {}", wt.display());
    }
    let opted = ftm(&["gc", "--include-upstream-deleted"]);
    assert!(
        opted.contains("remove  gcrepo/gone-pushed  (upstream branch deleted, everything pushed)"),
        "{opted}"
    );

    let tracking = |r: &str| git(&repo, &["for-each-ref", &format!("refs/remotes/origin/{r}")]);
    let removed = ftm(&["gc", "--yes"]);
    assert_eq!(removed.matches("removed ").count(), 2, "{removed}");
    for (wt, branch) in [(&merged, "feat/merged"), (&squashed, "squashed")] {
        assert!(!wt.exists(), "{} still there", wt.display());
        assert!(git(&repo, &["branch", "--list", branch]).is_empty(), "{branch} still there");
    }
    assert!(!data.join("ftm/worktrees/gcrepo/feat").exists());
    assert!(pushed.exists() && !git(&repo, &["branch", "--list", "gone-pushed"]).is_empty());
    assert!(!tracking("gone-pushed").is_empty(), "tracking ref removed without the flag");

    let removed = ftm(&["gc", "--yes", "--include-upstream-deleted"]);
    assert_eq!(removed.matches("removed ").count(), 1, "{removed}");
    assert!(!pushed.exists() && git(&repo, &["branch", "--list", "gone-pushed"]).is_empty());
    assert!(tracking("gone-pushed").is_empty(), "stale tracking ref kept");
    assert!(!tracking("gone-unpushed").is_empty());
    for wt in [&unpushed, &dirty, &open, &active] {
        assert!(wt.exists(), "{} was removed", wt.display());
    }
    let list = git(&repo, &["worktree", "list"]);
    assert!(!list.contains("squashed") && list.contains("active"), "{list}");

    drop(daemon);
}

/// A removal that fails goes to stderr and makes gc exit non-zero; the rest are still removed.
#[test]
fn gc_reports_failed_removals() {
    let bin = ftm_bin();
    let root = temp_root("gc-fail");
    let (repo, config, data) = (root.join("failgc"), root.join("config"), root.join("data"));
    std::fs::create_dir_all(config.join("ftm/repos")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(config.join("ftm/repos/failgc.toml"), "base = \"main\"\n").unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "user.email", "t@t"]);
    commit(&repo, "f", "init");

    let daemon = start_daemon("gc-fail", |c| {
        c.env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data);
    });
    let ftm = |args: &[&str]| {
        without_user_git_config(&mut Command::new(&bin))
            .args(args)
            .env("FTM_SOCKET", &daemon.socket)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data)
            .env_remove("FTM_WORKSPACE_ID")
            .output()
            .unwrap()
    };
    let worktree = |branch: &str| -> PathBuf {
        let out = ftm(&["spawn", "terminal", "--path", repo.to_str().unwrap(), "--worktree", branch]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(ftm(&["close", "--id", &id]).status.success());
        data.join("ftm/worktrees/failgc").join(branch)
    };
    let locked = worktree("locked");
    git(&repo, &["worktree", "lock", locked.to_str().unwrap()]);
    let fine = worktree("fine");
    // A held ref lock makes `branch -D` fail after the worktree is gone.
    let stuck = worktree("pre/stuck");
    std::fs::write(repo.join(".git/refs/heads/pre/stuck.lock"), "").unwrap();

    let out = ftm(&["gc", "--yes", "--no-fetch"]);
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "{stdout}");
    assert!(stderr.contains("failed  failgc/locked"), "{stderr}");
    assert!(!stdout.contains("failed"), "{stdout}");
    assert!(stdout.contains("removed failgc/fine"), "{stdout}");
    assert!(locked.exists() && !fine.exists());
    assert!(stdout.contains("removed failgc/pre/stuck"), "{stdout}");
    assert!(
        stderr.contains("failed  failgc/pre/stuck  (worktree removed, but:"),
        "{stderr}"
    );
    assert!(!stuck.exists() && !data.join("ftm/worktrees/failgc/pre").exists());
    assert!(!git(&repo, &["worktree", "list"]).contains("stuck"), "not pruned");
}

/// A repo made with `--separate-git-dir` is named after its git dir, from its main checkout and its worktrees alike,
/// so ftmd and gc read the same config for it.
#[test]
fn gc_and_ftmd_agree_on_a_separate_git_dir_repo() {
    let bin = ftm_bin();
    let root = temp_root("gc-sep");
    let (store, checkout, config, data) = (
        root.join("sepstore.git"),
        root.join("sepcheckout"),
        root.join("config"),
        root.join("data"),
    );
    std::fs::create_dir_all(config.join("ftm/repos")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(config.join("ftm/repos/sepstore.toml"), "base = \"main\"\n").unwrap();
    git(
        &root,
        &[
            "init",
            "-q",
            "-b",
            "main",
            "--separate-git-dir",
            store.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    git(&checkout, &["config", "user.name", "t"]);
    git(&checkout, &["config", "user.email", "t@t"]);
    commit(&checkout, "f", "init");

    let daemon = start_daemon("gc-sep", |c| {
        c.env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data);
    });
    let ftm = |args: &[&str]| {
        let out = without_user_git_config(&mut Command::new(&bin))
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
    let worktree = |from: &Path, branch: &str| -> PathBuf {
        let id = ftm(&["spawn", "terminal", "--path", from.to_str().unwrap(), "--worktree", branch]);
        ftm(&["close", "--id", id.trim()]);
        let path = data.join("ftm/worktrees/sepstore").join(branch);
        assert!(path.is_dir(), "{} missing", path.display());
        path
    };
    let outer = worktree(&checkout, "outer");
    let inner = worktree(&outer, "inner");
    commit(&inner, "i", "inner work");
    git(&checkout, &["merge", "-q", "--no-ff", "-m", "merge", "inner"]);

    let out = ftm(&["gc", "--yes", "--no-fetch"]);
    assert!(out.contains("removed sepstore/inner"), "{out}");
    assert!(out.contains("removed sepstore/outer"), "{out}");
    assert!(!inner.exists() && !outer.exists());
}

/// Without a configured base, gc judges worktrees against the remote's default branch, the one ftmd starts them from,
/// even when that is not `main`.
#[test]
fn gc_uses_the_remotes_default_branch() {
    let bin = ftm_bin();
    let root = temp_root("gc-master");
    let (remote, repo, config, data) = (
        root.join("remote.git"),
        root.join("masterrepo"),
        root.join("config"),
        root.join("data"),
    );
    std::fs::create_dir_all(root.join("home")).unwrap();
    git(&root, &["init", "-q", "--bare", "-b", "master", remote.to_str().unwrap()]);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "master"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "user.email", "t@t"]);
    commit(&repo, "f", "init");
    git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&repo, &["push", "-qu", "origin", "master"]);

    let daemon = start_daemon("gc-master", |c| {
        c.env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_DATA_HOME", &data);
    });
    let ftm = |args: &[&str]| {
        let out = without_user_git_config(&mut Command::new(&bin))
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
    let id = ftm(&["spawn", "terminal", "--path", repo.to_str().unwrap(), "--worktree", "feat"]);
    ftm(&["close", "--id", id.trim()]);
    let feat = data.join("ftm/worktrees/masterrepo/feat");
    commit(&feat, "x", "feature work");
    git(&repo, &["merge", "-q", "--no-ff", "-m", "merge", "feat"]);
    git(&repo, &["push", "-q", "origin", "master"]);

    let out = ftm(&["gc"]);
    assert!(out.contains("remove  masterrepo/feat  (merged into origin/master)"), "{out}");
}
