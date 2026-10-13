//! Color themes in Ghostty's format: `key = value` lines, `palette = N=#rrggbb` for the 16 ANSI colors.

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

pub const DEFAULT_DARK: &str = "Supacode Dark";
pub const DEFAULT_LIGHT: &str = "Supacode Light";

/// xterm's colors, for palette entries a theme leaves out.
const XTERM_PALETTE: [&str; 16] = [
    "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5", "#7f7f7f", "#ff0000", "#00ff00",
    "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Theme {
    pub name: String,
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub cursor_text: String,
    pub selection_background: String,
    pub selection_foreground: Option<String>,
    /// ANSI 0-15 as `#rrggbb`.
    pub palette: Vec<String>,
}

/// `#rrggbb` (lowercase) from `#rrggbb`, `rrggbb` or `#rgb`; other color syntaxes are not supported.
fn hex(value: &str) -> Option<String> {
    let digits = value.trim().trim_start_matches('#');
    let full = match digits.len() {
        6 => digits.to_string(),
        3 => digits.chars().flat_map(|c| [c, c]).collect(),
        _ => return None,
    };
    full.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| format!("#{}", full.to_lowercase()))
}

impl Theme {
    /// Unknown keys, comments and malformed lines are ignored; missing colors get sensible defaults.
    pub fn parse(name: &str, text: &str) -> Theme {
        let mut palette: Vec<String> = XTERM_PALETTE.iter().map(|c| c.to_string()).collect();
        let mut keys: HashMap<&str, String> = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim());
            if key == "palette" {
                if let Some((index, color)) = value.split_once('=')
                    && let Ok(index) = index.trim().parse::<usize>()
                    && let Some(color) = hex(color)
                    && index < 16
                {
                    palette[index] = color;
                }
            } else if let Some(color) = hex(value) {
                keys.insert(key, color);
            }
        }
        let background = keys.remove("background").unwrap_or_else(|| "#1e1e1e".into());
        let foreground = keys.remove("foreground").unwrap_or_else(|| "#ffffff".into());
        Theme {
            name: name.to_string(),
            cursor: keys.remove("cursor-color").unwrap_or_else(|| foreground.clone()),
            cursor_text: keys.remove("cursor-text").unwrap_or_else(|| background.clone()),
            selection_background: keys.remove("selection-background").unwrap_or_else(|| palette[8].clone()),
            selection_foreground: keys.remove("selection-foreground"),
            background,
            foreground,
            palette,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Builtin,
    User,
    System,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThemeEntry {
    pub source: Source,
    #[serde(flatten)]
    pub theme: Theme,
}

/// Every theme file directly in `dir`; the file name is the theme's name.
pub fn read_dir(dir: &Path, source: Source) -> Vec<ThemeEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.contains("LICENSE") || name.ends_with(".md") {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).ok()?;
            Some(ThemeEntry {
                source,
                theme: Theme::parse(&name, &text),
            })
        })
        .collect()
}

/// One entry per name (case-insensitive): a user's file overrides a built-in of the same name, and both win over
/// themes found in other apps. Sorted built-ins first, then user, then system, each by name.
pub fn merge(sources: impl IntoIterator<Item = ThemeEntry>) -> Vec<ThemeEntry> {
    let rank = |s: Source| match s {
        Source::User => 0,
        Source::Builtin => 1,
        Source::System => 2,
    };
    let mut by_name: HashMap<String, ThemeEntry> = HashMap::new();
    for entry in sources {
        let key = entry.theme.name.to_lowercase();
        match by_name.get(&key) {
            Some(existing) if rank(existing.source) <= rank(entry.source) => {},
            _ => {
                by_name.insert(key, entry);
            },
        }
    }
    let order = |s: Source| match s {
        Source::Builtin => 0,
        Source::User => 1,
        Source::System => 2,
    };
    let mut list: Vec<ThemeEntry> = by_name.into_values().collect();
    list.sort_by(|a, b| {
        let builtin_default = |e: &ThemeEntry| e.source != Source::Builtin || !e.theme.name.starts_with("Supacode");
        (order(a.source), builtin_default(a), a.theme.name.to_lowercase()).cmp(&(
            order(b.source),
            builtin_default(b),
            b.theme.name.to_lowercase(),
        ))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPACODE_DARK: &str = "palette = 0=#000000\npalette = 1=#ff4245\npalette = 2=#30d158\npalette = 3=#ffd600\n\
palette = 4=#0091ff\npalette = 5=#db34f2\npalette = 6=#3cd3fe\npalette = 7=#ffffff\npalette = 8=#98989d\n\
palette = 9=#ff676a\npalette = 10=#59da79\npalette = 11=#ffde33\npalette = 12=#33a7ff\npalette = 13=#ff375f\n\
palette = 14=#00d2e0\npalette = 15=#ffffff\nbackground = #1e1e1e\nforeground = #ffffff\ncursor-color = #9d9d9d\n\
cursor-text = #1e1e1e\nselection-background = #3f638b\nselection-foreground = #ffffff\n";

    #[test]
    fn parses_supacode_dark() {
        let t = Theme::parse(DEFAULT_DARK, SUPACODE_DARK);
        assert_eq!(t.background, "#1e1e1e");
        assert_eq!(t.foreground, "#ffffff");
        assert_eq!(t.cursor, "#9d9d9d");
        assert_eq!(t.cursor_text, "#1e1e1e");
        assert_eq!(t.selection_background, "#3f638b");
        assert_eq!(t.selection_foreground.as_deref(), Some("#ffffff"));
        assert_eq!(t.palette[1], "#ff4245");
        assert_eq!(t.palette[4], "#0091ff");
        assert_eq!(t.palette[14], "#00d2e0");
        assert_eq!(t.palette.len(), 16);
    }

    #[test]
    fn ignores_unknown_and_malformed_lines() {
        let t = Theme::parse(
            "x",
            "# comment\nfont-family = Menlo\npalette = 3=#ABC\npalette = 99=#ffffff\npalette = 2=red\n\
             background = 101010\ncursor-color = nonsense\n",
        );
        assert_eq!(t.palette[3], "#aabbcc");
        assert_eq!(t.palette[2], XTERM_PALETTE[2]);
        assert_eq!(t.background, "#101010");
        assert_eq!(t.cursor, t.foreground);
        assert_eq!(t.selection_foreground, None);
    }

    #[test]
    fn merge_dedupes_with_user_over_builtin_over_system() {
        let entry = |name: &str, source, bg: &str| ThemeEntry {
            source,
            theme: Theme::parse(name, &format!("background = {bg}")),
        };
        let list = merge([
            entry("Dracula", Source::System, "#000001"),
            entry("Supacode Dark", Source::Builtin, "#1e1e1e"),
            entry("dracula", Source::Builtin, "#000002"),
            entry("Mine", Source::User, "#000003"),
            entry("Dracula", Source::User, "#000004"),
            entry("Zenburn", Source::System, "#000005"),
        ]);
        let names: Vec<_> = list.iter().map(|e| (e.theme.name.as_str(), e.source)).collect();
        assert_eq!(
            names,
            [
                ("Supacode Dark", Source::Builtin),
                ("Dracula", Source::User),
                ("Mine", Source::User),
                ("Zenburn", Source::System)
            ]
        );
        assert_eq!(list[1].theme.background, "#000004");
    }
}
