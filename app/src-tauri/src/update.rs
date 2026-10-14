//! Looks for a newer release on the stable or nightly channel. Only a pacman `ftm-bin` or `ftm-nightly-bin` install
//! (from the PKGBUILD each release attaches) can be upgraded from here; ftmd then moves onto the new build by itself,
//! as after any package upgrade. A switch to the other channel gets a fresh daemon instead.

use ftm_core::config::Channel;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

const LATEST: &str = "https://api.github.com/repos/Frazzer951/ftm/releases/latest";
/// nightly.yml keeps the newest 5 nightlies; a stable release or two may be among the newest 10 too.
const RECENT: &str = "https://api.github.com/repos/Frazzer951/ftm/releases?per_page=10";
const PACKAGES: [&str; 2] = ["ftm-bin", "ftm-nightly-bin"];

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
    pub channel: Channel,
    /// The release is on the other channel than this build's: installing it starts a fresh daemon.
    pub switch: bool,
    /// The `sh` script that builds and installs the release's PKGBUILD; `None` when this install cannot use it.
    pub script: Option<String>,
}

/// Whether this build was installed by pacman from a release PKGBUILD, asking `owner` which package owns the running
/// binary.
fn pacman_install(exe: &Path, owner: impl FnOnce(&Path) -> Option<String>) -> bool {
    cfg!(target_os = "linux") && exe.starts_with("/usr/bin") && owner(exe).is_some_and(|pkg| PACKAGES.contains(&pkg.as_str()))
}

fn pacman_owner(path: &Path) -> Option<String> {
    let out = Command::new("pacman").arg("-Qqo").arg(path).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn version(release: &Release) -> Option<Version> {
    Version::parse(release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name)).ok()
}

/// A published release of `channel`: a stable release has no pre-release part, a nightly is a pre-release tagged
/// `-nightly.` (not an `-rc.1`).
fn on_channel(release: &Release, channel: Channel) -> Option<Version> {
    if release.draft {
        return None;
    }
    let version = version(release)?;
    let ok = match channel {
        Channel::Stable => !release.prerelease && version.pre.is_empty(),
        Channel::Nightly => release.prerelease && version.pre.as_str().starts_with("nightly."),
    };
    ok.then_some(version)
}

/// The newest release of `channel` among recent ones, by version rather than by list order.
fn newest(releases: Vec<Release>, channel: Channel) -> Option<Release> {
    releases
        .into_iter()
        .filter_map(|r| Some((on_channel(&r, channel)?, r)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, r)| r)
}

/// What to offer from `release`, the newest on `target`. On this build's own channel only a newer version counts; the
/// other channel's newest is always offered, as a switch. A release missing the PKGBUILD (still uploading, or a release
/// workflow that stopped attaching it) only counts for installs that do not need it.
fn offer(release: Release, current: &Version, own: Channel, target: Channel, installable: bool) -> Option<Update> {
    let version = on_channel(&release, target)?;
    let switch = own != target;
    if !switch && version <= *current {
        return None;
    }
    let script = if installable {
        let pkgbuild = release.assets.iter().find(|a| a.name == "PKGBUILD")?;
        Some(script(&version.to_string(), target, &pkgbuild.browser_download_url)?)
    } else {
        None
    };
    Some(Update {
        version: version.to_string(),
        channel: target,
        switch,
        script,
    })
}

/// Leaves the build dir behind when anything fails, so it can be looked at. No `--noconfirm`: the pane is
/// interactive, and with it pacman would answer no to removing the other channel's conflicting package on a switch.
fn script(version: &str, channel: Channel, pkgbuild_url: &str) -> Option<String> {
    if !pkgbuild_url.starts_with("https://") || pkgbuild_url.contains(['\'', '\n']) {
        return None;
    }
    let (name, done) = match channel {
        Channel::Stable => ("FTM", "ftmd switches to it by itself; relaunch FTM when it asks."),
        Channel::Nightly => (
            "FTM Nightly",
            "ftmd switches to it by itself; relaunch FTM Nightly when it asks.",
        ),
    };
    Some(format!(
        r#"echo 'Installing {name} {version}'
d=$(mktemp -d -t ftm-update.XXXXXX) || exit 1
cd "$d" && curl -fLO '{pkgbuild_url}' && makepkg -si && cd / && rm -rf "$d" || {{ echo "FTM update failed; build files are in $d"; exit 1; }}
echo '{name} {version} is installed. {done}'"#
    ))
}

/// Whether this running build came from a release's PKGBUILD, so the update script can replace it.
pub fn installable() -> bool {
    !cfg!(debug_assertions) && std::env::current_exe().is_ok_and(|exe| pacman_install(&exe, pacman_owner))
}

/// Unauthenticated (60 requests an hour), through curl, which pacman and makepkg need anyway.
fn fetch<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .arg(url)
        .output()
        .map_err(|e| format!("running curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parsing {url}: {e}"))
}

pub fn check(current: &str, target: Channel, installable: bool) -> Result<Option<Update>, String> {
    let current = Version::parse(current).map_err(|e| e.to_string())?;
    let release = match target {
        Channel::Stable => Some(fetch::<Release>(LATEST)?),
        Channel::Nightly => newest(fetch(RECENT)?, Channel::Nightly),
    };
    Ok(release.and_then(|r| offer(r, &current, Channel::current(), target, installable)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Channel::{Nightly, Stable};

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease: tag.contains('-'),
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
        let found = |r| offer(r, &current, Stable, Stable, false).map(|u| u.version);
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
        assert_eq!(
            found(Release {
                draft: true,
                ..release("v0.1.4", &[])
            }),
            None
        );
    }

    #[test]
    fn newest_nightly() {
        let pick = |tags: &[&str]| {
            let releases = tags.iter().map(|t| release(t, &[])).collect();
            newest(releases, Nightly).map(|r| r.tag_name)
        };
        // Not in list order: by version, so a later day and a higher run number win.
        assert_eq!(
            pick(&[
                "v0.1.3-nightly.20261014.9",
                "v0.1.4-nightly.20261014.11",
                "v0.1.3",
                "v0.1.4-nightly.20261014.10",
                "v0.2.0-rc.1",
            ])
            .as_deref(),
            Some("v0.1.4-nightly.20261014.11")
        );
        assert_eq!(
            pick(&["v0.1.4-nightly.20261014.9", "v0.1.4-nightly.20261015.2"]).as_deref(),
            Some("v0.1.4-nightly.20261015.2")
        );
        assert_eq!(pick(&["v0.1.3", "v0.2.0-rc.1"]), None);

        let mut draft = release("v0.1.5-nightly.20261016.1", &[]);
        draft.draft = true;
        let releases = vec![draft, release("v0.1.4-nightly.20261014.11", &[])];
        assert_eq!(newest(releases, Nightly).unwrap().tag_name, "v0.1.4-nightly.20261014.11");
        // A -nightly. tag not marked pre-release is not a nightly.
        let mut unmarked = release("v0.1.4-nightly.20261014.11", &[]);
        unmarked.prerelease = false;
        assert!(newest(vec![unmarked], Nightly).is_none());
    }

    #[test]
    fn same_channel_needs_newer_other_channel_is_a_switch() {
        let nightly = Version::parse("0.1.4-nightly.20261014.11").unwrap();
        let stable = Version::parse("0.1.3").unwrap();
        let offered = |r, current: &Version, own, target| offer(r, current, own, target, false);

        let next = offered(release("v0.1.4-nightly.20261015.12", &[]), &nightly, Nightly, Nightly).unwrap();
        assert_eq!(
            (next.version.as_str(), next.channel, next.switch),
            ("0.1.4-nightly.20261015.12", Nightly, false)
        );
        assert!(offered(release("v0.1.4-nightly.20261014.11", &[]), &nightly, Nightly, Nightly).is_none());
        assert!(offered(release("v0.1.4-nightly.20261014.9", &[]), &nightly, Nightly, Nightly).is_none());

        // The other channel is offered even when its version is older or the same.
        let to_stable = offered(release("v0.1.3", &[]), &nightly, Nightly, Stable).unwrap();
        assert_eq!(
            (to_stable.version.as_str(), to_stable.channel, to_stable.switch),
            ("0.1.3", Stable, true)
        );
        let to_nightly = offered(release("v0.1.4-nightly.20261014.11", &[]), &stable, Stable, Nightly).unwrap();
        assert!(to_nightly.switch);
        assert!(offered(release("v0.1.3", &[]), &stable, Nightly, Stable).unwrap().switch);
        // A release that is not on the target channel is never offered.
        assert!(offered(release("v0.2.0-rc.1", &[]), &stable, Nightly, Stable).is_none());
        assert!(offered(release("v0.1.4", &[]), &stable, Stable, Nightly).is_none());
    }

    #[test]
    fn installing_needs_the_pkgbuild_asset() {
        let current = Version::parse("0.1.3").unwrap();
        let assets = ["FTM_0.1.4_amd64.deb", "PKGBUILD.sig", "PKGBUILD", "ftm.rb"];
        let update = offer(release("v0.1.4", &assets), &current, Stable, Stable, true).unwrap();
        let script = update.script.unwrap();
        assert!(
            script.contains("curl -fLO 'https://github.com/Frazzer951/ftm/releases/download/v0.1.4/PKGBUILD' &&"),
            "{script}"
        );
        assert!(
            script.contains("makepkg -si &&") && !script.contains("--noconfirm"),
            "{script}"
        );
        let only_deb = release("v0.1.4", &["FTM_0.1.4_amd64.deb"]);
        assert!(offer(only_deb, &current, Stable, Stable, true).is_none());
        // Not installable here: still worth telling, without the script.
        let update = offer(release("v0.1.4", &[]), &current, Stable, Stable, false).unwrap();
        assert_eq!(update.script, None);

        let update = offer(
            release("v0.1.4-nightly.20261014.11", &["PKGBUILD"]),
            &current,
            Stable,
            Nightly,
            true,
        );
        let script = update.unwrap().script.unwrap();
        assert!(
            script.contains("FTM Nightly 0.1.4-nightly.20261014.11 is installed"),
            "{script}"
        );
    }

    #[test]
    fn script_refuses_urls_it_cannot_quote() {
        assert!(script("0.1.4", Stable, "https://example.com/a'b").is_none());
        assert!(script("0.1.4", Stable, "http://example.com/PKGBUILD").is_none());
    }

    #[test]
    fn script_keeps_the_build_dir_on_failure() {
        let dir = std::env::temp_dir().join(format!("ftm-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = script("0.1.4", Stable, "https://127.0.0.1:1/PKGBUILD").unwrap();
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
        let linux = cfg!(target_os = "linux");
        assert_eq!(pacman_install(usr_bin, owned_by("ftm-bin")), linux);
        assert_eq!(pacman_install(usr_bin, owned_by("ftm-nightly-bin")), linux);
        assert!(!pacman_install(usr_bin, owned_by("ftm")));
        assert!(!pacman_install(usr_bin, owned_by("ftm-nightly")));
        assert!(!pacman_install(usr_bin, |_| None));
        assert!(!pacman_install(Path::new("/home/u/.local/bin/ftm-app"), owned_by("ftm-bin")));
        assert!(!pacman_install(
            Path::new("/Applications/FTM.app/Contents/MacOS/ftm-app"),
            owned_by("ftm-bin")
        ));
    }
}
