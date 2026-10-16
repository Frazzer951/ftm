use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Talking to a remote can hang on the network; callers wait for this at most before going on without it.
pub const REMOTE_TIMEOUT: Duration = Duration::from_secs(60);

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Runs a git command that talks to a remote, never prompting and giving up after `REMOTE_TIMEOUT`. Its output is
/// read as it comes, so a git that prints more than a pipe holds does not stall until the timeout.
pub fn git_remote(dir: &Path, args: &[&str]) -> std::io::Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + REMOTE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // The readers are left behind: a child of git (ssh) may still hold the pipes open.
            return Err(std::io::Error::other(format!(
                "git {}: timed out after {}s",
                args.join(" "),
                REMOTE_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&stdout).trim_end().to_string())
}

/// The remote a repo's work is based on: the current branch's upstream remote, else `origin`.
fn default_remote(dir: &Path) -> Option<String> {
    let remotes = git(dir, &["remote"])?;
    let upstream = git(dir, &["symbolic-ref", "--short", "-q", "HEAD"])
        .and_then(|head| git(dir, &["config", &format!("branch.{head}.remote")]));
    upstream
        .into_iter()
        .chain(["origin".to_string()])
        .find(|r| remotes.lines().any(|l| l == r))
}

/// The remote's default branch as a remote-tracking ref (`origin/main`), or `None` for a repo with no remote. Asks
/// the remote (`ls-remote`, up to `REMOTE_TIMEOUT`) only when `refs/remotes/<remote>/HEAD` is not set.
pub fn remote_default(dir: &Path) -> Option<String> {
    let remote = default_remote(dir)?;
    if let Some(head) = git(
        dir,
        &["symbolic-ref", "--short", "-q", &format!("refs/remotes/{remote}/HEAD")],
    ) {
        return Some(head);
    }
    let listed = git_remote(dir, &["ls-remote", "--symref", &remote, "HEAD"]).unwrap_or_default();
    let symref = listed.lines().find_map(|l| {
        l.strip_prefix("ref: refs/heads/")
            .and_then(|r| r.strip_suffix("\tHEAD"))
            .map(|b| format!("{remote}/{b}"))
    });
    symref.or_else(|| {
        ["main", "master"]
            .into_iter()
            .map(|b| format!("{remote}/{b}"))
            .find(|r| git(dir, &["rev-parse", "--verify", "-q", &format!("refs/remotes/{r}")]).is_some())
    })
}

/// The repo the checkout `dir` is in belongs to, the same from every one of its worktrees: the main checkout when the
/// common git dir is its `.git`, else the common git dir itself. That is a bare repo, or one made with
/// `--separate-git-dir`, whose main checkout git does not record. Git commands run in either.
pub fn main_repo(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim_end());
    match common.parent() {
        Some(parent) if common.file_name().is_some_and(|n| n == ".git") => Some(parent.to_path_buf()),
        _ => Some(common),
    }
}

/// What a repo's config file and worktrees folder are named after: its directory, without a `.git` suffix.
pub fn repo_name(repo: &Path) -> String {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.strip_suffix(".git") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_git_output_bigger_than_a_pipe_does_not_stall() {
        let started = Instant::now();
        let out = git_remote(&std::env::temp_dir(), &["-c", "alias.big=!yes | head -c 1000000", "big"]).unwrap();
        assert_eq!(out.len(), 999_999);
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn names_drop_a_git_suffix() {
        assert_eq!(repo_name(Path::new("/src/ftm")), "ftm");
        assert_eq!(repo_name(Path::new("/src/ftm.git")), "ftm");
        assert_eq!(repo_name(Path::new("/src/.git")), ".git");
    }
}
