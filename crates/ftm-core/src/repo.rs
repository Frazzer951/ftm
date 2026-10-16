use std::path::{Path, PathBuf};
use std::process::Command;

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
    fn names_drop_a_git_suffix() {
        assert_eq!(repo_name(Path::new("/src/ftm")), "ftm");
        assert_eq!(repo_name(Path::new("/src/ftm.git")), "ftm");
        assert_eq!(repo_name(Path::new("/src/.git")), ".git");
    }
}
