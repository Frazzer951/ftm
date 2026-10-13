//! Installed font families for the Settings font picker.

use std::collections::BTreeMap;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FontFamily {
    pub name: String,
    pub monospace: bool,
}

/// Every visible family once (a Nerd Font's many styles share one family name), sorted by name. A family counts as
/// monospace if any face says so, or its name does: several fixed-width fonts (FiraCode Nerd Font with its double-width
/// icons, for one) do not carry the flag.
pub fn families() -> Vec<FontFamily> {
    let mut by_name: BTreeMap<String, FontFamily> = BTreeMap::new();
    for (name, monospace) in faces() {
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        let entry = by_name.entry(name.to_lowercase()).or_insert_with(|| FontFamily {
            monospace: looks_monospace(&name),
            name,
        });
        entry.monospace |= monospace;
    }
    by_name.into_values().collect()
}

/// "Propo" marks the proportional variant of a Nerd Font.
fn looks_monospace(name: &str) -> bool {
    let name = name.to_lowercase();
    !name.contains("propo")
        && ["mono", "code", "console", "courier", "terminal"]
            .iter()
            .any(|w| name.contains(w))
}

/// (family, monospace) for every installed face, from Core Text's descriptors without loading the fonts.
#[cfg(target_os = "macos")]
fn faces() -> Vec<(String, bool)> {
    use core_text::font_descriptor::{SymbolicTraitAccessors, TraitAccessors};
    let collection = core_text::font_collection::create_for_all_families();
    let Some(descriptors) = collection.get_descriptors() else {
        return Vec::new();
    };
    descriptors
        .iter()
        .map(|d| (d.family_name(), d.traits().symbolic_traits().is_monospace()))
        .collect()
}

/// (family, monospace) for every installed face, from fontconfig (spacing 100 is mono, 90 dual-width).
#[cfg(not(target_os = "macos"))]
fn faces() -> Vec<(String, bool)> {
    let Ok(out) = std::process::Command::new("fc-list")
        .args(["--format", "%{family[0]}\\t%{spacing}\\n"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (family, spacing) = line.split_once('\t')?;
            Some((family.to_string(), matches!(spacing, "90" | "100")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_families_once_with_monospace_marked() {
        let start = std::time::Instant::now();
        let all = families();
        let mono = all.iter().filter(|f| f.monospace).count();
        eprintln!("{} families ({mono} monospace) in {:?}", all.len(), start.elapsed());
        assert!(!all.is_empty());
        let mut seen = std::collections::HashSet::new();
        assert!(
            all.iter().all(|f| seen.insert(f.name.to_lowercase())),
            "a family is listed twice"
        );
        for f in all.iter().filter(|f| f.name.contains("Fira") || f.name == "Menlo") {
            eprintln!("  {} monospace={}", f.name, f.monospace);
        }
        if let Some(menlo) = all.iter().find(|f| f.name == "Menlo") {
            assert!(menlo.monospace);
        }
        assert!(looks_monospace("FiraCode Nerd Font") && !looks_monospace("Helvetica"));
        assert!(!looks_monospace("FiraCode Nerd Font Propo"));
    }
}
