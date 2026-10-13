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

/// Creates `dir` (and its parents) owner-only, so other local users cannot reach the sockets in it. An existing dir
/// is tightened to 0700 only when `ours`: a socket override may point into a shared dir like `/tmp` or `$HOME`.
pub fn private_dir(dir: &Path, ours: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let existed = dir.is_dir();
    std::fs::create_dir_all(dir)?;
    if !existed || ours {
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// The default socket lives in a dir named `ftm` that FTM owns; an overridden one may not.
pub fn socket_dir_is_ours(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == "ftm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_dirs_are_owner_only_but_shared_parents_are_left_alone() {
        let root = std::env::temp_dir().join(format!("ftm-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let fresh = root.join("a/b");
        private_dir(&fresh, false).unwrap();
        assert_eq!(mode(&fresh), 0o700);

        let shared = root.join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&shared, false).unwrap();
        assert_eq!(mode(&shared), 0o755);
        private_dir(&shared, true).unwrap();
        assert_eq!(mode(&shared), 0o700);

        assert!(socket_dir_is_ours(Path::new("/Users/x/Library/Caches/ftm")));
        assert!(!socket_dir_is_ours(Path::new("/tmp")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
