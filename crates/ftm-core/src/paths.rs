use std::path::{Path, PathBuf};

/// The dir the default socket lives in: `ftm`, or `ftm-nightly` in a build made with `FTM_CHANNEL=nightly`, so a
/// nightly install runs its own daemon next to the stable one.
pub const SOCKET_DIR_NAME: &str = socket_dir_name(option_env!("FTM_CHANNEL"));

/// The release channel this build was made for: `stable`, or `nightly` with `FTM_CHANNEL=nightly`.
pub const CHANNEL: &str = channel_name(option_env!("FTM_CHANNEL"));

const fn channel_name(channel: Option<&str>) -> &'static str {
    match socket_dir_name(channel).as_bytes() {
        b"ftm" => "stable",
        _ => "nightly",
    }
}

const fn socket_dir_name(channel: Option<&str>) -> &'static str {
    match channel {
        None => "ftm",
        Some(c) if c.is_empty() => "ftm",
        Some(c) if c.eq_ignore_ascii_case("nightly") => "ftm-nightly",
        Some(_) => panic!("FTM_CHANNEL must be unset or \"nightly\""),
    }
}

/// `$XDG_RUNTIME_DIR/<SOCKET_DIR_NAME>/ftmd.sock` on Linux; macOS has no runtime dir, so it falls back to the cache dir.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var(crate::ENV_SOCKET) {
        return PathBuf::from(p);
    }
    default_socket_path()
}

fn default_socket_path() -> PathBuf {
    let base = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir);
    base.join(SOCKET_DIR_NAME).join("ftmd.sock")
}

/// `$XDG_CONFIG_HOME/ftm`, else `~/.config/ftm` on both platforms.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("ftm")
}

/// App settings: `config.toml` in the config dir.
pub fn settings_path() -> PathBuf {
    config_dir().join("config.toml")
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

/// The default socket lives in a dir named [`SOCKET_DIR_NAME`] that FTM owns; an overridden one may not.
pub fn socket_dir_is_ours(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == SOCKET_DIR_NAME)
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

        assert!(socket_dir_is_ours(
            &Path::new("/Users/x/Library/Caches").join(SOCKET_DIR_NAME)
        ));
        assert!(!socket_dir_is_ours(Path::new("/tmp")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nightly_builds_default_to_their_own_socket_dir() {
        assert_eq!(socket_dir_name(None), "ftm");
        assert_eq!(socket_dir_name(Some("")), "ftm");
        assert_eq!(socket_dir_name(Some("nightly")), "ftm-nightly");
        assert_eq!(channel_name(None), "stable");
        assert_eq!(channel_name(Some("")), "stable");
        assert_eq!(channel_name(Some("nightly")), "nightly");

        let (ours, other) = match option_env!("FTM_CHANNEL") {
            Some("nightly") => ("ftm-nightly", "ftm"),
            _ => ("ftm", "ftm-nightly"),
        };
        let socket = default_socket_path();
        assert_eq!(socket.file_name().unwrap(), "ftmd.sock");
        let dir = socket.parent().unwrap();
        assert_eq!(dir.file_name().unwrap(), ours);
        assert!(socket_dir_is_ours(dir));
        assert!(!socket_dir_is_ours(&dir.with_file_name(other)));
    }
}
