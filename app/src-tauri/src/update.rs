//! Looks for a newer release on the stable or nightly channel and works out how this copy was installed, so the app
//! can say how to update it, and run the update in a terminal workspace where the package manager allows. ftmd then
//! moves onto the new build by itself, as after any package upgrade. A switch to the other channel gets a fresh daemon
//! instead.

use ftm_core::config::{Channel, UpdateSettings};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "https://github.com/Frazzer951/ftm";
const LATEST: &str = "https://api.github.com/repos/Frazzer951/ftm/releases/latest";
/// nightly.yml keeps the newest 5 nightlies; a stable release or two may be among the newest 10 too.
const RECENT: &str = "https://api.github.com/repos/Frazzer951/ftm/releases?per_page=10";
const TAP: &str = "frazzer951/ftm";
/// The packages that the release `PKGBUILD`s build.
const PACMAN_PACKAGES: [&str; 2] = ["ftm-bin", "ftm-nightly-bin"];
/// The `.deb` and `.rpm` package names.
const LINUX_PACKAGES: [&str; 2] = ["ftm", "ftm-nightly"];

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

/// How the running copy of FTM got onto this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Install {
    /// pacman's `ftm-bin` or `ftm-nightly-bin`, from a release `PKGBUILD`.
    Pacman,
    /// The `frazzer951/ftm` Homebrew cask, with the `brew` that manages it.
    Brew {
        brew: PathBuf,
    },
    /// A macOS app bundle that Homebrew doesn't manage: a release `.dmg`, or copied by hand.
    MacApp {
        bundle: PathBuf,
    },
    Deb,
    Rpm,
    /// Built and installed from a checkout by `scripts/install.sh`.
    Source,
    /// A debug build, as `scripts/dev.sh` runs.
    Dev,
    Unknown,
}

impl Install {
    /// The install `FTM_UPDATE_FAKE_INSTALL` names, for trying the update prompt from a dev build.
    fn fake(kind: &str) -> Option<Install> {
        Some(match kind {
            "pacman" => Install::Pacman,
            "brew" => Install::Brew {
                brew: "/opt/homebrew/bin/brew".into(),
            },
            "mac_app" => Install::MacApp {
                bundle: "/Applications/FTM.app".into(),
            },
            "deb" => Install::Deb,
            "rpm" => Install::Rpm,
            "source" => Install::Source,
            "dev" => Install::Dev,
            "unknown" => Install::Unknown,
            _ => return None,
        })
    }

    /// Whether a new release interrupts with a dialog rather than a notice. A build from a checkout already knows
    /// where its updates come from.
    fn prompts(&self) -> bool {
        !matches!(self, Install::Source | Install::Dev)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Manager {
    Pacman,
    Dpkg,
    Rpm,
}

/// What detection asks of the machine, so tests can stand in for it.
trait System {
    /// The package that `manager` says owns `path`.
    fn owner(&self, manager: Manager, path: &Path) -> Option<String>;
    fn exists(&self, path: &Path) -> bool;
}

struct Host;

impl System for Host {
    fn owner(&self, manager: Manager, path: &Path) -> Option<String> {
        let mut command = match manager {
            Manager::Pacman => Command::new("pacman"),
            Manager::Dpkg => Command::new("dpkg-query"),
            Manager::Rpm => Command::new("rpm"),
        };
        match manager {
            Manager::Pacman => command.arg("-Qqo"),
            Manager::Dpkg => command.arg("-S"),
            Manager::Rpm => command.args(["-qf", "--qf", "%{NAME}"]),
        };
        let out = command.arg(path).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let out = String::from_utf8_lossy(&out.stdout);
        let line = out.lines().next()?;
        // dpkg-query prints `ftm: /usr/bin/ftm-app`, or `ftm:amd64: …` for a multi-arch package.
        let name = if manager == Manager::Dpkg {
            line.split(':').next()?
        } else {
            line
        };
        Some(name.trim().to_string())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Linux,
    Mac,
    Other,
}

impl Os {
    const HOST: Os = if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Other
    };
}

/// The Homebrew cask that installs an app bundle of this name.
fn cask_for_bundle(name: &str) -> Option<&'static str> {
    match name {
        "FTM.app" => Some("ftm"),
        "FTM Nightly.app" => Some("ftm@nightly"),
        _ => None,
    }
}

/// How the binary at `exe` was installed. On Linux the release packages put it in `/usr/bin`, owned by pacman, dpkg or
/// rpm. On macOS it sits in an app bundle, which Homebrew manages when one of `homebrew`'s prefixes has a Caskroom
/// entry for the bundle's cask.
fn detect(exe: &Path, os: Os, homebrew: &[PathBuf], system: &impl System) -> Install {
    match os {
        Os::Linux if exe.starts_with("/usr/bin") => {
            let owned_by =
                |manager, names: &[&str]| system.owner(manager, exe).is_some_and(|pkg| names.contains(&pkg.as_str()));
            if owned_by(Manager::Pacman, &PACMAN_PACKAGES) {
                Install::Pacman
            } else if owned_by(Manager::Dpkg, &LINUX_PACKAGES) {
                Install::Deb
            } else if owned_by(Manager::Rpm, &LINUX_PACKAGES) {
                Install::Rpm
            } else {
                Install::Unknown
            }
        },
        Os::Mac => {
            let Some(bundle) = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")) else {
                return Install::Unknown;
            };
            let cask = bundle.file_name().and_then(|n| n.to_str()).and_then(cask_for_bundle);
            let brew = cask.and_then(|cask| {
                homebrew
                    .iter()
                    .map(|prefix| (prefix.join("Caskroom").join(cask), prefix.join("bin/brew")))
                    .find(|(caskroom, brew)| system.exists(caskroom) && system.exists(brew))
                    .map(|(_, brew)| brew)
            });
            match brew {
                Some(brew) => Install::Brew { brew },
                None => Install::MacApp {
                    bundle: bundle.to_path_buf(),
                },
            }
        },
        _ => Install::Unknown,
    }
}

/// How this running copy was installed. `scripts/install.sh` builds with `FTM_SOURCE_BUILD` set; the release workflows
/// don't.
pub fn install() -> Install {
    if cfg!(debug_assertions) {
        return Install::Dev;
    }
    if option_env!("FTM_SOURCE_BUILD").is_some() {
        return Install::Source;
    }
    let Ok(exe) = std::env::current_exe() else {
        return Install::Unknown;
    };
    // An app opened from Finder or the Dock has no HOMEBREW_PREFIX, so try both default prefixes too.
    let mut homebrew: Vec<PathBuf> = std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from).into_iter().collect();
    homebrew.extend(["/opt/homebrew", "/usr/local"].map(PathBuf::from));
    detect(&exe, Os::HOST, &homebrew, &Host)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Update {
    pub version: String,
    pub channel: Channel,
    /// The release is on the other channel than this build's: installing it starts a fresh daemon.
    pub switch: bool,
    pub install: Install,
    /// The release's page.
    pub page: String,
    /// The release file this install updates from, when it takes one.
    pub download: Option<String>,
    /// How this copy was installed and what updating it takes.
    pub how: String,
    /// The commands to update by hand, if there are any.
    pub commands: Option<String>,
    /// An `sh` script that updates this install from a terminal workspace; `None` when it has to be done by hand.
    pub script: Option<String>,
    /// Ask with a dialog rather than a notice.
    pub prompt: bool,
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

/// The end of the name of the release file `install` updates from on `arch` (as `std::env::consts::ARCH` names it),
/// matching the patterns release.yml and nightly.yml check for.
fn asset_suffix(install: &Install, arch: &str) -> Option<String> {
    let renamed = |x86_64: &str, aarch64: &str| match arch {
        "x86_64" => x86_64.to_string(),
        "aarch64" => aarch64.to_string(),
        other => other.to_string(),
    };
    match install {
        Install::Pacman => Some("PKGBUILD".into()),
        Install::Deb => Some(format!("_{}.deb", renamed("amd64", "arm64"))),
        Install::Rpm => Some(format!(".{arch}.rpm")),
        Install::MacApp { .. } => Some(format!("_{}.dmg", renamed("x64", "aarch64"))),
        _ => None,
    }
}

/// `s` in single quotes for `sh`, unless it holds a quote or a line break.
fn quoted(s: &str) -> Option<String> {
    (!s.contains(['\'', '\n'])).then(|| format!("'{s}'"))
}

/// What to offer from `release`, the newest on `target`. On this build's own channel only a newer version counts; the
/// other channel's newest is always offered, as a switch. A release missing the file this install updates from (still
/// uploading, or a release workflow that stopped attaching it) is not offered yet.
fn offer(release: Release, current: &Version, own: Channel, target: Channel, install: &Install, arch: &str) -> Option<Update> {
    let version = on_channel(&release, target)?;
    let switch = own != target;
    if !switch && version <= *current {
        return None;
    }
    let asset = match asset_suffix(install, arch) {
        Some(suffix) => Some(release.assets.iter().find(|a| a.name.ends_with(&suffix))?),
        None => None,
    };
    let plan = plan(install, &version.to_string(), target, switch, asset)?;
    Some(Update {
        version: version.to_string(),
        channel: target,
        switch,
        install: install.clone(),
        page: format!("{REPO}/releases/tag/{}", release.tag_name),
        download: asset.map(|a| a.browser_download_url.clone()),
        how: plan.how,
        commands: plan.commands,
        script: plan.script,
        prompt: install.prompts(),
    })
}

struct Plan {
    how: String,
    commands: Option<String>,
    script: Option<String>,
}

fn app_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "FTM",
        Channel::Nightly => "FTM Nightly",
    }
}

/// How to get `version` of `target` onto `install`. `None` when the release file's URL is not one the script can
/// safely quote.
fn plan(install: &Install, version: &str, target: Channel, switch: bool, asset: Option<&Asset>) -> Option<Plan> {
    let name = app_name(target);
    let download = |run: &str| -> Option<(String, String)> {
        let asset = asset?;
        let url = &asset.browser_download_url;
        if !url.starts_with("https://") {
            return None;
        }
        let url = quoted(url)?;
        let file = quoted(&format!("./{}", asset.name))?;
        let run = run.replace("{file}", &file);
        Some((
            format!("curl -fLO {url}\n{run}"),
            downloading_script(name, version, switch, &url, &run),
        ))
    };
    let plan = match install {
        Install::Pacman => {
            let package = match target {
                Channel::Stable => "ftm-bin",
                Channel::Nightly => "ftm-nightly-bin",
            };
            let (commands, script) = download("makepkg -si")?;
            Plan {
                how: format!(
                    "Installed with pacman. Update builds the release's {package} PKGBUILD with makepkg in a terminal \
                     workspace, where pacman asks for your sudo password."
                ),
                commands: Some(commands),
                script: Some(script),
            }
        },
        Install::Deb | Install::Rpm => {
            let (kind, tool) = if *install == Install::Deb {
                (".deb", "apt")
            } else {
                (".rpm", "dnf")
            };
            let file = &asset?.name;
            let (commands, script) = download(&format!("sudo {tool} install {{file}}"))?;
            Plan {
                how: format!(
                    "Installed from a {kind}. Update downloads {file} and installs it with {tool} in a terminal \
                     workspace, where sudo asks for your password."
                ),
                commands: Some(commands),
                script: Some(script),
            }
        },
        Install::Brew { brew } => {
            let cask = format!(
                "{TAP}/{}",
                match target {
                    Channel::Stable => "ftm",
                    Channel::Nightly => "ftm@nightly",
                }
            );
            let verb = if switch { "install" } else { "upgrade" };
            let command = format!("brew {verb} --cask {cask}");
            let brew = quoted(&brew.to_string_lossy())?;
            // The other channel's cask may already sit next to this one; then it wants an upgrade.
            let run = if switch {
                format!(
                    "if {brew} list --cask {cask} >/dev/null 2>&1; then {brew} upgrade --cask {cask}; else {brew} install --cask {cask}; fi"
                )
            } else {
                format!("{brew} upgrade --cask {cask}")
            };
            Plan {
                how: format!("Installed with Homebrew. Update runs `{command}` in a terminal workspace."),
                commands: Some(command),
                script: Some(script_around(name, version, switch, &run)),
            }
        },
        Install::MacApp { bundle } => {
            let file = &asset?.name;
            let folder = bundle.parent().unwrap_or(Path::new("/Applications"));
            let target_bundle = folder.join(format!("{name}.app"));
            let replace = if switch { "" } else { ", replacing the old one" };
            Plan {
                how: format!(
                    "Installed from a .dmg, not through Homebrew. Download {file}, quit FTM, then drag {name}.app from it \
                     into {}{replace}. FTM is not notarized, so clear the quarantine flag before opening it:",
                    folder.display()
                ),
                commands: Some(format!(
                    "xattr -dr com.apple.quarantine {}",
                    quoted(&target_bundle.to_string_lossy())?
                )),
                script: None,
            }
        },
        Install::Source => Plan {
            how: "Built from source with scripts/install.sh. To update, pull the checkout and rerun it.".into(),
            commands: Some("git pull\nscripts/install.sh".into()),
            script: None,
        },
        Install::Dev => Plan {
            how: "A dev build. Pull and rerun scripts/dev.sh to rebuild.".into(),
            commands: Some("git pull\nscripts/dev.sh".into()),
            script: None,
        },
        Install::Unknown => Plan {
            how: format!("FTM can't tell how this copy was installed. Get {name} {version} from the release page."),
            commands: None,
            script: None,
        },
    };
    Some(plan)
}

fn done_message(name: &str, switch: bool) -> String {
    if switch {
        format!("Quit FTM and start {name}. Panes in this window stay on the old daemon until it exits.")
    } else {
        format!("ftmd switches to it by itself; relaunch {name} when it asks.")
    }
}

/// Downloads `url` into a fresh dir and runs `run` there. Leaves the dir behind when anything fails, so it can be
/// looked at. No `--noconfirm` or `-y`: the pane is interactive, and pacman would otherwise answer no to removing the
/// other channel's conflicting package on a switch. After a switch nothing asks to relaunch: the old ftmd refuses the
/// other channel's build and the new app talks to a socket of its own.
fn downloading_script(name: &str, version: &str, switch: bool, url: &str, run: &str) -> String {
    let done = done_message(name, switch);
    format!(
        r#"echo 'Installing {name} {version}'
d=$(mktemp -d "${{TMPDIR:-/tmp}}/ftm-update.XXXXXX") || exit 1
cd "$d" && curl -fLO {url} && {run} && cd / && rm -rf "$d" || {{ echo "FTM update failed; the files are in $d"; exit 1; }}
echo '{name} {version} is installed. {done}'"#
    )
}

fn script_around(name: &str, version: &str, switch: bool, run: &str) -> String {
    let done = done_message(name, switch);
    format!(
        r#"echo 'Installing {name} {version}'
{run} || {{ echo 'FTM update failed'; exit 1; }}
echo '{name} {version} is installed. {done}'"#
    )
}

/// Unauthenticated (60 requests an hour), through curl, which every supported system has.
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

fn check(current: &str, target: Channel, install: &Install) -> Result<Option<Update>, String> {
    let current = Version::parse(current).map_err(|e| e.to_string())?;
    let release = match target {
        Channel::Stable => Some(fetch::<Release>(LATEST)?),
        Channel::Nightly => newest(fetch(RECENT)?, Channel::Nightly),
    };
    Ok(release.and_then(|r| offer(r, &current, Channel::current(), target, install, std::env::consts::ARCH)))
}

/// The result of a check that ran: the version this build counts as (`FTM_UPDATE_FAKE_VERSION` when set) and the
/// release on offer, if any.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Checked {
    pub current: String,
    pub update: Option<Update>,
}

/// Whether to look: a manual check always does, otherwise `[updates] check`, which unset means every install but a dev
/// build that isn't faking one.
fn wanted(check: Option<bool>, manual: bool, faking: bool, install: &Install) -> bool {
    manual || check.unwrap_or(faking || *install != Install::Dev)
}

/// What `[updates]` asks to look for: a newer release on its channel (else this build's), or the other channel's newest
/// as a switch. `None` when the check is off; `manual` (the Updates tab's Check now) looks regardless.
///
/// To try the prompt, `FTM_UPDATE_FAKE_VERSION` makes this build pretend to be that version and
/// `FTM_UPDATE_FAKE_INSTALL` (`pacman`, `brew`, `mac_app`, `deb`, `rpm`, `source`, `dev` or `unknown`) to have been
/// installed that way. Either one turns the check on for a dev build.
pub fn look(settings: &UpdateSettings, manual: bool) -> Result<Option<Checked>, String> {
    let fake_version = std::env::var("FTM_UPDATE_FAKE_VERSION").ok().filter(|v| !v.is_empty());
    let fake_install = match std::env::var("FTM_UPDATE_FAKE_INSTALL") {
        Ok(kind) if !kind.is_empty() => {
            Some(Install::fake(&kind).ok_or_else(|| format!("FTM_UPDATE_FAKE_INSTALL: unknown install {kind:?}"))?)
        },
        _ => None,
    };
    let faking = fake_version.is_some() || fake_install.is_some();
    let install = fake_install.unwrap_or_else(install);
    if !wanted(settings.check, manual, faking, &install) {
        return Ok(None);
    }
    let channel = settings.channel.unwrap_or_else(Channel::current);
    let current = fake_version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let update = check(&current, channel, &install)?;
    Ok(Some(Checked { current, update }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Channel::{Nightly, Stable};
    use std::collections::{HashMap, HashSet};

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

    const STABLE_ASSETS: [&str; 10] = [
        "FTM-0.1.4-1.aarch64.rpm",
        "FTM-0.1.4-1.x86_64.rpm",
        "FTM_0.1.4_aarch64.app.tar.gz",
        "FTM_0.1.4_aarch64.dmg",
        "FTM_0.1.4_amd64.deb",
        "FTM_0.1.4_arm64.deb",
        "FTM_0.1.4_x64.app.tar.gz",
        "FTM_0.1.4_x64.dmg",
        "PKGBUILD.sig",
        "PKGBUILD",
    ];
    const NIGHTLY_ASSETS: [&str; 9] = [
        "FTM.Nightly-0.1.4-nightly.20261014.11-1.aarch64.rpm",
        "FTM.Nightly-0.1.4-nightly.20261014.11-1.x86_64.rpm",
        "FTM.Nightly_0.1.4-nightly.20261014.11_aarch64.app.tar.gz",
        "FTM.Nightly_0.1.4-nightly.20261014.11_aarch64.dmg",
        "FTM.Nightly_0.1.4-nightly.20261014.11_amd64.deb",
        "FTM.Nightly_0.1.4-nightly.20261014.11_arm64.deb",
        "FTM.Nightly_0.1.4-nightly.20261014.11_x64.app.tar.gz",
        "FTM.Nightly_0.1.4-nightly.20261014.11_x64.dmg",
        "PKGBUILD",
    ];

    fn mac_app() -> Install {
        Install::MacApp {
            bundle: "/Applications/FTM.app".into(),
        }
    }

    fn brew() -> Install {
        Install::Brew {
            brew: "/opt/homebrew/bin/brew".into(),
        }
    }

    fn stable_update(install: &Install, arch: &str) -> Option<Update> {
        let current = Version::parse("0.1.3").unwrap();
        offer(release("v0.1.4", &STABLE_ASSETS), &current, Stable, Stable, install, arch)
    }

    #[test]
    fn only_newer_stable_releases() {
        let current = Version::parse("0.1.3").unwrap();
        let found = |r| offer(r, &current, Stable, Stable, &Install::Unknown, "x86_64").map(|u| u.version);
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
        let offered = |r, current: &Version, own, target| offer(r, current, own, target, &Install::Unknown, "x86_64");

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
    fn pacman_builds_the_pkgbuild() {
        let update = stable_update(&Install::Pacman, "x86_64").unwrap();
        assert!(update.prompt);
        assert_eq!(update.page, "https://github.com/Frazzer951/ftm/releases/tag/v0.1.4");
        let script = update.script.unwrap();
        assert!(
            script.contains("curl -fLO 'https://github.com/Frazzer951/ftm/releases/download/v0.1.4/PKGBUILD' &&"),
            "{script}"
        );
        assert!(
            script.contains("makepkg -si &&") && !script.contains("--noconfirm"),
            "{script}"
        );
        assert!(script.contains("relaunch FTM when it asks"), "{script}");
        assert!(update.how.contains("ftm-bin"), "{}", update.how);

        let current = Version::parse("0.1.3").unwrap();
        let only_deb = release("v0.1.4", &["FTM_0.1.4_amd64.deb"]);
        assert!(offer(only_deb, &current, Stable, Stable, &Install::Pacman, "x86_64").is_none());

        let update = offer(
            release("v0.1.4-nightly.20261014.11", &NIGHTLY_ASSETS),
            &current,
            Stable,
            Nightly,
            &Install::Pacman,
            "x86_64",
        )
        .unwrap();
        let script = update.script.unwrap();
        assert!(
            script.contains("FTM Nightly 0.1.4-nightly.20261014.11 is installed"),
            "{script}"
        );
        assert!(
            script.contains("Quit FTM and start FTM Nightly.") && !script.contains("relaunch"),
            "{script}"
        );
        assert!(update.how.contains("ftm-nightly-bin"), "{}", update.how);
        let nightly = Version::parse("0.1.4-nightly.20261014.11").unwrap();
        let update = offer(
            release("v0.1.4-nightly.20261015.12", &["PKGBUILD"]),
            &nightly,
            Nightly,
            Nightly,
            &Install::Pacman,
            "x86_64",
        );
        assert!(update.unwrap().script.unwrap().contains("relaunch FTM Nightly when it asks"));
        let update = offer(
            release("v0.1.3", &["PKGBUILD"]),
            &nightly,
            Nightly,
            Stable,
            &Install::Pacman,
            "x86_64",
        );
        assert!(update.unwrap().script.unwrap().contains("Quit FTM and start FTM."));
    }

    #[test]
    fn deb_and_rpm_take_this_arch_package() {
        let base = "https://github.com/Frazzer951/ftm/releases/download/v0.1.4";
        for (install, arch, file, tool) in [
            (Install::Deb, "x86_64", "FTM_0.1.4_amd64.deb", "apt"),
            (Install::Deb, "aarch64", "FTM_0.1.4_arm64.deb", "apt"),
            (Install::Rpm, "x86_64", "FTM-0.1.4-1.x86_64.rpm", "dnf"),
            (Install::Rpm, "aarch64", "FTM-0.1.4-1.aarch64.rpm", "dnf"),
        ] {
            let update = stable_update(&install, arch).unwrap();
            assert_eq!(update.download, Some(format!("{base}/{file}")));
            assert_eq!(
                update.commands.as_deref(),
                Some(format!("curl -fLO '{base}/{file}'\nsudo {tool} install './{file}'").as_str())
            );
            let script = update.script.unwrap();
            assert!(
                script.contains(&format!("curl -fLO '{base}/{file}' && sudo {tool} install './{file}' &&")),
                "{script}"
            );
            assert!(!script.contains(" -y"), "{script}");
            assert!(update.prompt);
        }

        let nightly = offer(
            release("v0.1.4-nightly.20261014.11", &NIGHTLY_ASSETS),
            &Version::parse("0.1.3").unwrap(),
            Stable,
            Nightly,
            &Install::Deb,
            "x86_64",
        )
        .unwrap();
        assert!(
            nightly
                .commands
                .unwrap()
                .ends_with("sudo apt install './FTM.Nightly_0.1.4-nightly.20261014.11_amd64.deb'")
        );
        // No package for this arch (yet): nothing to offer.
        assert!(stable_update(&Install::Deb, "riscv64").is_none());
        let current = Version::parse("0.1.3").unwrap();
        assert!(
            offer(
                release("v0.1.4", &["FTM_0.1.4_arm64.deb"]),
                &current,
                Stable,
                Stable,
                &Install::Deb,
                "x86_64"
            )
            .is_none()
        );
    }

    #[test]
    fn brew_upgrades_the_cask() {
        let update = stable_update(&brew(), "aarch64").unwrap();
        assert_eq!(update.commands.as_deref(), Some("brew upgrade --cask frazzer951/ftm/ftm"));
        let script = update.script.unwrap();
        assert!(
            script.contains("'/opt/homebrew/bin/brew' upgrade --cask frazzer951/ftm/ftm ||"),
            "{script}"
        );
        assert_eq!(update.download, None);

        let switch = offer(
            release("v0.1.4-nightly.20261014.11", &NIGHTLY_ASSETS),
            &Version::parse("0.1.3").unwrap(),
            Stable,
            Nightly,
            &brew(),
            "aarch64",
        )
        .unwrap();
        assert_eq!(
            switch.commands.as_deref(),
            Some("brew install --cask frazzer951/ftm/ftm@nightly")
        );
        let script = switch.script.unwrap();
        assert!(script.contains("upgrade --cask frazzer951/ftm/ftm@nightly; else"), "{script}");
        assert!(script.contains("Quit FTM and start FTM Nightly."), "{script}");
    }

    #[test]
    fn mac_app_gets_instructions_and_the_dmg() {
        let update = stable_update(&mac_app(), "aarch64").unwrap();
        assert_eq!(update.script, None);
        assert!(update.prompt);
        assert_eq!(
            update.download.as_deref(),
            Some("https://github.com/Frazzer951/ftm/releases/download/v0.1.4/FTM_0.1.4_aarch64.dmg")
        );
        assert!(update.how.contains("FTM_0.1.4_aarch64.dmg") && update.how.contains("into /Applications"));
        assert_eq!(
            update.commands.as_deref(),
            Some("xattr -dr com.apple.quarantine '/Applications/FTM.app'")
        );
        let intel = stable_update(&mac_app(), "x86_64").unwrap();
        assert!(intel.download.unwrap().ends_with("FTM_0.1.4_x64.dmg"));
    }

    #[test]
    fn checkouts_and_unknown_installs_only_say() {
        let source = stable_update(&Install::Source, "x86_64").unwrap();
        assert!(!source.prompt && source.script.is_none() && source.download.is_none());
        assert!(source.commands.unwrap().contains("scripts/install.sh"));
        let dev = stable_update(&Install::Dev, "x86_64").unwrap();
        assert!(!dev.prompt && dev.script.is_none());
        let unknown = stable_update(&Install::Unknown, "x86_64").unwrap();
        assert!(unknown.prompt && unknown.script.is_none() && unknown.commands.is_none());
    }

    #[test]
    fn script_refuses_urls_it_cannot_quote() {
        let current = Version::parse("0.1.3").unwrap();
        let mut quote = release("v0.1.4", &["PKGBUILD"]);
        quote.assets[0].browser_download_url = "https://example.com/a'b/PKGBUILD".into();
        assert!(offer(quote, &current, Stable, Stable, &Install::Pacman, "x86_64").is_none());
        let mut http = release("v0.1.4", &["PKGBUILD"]);
        http.assets[0].browser_download_url = "http://example.com/PKGBUILD".into();
        assert!(offer(http, &current, Stable, Stable, &Install::Pacman, "x86_64").is_none());
    }

    #[test]
    fn script_keeps_the_download_dir_on_failure() {
        let dir = std::env::temp_dir().join(format!("ftm-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = downloading_script("FTM", "0.1.4", false, "'https://127.0.0.1:1/PKGBUILD'", "makepkg -si");
        let out = Command::new("/bin/sh")
            .args(["-c", &script])
            .env("TMPDIR", &dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!out.status.success(), "{stdout}");
        assert!(stdout.contains("FTM update failed; the files are in"), "{stdout}");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?} in {}: {stdout}", dir.display());
        assert!(left[0].to_string_lossy().starts_with("ftm-update."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Default)]
    struct Fake {
        owners: HashMap<Manager, &'static str>,
        paths: HashSet<PathBuf>,
    }

    impl System for Fake {
        fn owner(&self, manager: Manager, _: &Path) -> Option<String> {
            self.owners.get(&manager).map(ToString::to_string)
        }
        fn exists(&self, path: &Path) -> bool {
            self.paths.contains(path)
        }
    }

    fn owned(manager: Manager, package: &'static str) -> Fake {
        Fake {
            owners: HashMap::from([(manager, package)]),
            ..Fake::default()
        }
    }

    #[test]
    fn detects_linux_packages() {
        let usr_bin = Path::new("/usr/bin/ftm-app");
        let linux = |exe, system: &Fake| detect(exe, Os::Linux, &[], system);
        assert_eq!(linux(usr_bin, &owned(Manager::Pacman, "ftm-bin")), Install::Pacman);
        assert_eq!(linux(usr_bin, &owned(Manager::Pacman, "ftm-nightly-bin")), Install::Pacman);
        assert_eq!(linux(usr_bin, &owned(Manager::Pacman, "ftm")), Install::Unknown);
        assert_eq!(linux(usr_bin, &owned(Manager::Dpkg, "ftm")), Install::Deb);
        assert_eq!(linux(usr_bin, &owned(Manager::Dpkg, "ftm-nightly")), Install::Deb);
        assert_eq!(linux(usr_bin, &owned(Manager::Rpm, "ftm")), Install::Rpm);
        assert_eq!(linux(usr_bin, &owned(Manager::Rpm, "ftm-nightly")), Install::Rpm);
        assert_eq!(linux(usr_bin, &owned(Manager::Rpm, "something-else")), Install::Unknown);
        assert_eq!(linux(usr_bin, &Fake::default()), Install::Unknown);
        // install.sh's ~/.local/lib/ftm is never a package's.
        let local = Path::new("/home/u/.local/lib/ftm/ftm-app");
        assert_eq!(linux(local, &owned(Manager::Pacman, "ftm-bin")), Install::Unknown);
        assert_eq!(linux(local, &owned(Manager::Dpkg, "ftm")), Install::Unknown);
        assert_eq!(
            detect(usr_bin, Os::Mac, &[], &owned(Manager::Pacman, "ftm-bin")),
            Install::Unknown
        );
    }

    #[test]
    fn detects_brew_casks_and_dmg_apps() {
        let prefixes = ["/opt/homebrew", "/usr/local"].map(PathBuf::from);
        let with = |paths: &[&str]| Fake {
            paths: paths.iter().map(PathBuf::from).collect(),
            ..Fake::default()
        };
        let mac = |exe: &str, system: &Fake| detect(Path::new(exe), Os::Mac, &prefixes, system);
        let stable = "/Applications/FTM.app/Contents/MacOS/ftm-app";
        let nightly = "/Applications/FTM Nightly.app/Contents/MacOS/ftm-app";

        let arm = with(&["/opt/homebrew/Caskroom/ftm", "/opt/homebrew/bin/brew"]);
        assert_eq!(
            mac(stable, &arm),
            Install::Brew {
                brew: "/opt/homebrew/bin/brew".into()
            }
        );
        assert_eq!(
            mac(nightly, &arm),
            Install::MacApp {
                bundle: "/Applications/FTM Nightly.app".into()
            }
        );
        let intel = with(&["/usr/local/Caskroom/ftm@nightly", "/usr/local/bin/brew"]);
        assert_eq!(
            mac(nightly, &intel),
            Install::Brew {
                brew: "/usr/local/bin/brew".into()
            }
        );
        // A Caskroom entry without its brew is a leftover.
        assert_eq!(mac(stable, &with(&["/opt/homebrew/Caskroom/ftm"])), mac_app());
        assert_eq!(mac(stable, &Fake::default()), mac_app());
        assert_eq!(
            mac("/Users/u/Applications/FTM.app/Contents/MacOS/ftm-app", &Fake::default()),
            Install::MacApp {
                bundle: "/Users/u/Applications/FTM.app".into()
            }
        );
        // A renamed bundle can't be the cask's.
        assert_eq!(
            mac("/Applications/My FTM.app/Contents/MacOS/ftm-app", &arm),
            Install::MacApp {
                bundle: "/Applications/My FTM.app".into()
            }
        );
        assert_eq!(mac("/usr/local/bin/ftm-app", &arm), Install::Unknown);
        assert_eq!(detect(Path::new(stable), Os::Linux, &prefixes, &arm), Install::Unknown);
    }

    #[test]
    fn fake_installs() {
        for kind in ["pacman", "brew", "mac_app", "deb", "rpm", "source", "dev", "unknown"] {
            let install = Install::fake(kind).unwrap();
            assert_eq!(serde_json::to_value(&install).unwrap()["kind"], kind);
        }
        assert_eq!(Install::fake("snap"), None);
    }

    #[test]
    fn wanted_checks() {
        let pacman = Install::Pacman;
        assert!(wanted(None, false, false, &pacman));
        assert!(!wanted(None, false, false, &Install::Dev));
        assert!(wanted(None, false, true, &Install::Dev));
        assert!(!wanted(Some(false), false, true, &pacman));
        assert!(wanted(Some(true), false, false, &Install::Dev));
        for check in [None, Some(false), Some(true)] {
            assert!(wanted(check, true, false, &Install::Dev));
            assert!(wanted(check, true, false, &pacman));
        }
    }
}
