//! Sounds for the status alerts: built into the app (`scripts/gen-sounds.py`), dropped in `~/.config/ftm/sounds`, or
//! the system's own. The webview decides which formats it can play.

use ftm_core::paths;
use ftm_core::theme::Source;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const BUILTIN: &[(&str, &[u8])] = &[
    ("Chime", include_bytes!("../sounds/Chime.wav")),
    ("Nudge", include_bytes!("../sounds/Nudge.wav")),
    ("Arpeggio", include_bytes!("../sounds/Arpeggio.wav")),
    ("Bell", include_bytes!("../sounds/Bell.wav")),
    ("Blip", include_bytes!("../sounds/Blip.wav")),
    ("Knock", include_bytes!("../sounds/Knock.wav")),
];

const SYSTEM_DIRS: &[&str] = &["/System/Library/Sounds", "/usr/share/sounds/freedesktop/stereo"];

#[derive(Debug, Clone, Serialize)]
pub struct SoundEntry {
    pub name: String,
    pub source: Source,
    pub mime: &'static str,
    #[serde(skip)]
    path: Option<PathBuf>,
}

fn mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "wav" => "audio/wav",
        "aif" | "aiff" | "aifc" => "audio/aiff",
        "oga" | "ogg" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "caf" => "audio/x-caf",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// Every audio file directly in `dir`, named by its file stem.
fn read_dir(dir: &Path, source: Source) -> Vec<SoundEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let path = e.path();
            Some(SoundEntry {
                name: path.file_stem()?.to_string_lossy().into_owned(),
                source,
                mime: mime(&path)?,
                path: Some(path),
            })
        })
        .collect()
}

/// One entry per name (case-insensitive): yours over built-in over system. Built-ins first in their own order, then
/// yours, then the system's, each by name.
fn merge(sources: impl IntoIterator<Item = SoundEntry>) -> Vec<SoundEntry> {
    let rank = |s: Source| match s {
        Source::User => 0,
        Source::Builtin => 1,
        Source::System => 2,
    };
    let mut by_name: HashMap<String, SoundEntry> = HashMap::new();
    for entry in sources {
        let key = entry.name.to_lowercase();
        if by_name
            .get(&key)
            .is_none_or(|existing| rank(entry.source) < rank(existing.source))
        {
            by_name.insert(key, entry);
        }
    }
    let order = |e: &SoundEntry| match e.source {
        Source::Builtin => (0, BUILTIN.iter().position(|(n, _)| *n == e.name).unwrap_or(0)),
        Source::User => (1, 0),
        Source::System => (2, 0),
    };
    let mut list: Vec<SoundEntry> = by_name.into_values().collect();
    list.sort_by_key(|e| (order(e), e.name.to_lowercase()));
    list
}

pub fn all() -> Vec<SoundEntry> {
    let builtin = BUILTIN.iter().map(|(name, _)| SoundEntry {
        name: name.to_string(),
        source: Source::Builtin,
        mime: "audio/wav",
        path: None,
    });
    let user = read_dir(&paths::config_dir().join("sounds"), Source::User);
    let system = SYSTEM_DIRS.iter().flat_map(|dir| read_dir(Path::new(dir), Source::System));
    merge(builtin.chain(user).chain(system))
}

/// The bytes of the sound listed as `name`.
pub fn data(name: &str) -> Result<Vec<u8>, String> {
    let entry = all()
        .into_iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no sound named {name}"))?;
    match entry.path {
        Some(path) => std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display())),
        None => Ok(BUILTIN
            .iter()
            .find(|(n, _)| *n == entry.name)
            .map(|(_, b)| b.to_vec())
            .unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, source: Source) -> SoundEntry {
        SoundEntry {
            name: name.into(),
            source,
            mime: "audio/wav",
            path: None,
        }
    }

    #[test]
    fn builtins_are_wav_and_lead_the_list() {
        for (name, bytes) in BUILTIN {
            assert!(bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE", "{name}");
        }
        let list = all();
        let names: Vec<_> = list.iter().take(BUILTIN.len()).map(|e| e.name.as_str()).collect();
        assert_eq!(names, BUILTIN.iter().map(|(n, _)| *n).collect::<Vec<_>>());
        assert_eq!(data("chime").unwrap(), BUILTIN[0].1);
        assert!(data("no such sound").is_err());
    }

    #[test]
    fn yours_win_over_builtin_over_system() {
        let list = merge([
            entry("Glass", Source::System),
            entry("Chime", Source::Builtin),
            entry("chime", Source::System),
            entry("Nudge", Source::Builtin),
            entry("nudge", Source::User),
            entry("Beep", Source::User),
        ]);
        let got: Vec<_> = list.iter().map(|e| (e.name.as_str(), e.source)).collect();
        assert_eq!(
            got,
            [
                ("Chime", Source::Builtin),
                ("Beep", Source::User),
                ("nudge", Source::User),
                ("Glass", Source::System)
            ]
        );
    }

    #[test]
    fn only_audio_extensions_are_listed() {
        assert_eq!(mime(Path::new("/x/Glass.aiff")), Some("audio/aiff"));
        assert_eq!(mime(Path::new("/x/complete.OGA")), Some("audio/ogg"));
        assert_eq!(mime(Path::new("/x/README")), None);
        assert_eq!(mime(Path::new("/x/notes.txt")), None);
    }
}
