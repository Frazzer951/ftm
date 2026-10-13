//! Themes for the Settings picker: built into the app, dropped in `~/.config/ftm/themes`, or found in other
//! terminal apps' theme folders.

use ftm_core::paths;
use ftm_core::theme::{self, Source, Theme, ThemeEntry};
use std::path::Path;

const BUILTIN: &[(&str, &str)] = &[
    ("Supacode Dark", include_str!("../themes/Supacode Dark")),
    ("Supacode Light", include_str!("../themes/Supacode Light")),
    ("Atom One Dark", include_str!("../themes/upstream/Atom One Dark")),
    ("Atom One Light", include_str!("../themes/upstream/Atom One Light")),
    ("Catppuccin Latte", include_str!("../themes/upstream/Catppuccin Latte")),
    ("Catppuccin Mocha", include_str!("../themes/upstream/Catppuccin Mocha")),
    ("Dracula", include_str!("../themes/upstream/Dracula")),
    ("Everforest Dark Med", include_str!("../themes/upstream/Everforest Dark Med")),
    ("GitHub Dark Default", include_str!("../themes/upstream/GitHub Dark Default")),
    (
        "GitHub Light Default",
        include_str!("../themes/upstream/GitHub Light Default"),
    ),
    ("Gruvbox Dark", include_str!("../themes/upstream/Gruvbox Dark")),
    ("Gruvbox Light", include_str!("../themes/upstream/Gruvbox Light")),
    ("Kanagawa Wave", include_str!("../themes/upstream/Kanagawa Wave")),
    ("Monokai Pro", include_str!("../themes/upstream/Monokai Pro")),
    ("Nord", include_str!("../themes/upstream/Nord")),
    ("Rose Pine", include_str!("../themes/upstream/Rose Pine")),
    ("TokyoNight", include_str!("../themes/upstream/TokyoNight")),
    ("TokyoNight Day", include_str!("../themes/upstream/TokyoNight Day")),
    (
        "iTerm2 Solarized Dark",
        include_str!("../themes/upstream/iTerm2 Solarized Dark"),
    ),
    (
        "iTerm2 Solarized Light",
        include_str!("../themes/upstream/iTerm2 Solarized Light"),
    ),
];

/// Ghostty-format theme folders of apps that may be installed; read-only.
const SYSTEM_DIRS: &[&str] = &[
    "/Applications/Ghostty.app/Contents/Resources/ghostty/themes",
    "/Applications/supacode.app/Contents/Resources/ghostty/themes",
    "/usr/share/ghostty/themes",
];

pub fn all() -> Vec<ThemeEntry> {
    let builtin = BUILTIN.iter().map(|(name, text)| ThemeEntry {
        source: Source::Builtin,
        theme: Theme::parse(name, text),
    });
    let user = theme::read_dir(&paths::config_dir().join("themes"), Source::User);
    let system = SYSTEM_DIRS
        .iter()
        .flat_map(|dir| theme::read_dir(Path::new(dir), Source::System));
    theme::merge(builtin.chain(user).chain(system))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_lead_the_list() {
        assert_eq!(BUILTIN.len(), 20);
        for (name, text) in BUILTIN {
            let t = Theme::parse(name, text);
            assert!(text.contains("background"), "{name} has no background");
            assert_ne!(t.background, t.foreground, "{name}");
        }
        let list = all();
        assert_eq!(list[0].theme.name, "Supacode Dark");
        assert_eq!(list[1].theme.name, "Supacode Light");
        assert_eq!(list.iter().filter(|e| e.source == Source::Builtin).count(), 20);
        let mut seen = std::collections::HashSet::new();
        assert!(list.iter().all(|e| seen.insert(e.theme.name.to_lowercase())));
        eprintln!("{} themes", list.len());
    }
}
