//! Looks for a newer stable release. Only a pacman `ftm-bin` install (from the PKGBUILD each release attaches) can
//! be upgraded from here; ftmd then moves onto the new build by itself, as after any package upgrade.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

const LATEST: &str = "https://api.github.com/repos/Frazzer951/ftm/releases/latest";
const PACKAGE: &str = "ftm-bin";

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Update {
    pub version: String,
    /// The `sh` script that builds and installs the release's PKGBUILD; `None` when this install cannot use it.
    pub script: Option<String>,
}

/// Whether this build was installed by pacman as `ftm-bin`, asking `owner` which package owns the running binary.
fn pacman_install(channel: &str, exe: &Path, owner: impl FnOnce(&Path) -> Option<String>) -> bool {
    cfg!(target_os = "linux") && channel == "stable" && exe.starts_with("/usr/bin") && owner(exe).as_deref() == Some(PACKAGE)
}

fn pacman_owner(path: &Path) -> Option<String> {
    let out = Command::new("pacman").arg("-Qqo").arg(path).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `None` when nothing newer than `current` is out. A release missing the PKGBUILD (still uploading, or a release
/// workflow that stopped attaching it) only counts for installs that do not need it.
fn newer(release: Release, current: &Version, installable: bool) -> Option<Update> {
    if release.draft || release.prerelease {
        return None;
    }
    let version = Version::parse(release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name)).ok()?;
    if !version.pre.is_empty() || version <= *current {
        return None;
    }
    let script = if installable {
        let pkgbuild = release.assets.iter().find(|a| a.name == "PKGBUILD")?;
        Some(script(&version.to_string(), &pkgbuild.browser_download_url)?)
    } else {
        None
    };
    Some(Update {
        version: version.to_string(),
        script,
    })
}

/// Leaves the build dir behind when anything fails, so it can be looked at.
fn script(version: &str, pkgbuild_url: &str) -> Option<String> {
    if !pkgbuild_url.starts_with("https://") || pkgbuild_url.contains(['\'', '\n']) {
        return None;
    }
    Some(format!(
        r#"echo 'Updating FTM to {version}'
d=$(mktemp -d -t ftm-update.XXXXXX) || exit 1
cd "$d" && curl -fLO '{pkgbuild_url}' && makepkg -si --noconfirm && cd / && rm -rf "$d" || {{ echo "FTM update failed; build files are in $d"; exit 1; }}
echo 'FTM {version} is installed. ftmd switches to it by itself; relaunch FTM when it asks.'"#
    ))
}

/// Whether this running build came from the release's PKGBUILD, so the update script can replace it.
pub fn installable() -> bool {
    !cfg!(debug_assertions)
        && std::env::current_exe().is_ok_and(|exe| pacman_install(ftm_core::paths::CHANNEL, &exe, pacman_owner))
}

/// Unauthenticated (60 requests an hour), through curl, which pacman and makepkg need anyway.
pub fn check(current: &str, installable: bool) -> Result<Option<Update>, String> {
    let current = Version::parse(current).map_err(|e| e.to_string())?;
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .arg(LATEST)
        .output()
        .map_err(|e| format!("running curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl {LATEST}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let release: Release = serde_json::from_slice(&out.stdout).map_err(|e| format!("parsing {LATEST}: {e}"))?;
    Ok(newer(release, &current, installable))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease: false,
            draft: false,
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: name.to_string(),
                    browser_download_url: format!("https://github.com/Frazzer951/ftm/releases/download/{tag}/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn only_newer_stable_releases() {
        let current = Version::parse("0.1.3").unwrap();
        let found = |r| newer(r, &current, false).map(|u| u.version);
        assert_eq!(found(release("v0.1.4", &[])).as_deref(), Some("0.1.4"));
        assert_eq!(found(release("v0.10.0", &[])).as_deref(), Some("0.10.0"));
        assert_eq!(found(release("v0.1.3", &[])), None);
        assert_eq!(found(release("v0.1.2", &[])), None);
        assert_eq!(found(release("v0.1.4-nightly.20261014.7", &[])), None);
        assert_eq!(found(release("v0.2.0-rc.1", &[])), None);
        assert_eq!(found(release("garbage", &[])), None);
        assert_eq!(
            found(Release {
                prerelease: true,
                ..release("v0.1.4", &[])
            }),
            None
        );

        let nightly = Version::parse("0.1.4-nightly.20261014.7").unwrap();
        assert!(newer(release("v0.1.4", &[]), &nightly, false).is_some());
    }

    #[test]
    fn installing_needs_the_pkgbuild_asset() {
        let current = Version::parse("0.1.3").unwrap();
        let assets = ["FTM_0.1.4_amd64.deb", "PKGBUILD.sig", "PKGBUILD", "ftm.rb"];
        let update = newer(release("v0.1.4", &assets), &current, true).unwrap();
        let script = update.script.unwrap();
        assert!(
            script.contains("curl -fLO 'https://github.com/Frazzer951/ftm/releases/download/v0.1.4/PKGBUILD' &&"),
            "{script}"
        );
        assert!(newer(release("v0.1.4", &["FTM_0.1.4_amd64.deb"]), &current, true).is_none());
        // Not installable here: still worth telling, without the script.
        let update = newer(release("v0.1.4", &[]), &current, false).unwrap();
        assert_eq!(update.script, None);
    }

    #[test]
    fn script_refuses_urls_it_cannot_quote() {
        assert!(script("0.1.4", "https://example.com/a'b").is_none());
        assert!(script("0.1.4", "http://example.com/PKGBUILD").is_none());
    }

    #[test]
    fn script_keeps_the_build_dir_on_failure() {
        let dir = std::env::temp_dir().join(format!("ftm-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = script("0.1.4", "https://127.0.0.1:1/PKGBUILD").unwrap();
        let out = Command::new("/bin/sh")
            .args(["-c", &script])
            .env("TMPDIR", &dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success(), "{stdout}");
        assert!(stdout.contains("FTM update failed; build files are in"), "{stdout}");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].to_string_lossy().starts_with("ftm-update."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eligibility() {
        let owned_by = |pkg: &'static str| move |_: &Path| Some(pkg.to_string());
        let usr_bin = Path::new("/usr/bin/ftm-app");
        assert_eq!(
            pacman_install("stable", usr_bin, owned_by(PACKAGE)),
            cfg!(target_os = "linux")
        );
        assert!(!pacman_install("nightly", usr_bin, owned_by(PACKAGE)));
        assert!(!pacman_install("stable", usr_bin, owned_by("ftm")));
        assert!(!pacman_install("stable", usr_bin, |_| None));
        assert!(!pacman_install(
            "stable",
            Path::new("/home/u/.local/bin/ftm-app"),
            owned_by(PACKAGE)
        ));
        assert!(!pacman_install(
            "stable",
            Path::new("/Applications/FTM.app/Contents/MacOS/ftm-app"),
            owned_by(PACKAGE)
        ));
    }
}
