use std::path::{Path, PathBuf};

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

/// `$XDG_CONFIG_HOME/ftm`, else `~/.config/ftm` on both platforms.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("ftm")
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

/// Pane holder sockets for the daemon on `socket`: `<socket>.d/<pane>.sock`, short enough for the ~104 byte limit.
pub fn holders_dir(socket: &Path) -> PathBuf {
    let mut dir = socket.as_os_str().to_owned();
    dir.push(".d");
    PathBuf::from(dir)
}

/// The workspace tree saved by the daemon on `socket`, under `$XDG_STATE_HOME/ftm` (else `~/.local/state/ftm`),
/// keyed by socket path so a dev or test daemon never picks up another daemon's panes.
pub fn state_file(socket: &Path) -> PathBuf {
    let key: String = socket
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local").join("state"))
        .join("ftm")
        .join(format!("{key}.json"))
}
