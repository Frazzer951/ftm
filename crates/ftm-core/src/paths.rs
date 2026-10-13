use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/ftm/ftmd.sock` on Linux; macOS has no runtime dir, so it falls back to the cache dir.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var(crate::ENV_SOCKET) {
        return PathBuf::from(p);
    }
    let base = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir);
    base.join("ftm").join("ftmd.sock")
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config").join("ftm")
}

pub fn repo_config_path(repo_name: &str) -> PathBuf {
    config_dir().join("repos").join(format!("{repo_name}.toml"))
}

/// `$XDG_DATA_HOME/ftm/worktrees`, else `~/.local/share/ftm/worktrees` on both platforms; macOS's own data dir has a
/// space in it, which trips up shell scripts run inside worktrees.
pub fn worktrees_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local").join("share"))
        .join("ftm")
        .join("worktrees")
}
