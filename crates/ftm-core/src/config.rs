use serde::{Deserialize, Serialize};
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

/// App settings from `config.toml` in the config dir.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub terminal: TerminalSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// A font family name, or a CSS font-family list.
    pub font_family: String,
    pub font_size: f32,
    pub ligatures: bool,
    /// Multiple of the font's natural line height.
    pub line_height: f32,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        TerminalSettings {
            font_family: "Menlo, \"DejaVu Sans Mono\", monospace".into(),
            font_size: 13.0,
            ligatures: false,
            line_height: 1.0,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let display = path.display().to_string();
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| ConfigError::Parse { path: display, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io { path: display, source }),
        }
    }

    /// Rewrites only the keys this struct owns, so other sections and keys in the file survive.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let display = path.display().to_string();
        let io = |source| ConfigError::Io {
            path: display.clone(),
            source,
        };
        let mut doc: toml::Table = match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| ConfigError::Parse {
                path: display.clone(),
                source,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(io(e)),
        };
        let ours = toml::Table::try_from(self).expect("settings serialize to a table");
        for (section, values) in ours {
            match (doc.get_mut(&section), values) {
                (Some(toml::Value::Table(existing)), toml::Value::Table(values)) => existing.extend(values),
                (_, values) => {
                    doc.insert(section, values);
                },
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        std::fs::write(path, toml::to_string_pretty(&doc).expect("toml table serializes")).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_keep_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("ftm-settings-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "[terminal]\nfont_size = 15\nfuture = \"keep\"\n\n[other]\nx = 1\n").unwrap();
        let mut settings = Settings::load(&path).unwrap();
        assert_eq!(settings.terminal.font_size, 15.0);
        assert!(!settings.terminal.ligatures);

        settings.terminal.font_family = "FiraCode Nerd Font Mono".into();
        settings.terminal.ligatures = true;
        settings.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future = \"keep\"") && text.contains("[other]"), "{text}");
        assert_eq!(Settings::load(&path).unwrap(), settings);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
