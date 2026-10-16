//! Worktrees made through ftmd: nesting, carrying changes over, and failing cleanly.

mod common;

use common::{Daemon, TempRoot, ftm_bin, git, start_daemon, temp_root, without_user_git_config};
use ftm_core::model::Workspace;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    daemon: Daemon,
    bin: PathBuf,
    repo: PathBuf,
    root: TempRoot,
}

impl Env {
    fn new(name: &str, repo_config: &str, configure: impl FnOnce(&mut Command, &Path)) -> Self {
        let bin = ftm_bin();
        let root = temp_root(name);
        let repo = root.join(format!("{name}repo"));
        std::fs::create_dir_all(root.join("config/ftm/repos")).unwrap();
        std::fs::write(root.join(format!("config/ftm/repos/{name}repo.toml")), repo_config).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "user.email", "t@t"]);
        std::fs::write(repo.join("f"), "committed\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        let daemon = start_daemon(name, |c| {
            c.env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"));
            configure(c, &root);
        });
        Env {
            bin,
            root,
            repo,
            daemon,
        }
    }

    fn try_ftm(&self, args: &[&str]) -> Output {
        without_user_git_config(&mut Command::new(&self.bin))
            .args(args)
            .env("FTM_SOCKET", &self.daemon.socket)
            .env_remove("FTM_WORKSPACE_ID")
            .output()
            .unwrap()
    }

    fn ftm(&self, args: &[&str]) -> String {
        let out = self.try_ftm(args);
        assert!(out.status.success(), "ftm {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn spawn(&self, from: &Path, branch: &str, extra: &[&str]) -> Output {
        let mut args = vec!["spawn", "terminal", "--path", from.to_str().unwrap(), "--worktree", branch];
        args.extend_from_slice(extra);
        self.try_ftm(&args)
    }

    fn workspace(&self, id: &str) -> Workspace {
        let all: Vec<Workspace> = serde_json::from_str(&self.ftm(&["ls", "--json"])).unwrap();
        all.into_iter().find(|w| w.id.to_string() == id).unwrap()
    }

    fn worktrees(&self) -> PathBuf {
        self.root.join("data/ftm/worktrees")
    }
}

fn id(out: Output) -> String {
    assert!(out.status.success(), "spawn: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A worktree spawned from inside another FTM worktree belongs to the main repo: its config, its folder under the
/// worktrees dir, and its removal after the parent worktree is gone.
#[test]
fn nested_worktrees_belong_to_the_main_repo() {
    let env = Env::new("nest", "base = \"based\"\n", |_, _| {});
    git(&env.repo, &["branch", "based"]);
    let based = git(&env.repo, &["rev-parse", "based"]);
    git(&env.repo, &["commit", "-q", "--allow-empty", "-m", "past base"]);

    let parent = id(env.spawn(&env.repo, "feat", &[]));
    let parent_wt = env.worktrees().join("nestrepo/feat");
    assert_eq!(env.workspace(&parent).cwd, parent_wt);

    let child = id(env.spawn(&parent_wt, "fix", &[]));
    let child_ws = env.workspace(&child);
    let child_wt = env.worktrees().join("nestrepo/fix");
    assert_eq!(child_ws.cwd, child_wt);
    assert_eq!(child_ws.worktree.as_ref().unwrap().repo, env.repo);
    assert_eq!(git(&child_wt, &["rev-parse", "HEAD"]), based, "the repo's configured base");

    env.ftm(&["close", "--id", &parent, "--remove-worktree"]);
    assert!(!parent_wt.exists());
    env.ftm(&["close", "--id", &child, "--remove-worktree"]);
    assert!(!child_wt.exists());
    assert!(git(&env.repo, &["branch", "--list", "fix"]).is_empty());
}

/// `--copy-changes` carries modified, staged and binary files over whatever diff settings the user has.
#[test]
fn copy_changes_ignores_the_users_diff_config() {
    let env = Env::new("copy", "copy_ignored = false\n", |c, root| {
        let gitconfig = root.join("gitconfig");
        std::fs::write(
            &gitconfig,
            "[diff]\n\tnoprefix = true\n\trelative = true\n\texternal = false\n[color]\n\tdiff = always\n",
        )
        .unwrap();
        c.env("GIT_CONFIG_GLOBAL", gitconfig);
    });
    let binary: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    std::fs::write(env.repo.join("bin"), &binary[..100]).unwrap();
    git(&env.repo, &["add", "bin"]);
    git(&env.repo, &["commit", "-qm", "bin"]);

    std::fs::write(env.repo.join("f"), "edited\n").unwrap();
    std::fs::write(env.repo.join("staged"), "new\n").unwrap();
    git(&env.repo, &["add", "staged"]);
    std::fs::write(env.repo.join("bin"), &binary).unwrap();

    id(env.spawn(&env.repo, "carry", &["--copy-changes"]));
    let wt = env.worktrees().join("copyrepo/carry");
    assert_eq!(std::fs::read_to_string(wt.join("f")).unwrap(), "edited\n");
    assert_eq!(std::fs::read_to_string(wt.join("staged")).unwrap(), "new\n");
    assert_eq!(std::fs::read(wt.join("bin")).unwrap(), binary);
}
