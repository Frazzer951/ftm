use eyre::{Result, WrapErr, bail};
use ftm_core::config::{CopyIgnored, DEFAULT_COPY_IGNORED_EXCLUDE, RepoConfig};
use ftm_core::{paths, repo};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Created {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub branch: String,
    new_branch: bool,
    pub setup: Option<String>,
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

/// The checkout `dir` is in, and the repo it belongs to, so a worktree made from inside another worktree is named,
/// configured and removed by that repo.
fn checkouts(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let src = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"]).ok()?);
    Some((src, repo::main_repo(dir)?))
}

pub fn create(cwd: &Path, branch: &str, base: Option<&str>, copy_changes: bool) -> Result<Created> {
    let (src, repo) = checkouts(cwd).ok_or_else(|| eyre::eyre!("{} is not in a git repo", cwd.display()))?;
    let name = repo::repo_name(&repo);
    let config = RepoConfig::load(&paths::repo_config_path(&name))?;
    let excludes = match &config.copy_ignored {
        CopyIgnored::All(true) => Excludes::new(&config.copy_ignored_exclude)?,
        _ => Excludes::default(),
    };
    if let CopyIgnored::Only(patterns) = &config.copy_ignored {
        for pattern in patterns {
            glob::Pattern::new(&src.join(pattern).to_string_lossy())
                .wrap_err_with(|| format!("bad copy_ignored pattern {pattern}"))?;
        }
    }

    let path = paths::worktrees_dir().join(&name).join(branch);
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let path_str = path.to_string_lossy().into_owned();
    let new_branch = git(&src, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_err();
    if new_branch {
        let mut args = vec!["worktree", "add", "-b", branch, &path_str];
        if let Some(base) = base.or(config.base.as_deref()) {
            args.push(base);
        }
        git(&src, &args)?;
    } else {
        git(&src, &["worktree", "add", &path_str, branch])?;
    }

    let created = Created {
        path,
        repo,
        branch: branch.to_string(),
        new_branch,
        setup: config.setup.clone(),
    };
    if let Err(e) = fill(&src, &created.path, &config, &excludes, copy_changes) {
        created.discard();
        return Err(e);
    }
    Ok(created)
}

impl Created {
    /// Undoes a `create` whose workspace never opened: force-removes the worktree, and its branch if `create` made it.
    pub fn discard(&self) {
        if let Err(e) = git(&self.repo, &["worktree", "remove", "--force", &self.path.to_string_lossy()]) {
            tracing::warn!("removing {}: {e:#}", self.path.display());
        }
        if self.new_branch
            && let Err(e) = git(&self.repo, &["branch", "-D", &self.branch])
        {
            tracing::warn!("deleting branch {}: {e:#}", self.branch);
        }
        prune_empty_parents(&self.path);
    }
}

/// Copies the source checkout's changes (with `copy_changes`) and its configured ignored files into `path`.
fn fill(src: &Path, path: &Path, config: &RepoConfig, excludes: &Excludes, copy_changes: bool) -> Result<()> {
    let mut copied = Copied::default();
    if copy_changes {
        copy_tracked_changes(src, path)?;
        let untracked = git(src, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for rel in split_nul(&untracked) {
            copy_entry(src, path, &rel, &Excludes::default(), &mut copied);
        }
    }
    match &config.copy_ignored {
        CopyIgnored::All(false) => {},
        CopyIgnored::All(true) => {
            let ignored = git(
                src,
                &["ls-files", "--others", "--ignored", "--exclude-standard", "--directory", "-z"],
            )?;
            for rel in split_nul(&ignored) {
                copy_entry(src, path, &rel, excludes, &mut copied);
            }
        },
        CopyIgnored::Only(patterns) => {
            for pattern in patterns {
                let full = src.join(pattern).to_string_lossy().into_owned();
                let matches = glob::glob(&full).wrap_err_with(|| format!("bad copy_ignored pattern {pattern}"))?;
                for rel in matches
                    .flatten()
                    .filter_map(|p| p.strip_prefix(src).ok().map(Path::to_path_buf))
                {
                    copy_entry(src, path, &rel, &Excludes::default(), &mut copied);
                }
            }
        },
    }
    tracing::info!(
        "copied {} files ({} KiB) into {}, skipped {}, failed {}",
        copied.files,
        copied.bytes / 1024,
        path.display(),
        copied.skipped,
        copied.failed
    );
    Ok(())
}

fn copy_tracked_changes(repo: &Path, dest: &Path) -> Result<()> {
    let diff = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff-index", "-p", "--binary", "HEAD"])
        .output()?;
    if !diff.status.success() || diff.stdout.is_empty() {
        return Ok(());
    }
    let mut apply = Command::new("git")
        .arg("-C")
        .arg(dest)
        .args(["apply", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::io::Write::write_all(apply.stdin.as_mut().expect("piped stdin"), &diff.stdout)?;
    let out = apply.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "applying changes to worktree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[derive(Default)]
struct Copied {
    files: u64,
    bytes: u64,
    skipped: u64,
    failed: u64,
}

/// Names or globs matched against every component of a repo-relative path, and against the whole path.
#[derive(Default)]
struct Excludes(Vec<glob::Pattern>);

impl Excludes {
    fn new(extra: &[String]) -> Result<Self> {
        let all = DEFAULT_COPY_IGNORED_EXCLUDE
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str));
        all.map(|p| glob::Pattern::new(p).wrap_err_with(|| format!("bad copy_ignored_exclude pattern {p}")))
            .collect::<Result<_>>()
            .map(Excludes)
    }

    fn matches(&self, rel: &Path) -> bool {
        self.0
            .iter()
            .any(|p| p.matches_path(rel) || rel.components().any(|c| p.matches(&c.as_os_str().to_string_lossy())))
    }
}

fn split_nul(s: &str) -> impl Iterator<Item = PathBuf> + '_ {
    s.split('\0').filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// Copies `from/rel` to `to/rel`, recursing into directories. Symlinks are recreated, never followed; nested
/// repositories and worktrees (a directory holding `.git`) and special files are skipped. On macOS `fs::copy` clones
/// on APFS and falls back to a byte copy; either way the file mode is kept. Best effort: a path that fails to copy is
/// logged and counted, and the rest are still copied.
fn copy_entry(from: &Path, to: &Path, rel: &Path, excludes: &Excludes, copied: &mut Copied) {
    if let Err(e) = try_copy_entry(from, to, rel, excludes, copied) {
        tracing::warn!("not copying {}: {e:#}", rel.display());
        copied.failed += 1;
    }
}

fn try_copy_entry(from: &Path, to: &Path, rel: &Path, excludes: &Excludes, copied: &mut Copied) -> Result<()> {
    if excludes.matches(rel) {
        tracing::debug!("not copying excluded {}", rel.display());
        copied.skipped += 1;
        return Ok(());
    }
    let (src, dst) = (from.join(rel), to.join(rel));
    let Ok(meta) = std::fs::symlink_metadata(&src) else {
        return Ok(());
    };
    let kind = meta.file_type();
    if kind.is_dir() && src.join(".git").symlink_metadata().is_ok() {
        tracing::debug!("not copying nested repository {}", rel.display());
        copied.skipped += 1;
        return Ok(());
    }
    if !(kind.is_dir() || kind.is_file() || kind.is_symlink()) {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if kind.is_symlink() {
        let target = std::fs::read_link(&src)?;
        match std::os::unix::fs::symlink(&target, &dst) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e.into()),
            _ => copied.files += 1,
        }
    } else if kind.is_dir() {
        std::fs::create_dir_all(&dst)?;
        for entry in std::fs::read_dir(&src)? {
            match entry {
                Ok(entry) => copy_entry(from, to, &rel.join(entry.file_name()), excludes, copied),
                Err(e) => {
                    tracing::warn!("listing {}: {e}", rel.display());
                    copied.failed += 1;
                },
            }
        }
        std::fs::set_permissions(&dst, meta.permissions())?;
    } else {
        copied.bytes += std::fs::copy(&src, &dst)?;
        copied.files += 1;
    }
    Ok(())
}

/// Removes the worktree (never forced), then deletes its branch only if it is merged, and prunes the empty
/// directories a `prefix/name` branch left under the worktrees dir.
pub fn remove(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    git(repo, &["worktree", "remove", &path.to_string_lossy()])?;
    if let Err(e) = git(repo, &["branch", "-d", branch]) {
        tracing::info!("keeping branch {branch}: {e:#}");
    }
    prune_empty_parents(path);
    Ok(())
}

fn prune_empty_parents(path: &Path) {
    let root = paths::worktrees_dir();
    let mut dir = path.parent();
    while let Some(d) = dir
        && d.starts_with(&root)
        && d != root
        && std::fs::remove_dir(d).is_ok()
    {
        dir = d.parent();
    }
}
