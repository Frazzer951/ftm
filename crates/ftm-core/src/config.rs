use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-repo settings from `~/.config/ftm/repos/<repo>.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RepoConfig {
    pub base: Option<String>,
    pub setup: Option<String>,
    /// Which gitignored paths to copy into new worktrees. Untracked, non-ignored files are always copied.
    pub copy_ignored: CopyIgnored,
    /// Names or globs skipped on top of [`DEFAULT_COPY_IGNORED_EXCLUDE`] when `copy_ignored` is `true`.
    pub copy_ignored_exclude: Vec<String>,
}

/// Skipped at any depth when copying all gitignored paths: big, rebuildable, or machine-specific.
pub const DEFAULT_COPY_IGNORED_EXCLUDE: &[&str] = &[
    ".venv",
    "venv",
    "node_modules",
    "target",
    "__pycache__",
    ".pytest_cache",
    ".ruff_cache",
    ".mypy_cache",
    ".tox",
    ".DS_Store",
];

/// `copy_ignored = true` (the default) copies every gitignored path except the excludes, `false` copies none, and a
/// list of globs copies only the paths they match, with no excludes applied.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CopyIgnored {
    All(bool),
    Only(Vec<String>),
}

impl Default for CopyIgnored {
    fn default() -> Self {
        CopyIgnored::All(true)
    }
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
    pub theme: ThemeSettings,
    pub sounds: SoundSettings,
    pub updates: UpdateSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Look for a newer release. Unset means only when installed from the release's `PKGBUILD` (pacman `ftm-bin`),
    /// the one install the app can upgrade by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<bool>,
    /// The channel to update from. Unset means this build's own; the other one is offered as a switch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    /// The channel this build was made for.
    pub fn current() -> Self {
        if crate::paths::CHANNEL == "nightly" {
            Channel::Nightly
        } else {
            Channel::Stable
        }
    }
}

/// Keys the app owns that are left out while unset, so saving has to remove them rather than leave the old value.
const OPTIONAL_KEYS: &[(&str, &str)] = &[("updates", "check"), ("updates", "channel")];

/// Sounds the app plays on status changes, by name from its sound list; an empty name plays nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundSettings {
    /// An agent started waiting on you.
    pub needs_input: String,
    /// An agent finished its turn while you were not looking at it.
    pub completed: String,
    /// 0 to 1.
    pub volume: f32,
}

impl Default for SoundSettings {
    fn default() -> Self {
        SoundSettings {
            needs_input: "Nudge".into(),
            completed: "Chime".into(),
            volume: 0.7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    /// Used unless `follow_system` is set.
    pub name: String,
    /// Switch between `dark` and `light` with the system appearance.
    pub follow_system: bool,
    pub dark: String,
    pub light: String,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        ThemeSettings {
            name: crate::theme::DEFAULT_DARK.into(),
            follow_system: false,
            dark: crate::theme::DEFAULT_DARK.into(),
            light: crate::theme::DEFAULT_LIGHT.into(),
        }
    }
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
        for (section, key) in OPTIONAL_KEYS {
            if let Some(toml::Value::Table(existing)) = doc.get_mut(*section) {
                existing.remove(*key);
                if existing.is_empty() {
                    doc.remove(*section);
                }
            }
        }
        for (section, values) in ours {
            match (doc.get_mut(&section), values) {
                (Some(toml::Value::Table(existing)), toml::Value::Table(values)) => existing.extend(values),
                (None, toml::Value::Table(values)) if values.is_empty() => {},
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
    fn copy_ignored_forms() {
        let parse = |s: &str| toml::from_str::<RepoConfig>(s).unwrap();
        assert_eq!(parse("").copy_ignored, CopyIgnored::All(true));
        assert_eq!(parse("copy_ignored = false").copy_ignored, CopyIgnored::All(false));
        assert_eq!(
            parse("copy_ignored = [\".env\"]").copy_ignored,
            CopyIgnored::Only(vec![".env".into()])
        );
        let c = parse("copy_ignored_exclude = [\"data\", \"*.parquet\"]");
        assert_eq!(c.copy_ignored, CopyIgnored::All(true));
        assert_eq!(c.copy_ignored_exclude, ["data", "*.parquet"]);
    }

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

        assert_eq!(settings.theme.name, "Supacode Dark");
        settings.terminal.font_family = "FiraCode Nerd Font Mono".into();
        settings.terminal.ligatures = true;
        settings.theme = ThemeSettings {
            name: "Dracula".into(),
            follow_system: true,
            dark: "Nord".into(),
            light: "Supacode Light".into(),
        };
        assert_eq!(settings.sounds.completed, "Chime");
        settings.sounds.needs_input = String::new();
        settings.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future = \"keep\"") && text.contains("[other]"), "{text}");
        assert!(text.contains("[theme]") && text.contains("follow_system = true"), "{text}");
        assert!(text.contains("needs_input = \"\""), "{text}");
        assert!(!text.contains("[updates]"), "{text}");
        assert_eq!(Settings::load(&path).unwrap(), settings);

        std::fs::write(&path, "[updates]\ncheck = false\n").unwrap();
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.updates.check, Some(false));
        assert_eq!(settings.updates.channel, None);

        std::fs::write(&path, "[updates]\ncheck = true\nchannel = \"nightly\"\nfuture = 1\n").unwrap();
        let mut settings = Settings::load(&path).unwrap();
        assert_eq!(settings.updates.check, Some(true));
        assert_eq!(settings.updates.channel, Some(Channel::Nightly));
        settings.updates.channel = Some(Channel::Stable);
        settings.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("channel = \"stable\"") && text.contains("future = 1"), "{text}");
        assert_eq!(Settings::load(&path).unwrap(), settings);

        // Unset again: the keys go, and so does a section left empty.
        Settings::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("check") && !text.contains("channel") && text.contains("future = 1"),
            "{text}"
        );
        std::fs::write(&path, "[updates]\ncheck = false\n").unwrap();
        Settings::default().save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("[updates]"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
