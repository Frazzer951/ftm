use eyre::{Result, WrapErr, bail};
use ftm_core::{config::RepoConfig, paths};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Created {
    pub path: PathBuf,
    pub repo: PathBuf,
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

pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).ok().map(PathBuf::from)
}

pub fn create(cwd: &Path, branch: &str, base: Option<&str>, copy_changes: bool) -> Result<Created> {
    let repo = repo_root(cwd).ok_or_else(|| eyre::eyre!("{} is not in a git repo", cwd.display()))?;
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let config = RepoConfig::load(&paths::repo_config_path(&name))?;

    let path = paths::worktrees_dir().join(&name).join(branch);
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let path_str = path.to_string_lossy().into_owned();
    if git(&repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok() {
        git(&repo, &["worktree", "add", &path_str, branch])?;
    } else {
        let mut args = vec!["worktree", "add", "-b", branch, &path_str];
        if let Some(base) = base.or(config.base.as_deref()) {
            args.push(base);
        }
        git(&repo, &args)?;
    }

    if copy_changes {
        copy_tracked_changes(&repo, &path)?;
        let untracked = git(&repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        copy_files(
            &repo,
            &path,
            untracked.split('\0').filter(|s| !s.is_empty()).map(PathBuf::from),
        )?;
    }
    for pattern in &config.copy_ignored {
        let full = repo.join(pattern).to_string_lossy().into_owned();
        let matches = glob::glob(&full).wrap_err_with(|| format!("bad copy_ignored pattern {pattern}"))?;
        let rel = matches
            .flatten()
            .filter_map(|p| p.strip_prefix(&repo).ok().map(Path::to_path_buf));
        copy_files(&repo, &path, rel)?;
    }

    Ok(Created {
        path,
        repo,
        setup: config.setup,
    })
}

fn copy_tracked_changes(repo: &Path, dest: &Path) -> Result<()> {
    let diff = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "HEAD", "--binary"])
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

fn copy_files(from: &Path, to: &Path, rel: impl Iterator<Item = PathBuf>) -> Result<()> {
    for rel in rel {
        let src = from.join(&rel);
        let dst = to.join(&rel);
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else if src.is_file() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst).wrap_err_with(|| format!("copying {}", rel.display()))?;
        }
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let (s, d) = (entry.path(), dst.join(entry.file_name()));
        if s.is_dir() {
            copy_dir(&s, &d)?
        } else {
            std::fs::copy(&s, &d).map(|_| ())?
        }
    }
    Ok(())
}

pub fn remove(repo: &Path, path: &Path) -> Result<()> {
    git(repo, &["worktree", "remove", &path.to_string_lossy()]).map(|_| ())
}
