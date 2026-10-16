use crate::client;
use eyre::{Result, WrapErr, bail};
use ftm_core::config::RepoConfig;
use ftm_core::model::Workspace;
use ftm_core::paths;
use ftm_core::protocol::{Request, Response};
use ftm_core::repo::{main_repo, remote_default, repo_name};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_BASE: &str = "origin/main";

struct Found {
    path: PathBuf,
    /// Relative to the worktrees dir: `<repo>/<branch>`.
    shown: String,
}

enum Verdict {
    Remove {
        branch: String,
        /// The remote-tracking ref of a branch deleted on the remote, removed along with the branch.
        stale: Option<String>,
        why: String,
    },
    Skip(String),
    Keep,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .wrap_err("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn ok(dir: &Path, args: &[&str]) -> bool {
    git(dir, args).is_ok()
}

/// Lists FTM worktrees whose branch is merged into the repo's base and, with `yes`, removes them and their branches.
pub fn run(yes: bool, fetch: bool, upstream_deleted: bool) -> Result<()> {
    let Response::Workspaces { workspaces } =
        client::send(&Request::List).wrap_err("gc needs ftmd running to see which worktrees have open workspaces")?
    else {
        bail!("unexpected response from ftmd");
    };
    let root = paths::worktrees_dir();
    let mut by_repo: BTreeMap<PathBuf, Vec<Found>> = BTreeMap::new();
    for path in find_worktrees(&root) {
        let shown = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
        match main_repo(&path) {
            Some(repo) => by_repo.entry(repo).or_default().push(Found { path, shown }),
            None => println!("skip    {shown}  (not a working git worktree)"),
        }
    }

    let (mut removable, mut failed) = (0, 0);
    for (repo, found) in by_repo {
        let config = RepoConfig::load(&paths::repo_config_path(&repo_name(&repo)))?;
        let base = config
            .base
            .or_else(|| remote_default(&repo))
            .unwrap_or_else(|| DEFAULT_BASE.into());

        let live = if fetch { fetch_base(&repo, &base) } else { None };
        let mut removed_any = false;
        for wt in found {
            match verdict(&repo, &wt.path, &base, &workspaces, live.as_ref(), upstream_deleted) {
                Verdict::Keep => {},
                Verdict::Skip(why) => println!("skip    {}  ({why})", wt.shown),
                Verdict::Remove { branch, stale, why } => {
                    removable += 1;
                    if !yes {
                        println!("remove  {}  ({why})", wt.shown);
                        continue;
                    }
                    match remove_worktree(&repo, &wt.path, &root) {
                        Ok(()) => {
                            removed_any = true;
                            println!("removed {}  ({why})", wt.shown);
                            if let Err(e) = delete_branch(&repo, &branch, stale.as_deref()) {
                                failed += 1;
                                eprintln!("failed  {}  (worktree removed, but: {e:#})", wt.shown);
                            }
                        },
                        Err(e) => {
                            failed += 1;
                            eprintln!("failed  {}  ({e:#})", wt.shown);
                        },
                    }
                },
            }
        }
        if removed_any && let Err(e) = git(&repo, &["worktree", "prune"]) {
            eprintln!("{}: {e:#}", repo.display());
        }
    }
    if !yes && removable > 0 {
        println!("dry run: pass --yes to remove {removable} worktree(s) and their branches");
    }
    if failed > 0 {
        bail!("{failed} worktree(s) could not be removed");
    }
    Ok(())
}

/// Directories under `root` holding a `.git` file or dir; nothing below one is searched.
fn find_worktrees(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir != root && dir.join(".git").symlink_metadata().is_ok() {
            found.push(dir);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

/// The branches that exist on a remote right now.
struct Live {
    remote: String,
    heads: HashSet<String>,
}

/// Fetches the base's remote without pruning, so the remote-tracking ref of a branch deleted there still holds the
/// last pushed tip, and lists the remote's branches to see which were deleted.
fn fetch_base(repo: &Path, base: &str) -> Option<Live> {
    let remotes = git(repo, &["remote"]).unwrap_or_default();
    let remote = base
        .split_once('/')
        .map(|(r, _)| r)
        .filter(|r| remotes.lines().any(|l| l == *r))?;
    if let Err(e) = git(repo, &["fetch", "--quiet", remote]) {
        eprintln!("{}: fetch failed, using the last fetch: {e:#}", repo.display());
        return None;
    }
    let heads = git(repo, &["ls-remote", "--heads", remote]).ok()?;
    Some(Live {
        remote: remote.to_string(),
        heads: heads
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, r)| r.to_string()))
            .collect(),
    })
}

enum Upstream {
    /// No upstream, or one that still exists.
    Present,
    /// Deleted on the remote; the remote-tracking ref still holds its last fetched tip.
    Deleted { tracking: String, tip: String },
    /// Deleted and already pruned, so what was pushed is unknown.
    Pruned,
}

fn upstream(repo: &Path, branch: &str, live: Option<&Live>) -> Upstream {
    let Ok(info) = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(upstream)%00%(upstream:remotename)%00%(upstream:remoteref)",
            &format!("refs/heads/{branch}"),
        ],
    ) else {
        return Upstream::Present;
    };
    let mut parts = info.split('\0');
    let (Some(tracking), Some(remote), Some(remote_ref)) = (parts.next(), parts.next(), parts.next()) else {
        return Upstream::Present;
    };
    if !tracking.starts_with("refs/remotes/") {
        return Upstream::Present;
    }
    let Ok(tip) = git(repo, &["rev-parse", "--verify", "-q", tracking]) else {
        return Upstream::Pruned;
    };
    match live {
        Some(live) if live.remote == remote && !live.heads.contains(remote_ref) => Upstream::Deleted {
            tracking: tracking.to_string(),
            tip,
        },
        _ => Upstream::Present,
    }
}

fn verdict(
    repo: &Path,
    wt: &Path,
    base: &str,
    workspaces: &[Workspace],
    live: Option<&Live>,
    upstream_deleted: bool,
) -> Verdict {
    let Ok(branch) = git(wt, &["symbolic-ref", "--short", "-q", "HEAD"]) else {
        return Verdict::Keep;
    };
    let Ok(head) = git(wt, &["rev-parse", "HEAD"]) else {
        return Verdict::Skip("no commits".into());
    };
    if !ok(repo, &["rev-parse", "--verify", "-q", base]) {
        return Verdict::Skip(format!("base {base} not found"));
    }
    let upstream = upstream(repo, &branch, live);
    let mut stale = None;
    let why = if ok(repo, &["merge-base", "--is-ancestor", &head, base]) {
        format!("merged into {base}")
    } else if squash_merged(repo, &head, base) {
        format!("squash-merged into {base}")
    } else {
        match upstream {
            Upstream::Present => return Verdict::Keep,
            Upstream::Pruned => {
                return Verdict::Skip("upstream branch deleted and pruned, cannot check for unpushed commits".into());
            },
            Upstream::Deleted { tip, .. } if tip != head => {
                return Verdict::Skip("upstream branch deleted, local commits never pushed".into());
            },
            // A PR closed unmerged looks the same, and its commits would then live only in the reflog.
            Upstream::Deleted { .. } if !upstream_deleted => {
                return Verdict::Skip(
                    "upstream branch deleted, everything pushed; --include-upstream-deleted removes it".into(),
                );
            },
            Upstream::Deleted { tracking, .. } => {
                stale = Some(tracking);
                "upstream branch deleted, everything pushed".into()
            },
        }
    };

    if let Some(ws) = workspaces.iter().find(|w| same_or_inside(&w.cwd, wt)) {
        return Verdict::Skip(format!("open workspace {}", ws.display_name()));
    }
    match git(wt, &["status", "--porcelain"]) {
        Ok(s) if !s.is_empty() => return Verdict::Skip("uncommitted or untracked changes".into()),
        Err(e) => return Verdict::Skip(format!("{e:#}")),
        Ok(_) => {},
    }
    Verdict::Remove { branch, stale, why }
}

/// The branch's whole diff from where it left `base` already landed in `base` as one commit, as a squash merge does.
fn squash_merged(repo: &Path, head: &str, base: &str) -> bool {
    let Ok(fork) = git(repo, &["merge-base", base, head]) else {
        return false;
    };
    let Ok(tree) = git(repo, &["rev-parse", &format!("{head}^{{tree}}")]) else {
        return false;
    };
    let Ok(squashed) = git(repo, &["commit-tree", &tree, "-p", &fork, "-m", "ftm gc"]) else {
        return false;
    };
    git(repo, &["cherry", base, &squashed]).is_ok_and(|c| c.starts_with('-'))
}

fn same_or_inside(path: &Path, dir: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(path).starts_with(canon(dir))
}

/// Never forced: `git worktree remove` refuses a dirty worktree.
fn remove_worktree(repo: &Path, wt: &Path, root: &Path) -> Result<()> {
    git(repo, &["worktree", "remove", &wt.to_string_lossy()])?;
    let mut dir = wt.parent();
    while let Some(d) = dir
        && d.starts_with(root)
        && d != root
        && std::fs::remove_dir(d).is_ok()
    {
        dir = d.parent();
    }
    Ok(())
}

/// Force-deleted since squash merges are not ancestors of the base; `verdict` has already checked it.
fn delete_branch(repo: &Path, branch: &str, stale: Option<&str>) -> Result<()> {
    git(repo, &["branch", "-D", branch])?;
    if let Some(tracking) = stale {
        git(repo, &["update-ref", "-d", tracking])?;
    }
    Ok(())
}
