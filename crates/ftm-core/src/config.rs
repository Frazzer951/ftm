use serde::Deserialize;
use std::path::Path;

/// Per-repo settings from `~/.config/ftm/repos/<repo>.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RepoConfig {
    pub base: Option<String>,
    pub setup: Option<String>,
    /// Gitignored paths (globs) to copy into new worktrees, e.g. `.env`. Untracked, non-ignored files are always copied.
    pub copy_ignored: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("parsing {path}: {source}")]
    Parse { path: String, source: toml::de::Error },
}

impl RepoConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let display = path.display().to_string();
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| ConfigError::Parse { path: display, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io { path: display, source }),
        }
    }
}
