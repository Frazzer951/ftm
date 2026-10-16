//! Opening a workspace's folder in another app: a fixed table of targets, each launched by a known command with the
//! folder passed as an argument, never through a shell.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Target {
    id: &'static str,
    name: &'static str,
    /// The menu item's text.
    label: &'static str,
    /// Detected and launched by bundle id; `None` for Finder, which is always there.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    bundle_id: Option<&'static str>,
    /// Detected on PATH and run as `<binary> <args> <path>`; `None` for the file manager, opened with xdg-open.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    binary: Option<(&'static str, &'static [&'static str])>,
}

const TARGETS: &[Target] = &[
    Target {
        id: "vscode",
        name: "VS Code",
        label: "Open in VS Code",
        bundle_id: Some("com.microsoft.VSCode"),
        binary: Some(("code", &[])),
    },
    Target {
        id: "gitkraken",
        name: "GitKraken",
        label: "Open in GitKraken",
        bundle_id: Some("com.axosoft.gitkraken"),
        binary: Some(("gitkraken", &["-p"])),
    },
    Target {
        id: "files",
        name: if cfg!(target_os = "macos") { "Finder" } else { "Files" },
        label: if cfg!(target_os = "macos") {
            "Reveal in Finder"
        } else {
            "Open folder"
        },
        bundle_id: None,
        binary: None,
    },
];

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OpenTarget {
    pub id: &'static str,
    pub name: &'static str,
    pub label: &'static str,
}

/// Targets whose app is installed, in table order.
pub fn installed() -> Vec<OpenTarget> {
    installed_with(is_installed)
}

fn installed_with(present: impl Fn(&Target) -> bool) -> Vec<OpenTarget> {
    TARGETS
        .iter()
        .filter(|t| present(t))
        .map(|t| OpenTarget {
            id: t.id,
            name: t.name,
            label: t.label,
        })
        .collect()
}

pub fn open(target: &str, path: &str) -> Result<(), String> {
    let target = TARGETS
        .iter()
        .find(|t| t.id == target)
        .ok_or_else(|| format!("unknown target {target:?}"))?;
    let path = Path::new(path);
    if !path.is_absolute() || !path.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    let mut command = command(target, path);
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    // Drained for as long as the child lives: a launched app that inherits the pipe must never find it full or closed.
    let stderr = Arc::new(Mutex::new(Vec::new()));
    if let Some(mut pipe) = child.stderr.take() {
        let stderr = stderr.clone();
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                keep_tail(&mut stderr.lock().unwrap_or_else(|e| e.into_inner()), &chunk[..n]);
            }
        });
    }
    match wait_briefly(&mut child, LAUNCH_WAIT) {
        Some(status) if !status.success() => {
            std::thread::sleep(Duration::from_millis(50));
            let stderr = stderr.lock().unwrap_or_else(|e| e.into_inner());
            Err(format!(
                "opening in {} failed: {}",
                target.name,
                String::from_utf8_lossy(&stderr).trim()
            ))
        },
        Some(_) => Ok(()),
        None => {
            // Still running is a launch that worked: on Linux, an app that was not already open runs in the
            // foreground of the command that started it. Reap it whenever it exits.
            std::thread::spawn(move || child.wait());
            Ok(())
        },
    }
}

/// The end of a launcher's stderr, which says why it failed. A launched app can hold the pipe open and keep writing
/// for as long as it runs.
const STDERR_KEEP: usize = 8 * 1024;

fn keep_tail(kept: &mut Vec<u8>, chunk: &[u8]) {
    kept.extend_from_slice(chunk);
    let excess = kept.len().saturating_sub(STDERR_KEEP);
    kept.drain(..excess);
}

/// How long a launcher gets to report failure before it counts as having started the app.
const LAUNCH_WAIT: Duration = Duration::from_secs(3);

fn wait_briefly(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if start.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "macos")]
fn is_installed(target: &Target) -> bool {
    target.bundle_id.is_none_or(app_installed)
}

/// Spotlight knows apps wherever they live; the usual folders cover a disabled or still-indexing Spotlight.
#[cfg(target_os = "macos")]
fn app_installed(bundle_id: &str) -> bool {
    let found = Command::new("mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{bundle_id}'"))
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty());
    found || app_folders().iter().any(|dir| folder_has_app(dir, bundle_id))
}

#[cfg(target_os = "macos")]
fn app_folders() -> Vec<std::path::PathBuf> {
    let mut dirs = vec![std::path::PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join("Applications"));
    }
    dirs
}

/// An app's Info.plist names its bundle id as a plain string, in both the XML and the binary format.
#[cfg(target_os = "macos")]
fn folder_has_app(dir: &Path, bundle_id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path().extension().is_some_and(|x| x == "app")
            && std::fs::read(e.path().join("Contents/Info.plist"))
                .is_ok_and(|plist| plist.windows(bundle_id.len()).any(|w| w == bundle_id.as_bytes()))
    })
}

/// GitKraken gets a `gitkraken://repo/<path>` URL: `open` hands that to the running app, while `--args -p` only
/// reaches a fresh launch.
#[cfg(target_os = "macos")]
fn command(target: &Target, path: &Path) -> Command {
    let mut command = Command::new("open");
    match target.bundle_id {
        None => command.arg(path),
        Some("com.axosoft.gitkraken") => command.arg(format!("gitkraken://repo{}", encode_path(path))),
        Some(id) => command.args(["-b", id]).arg(path),
    };
    command
}

#[cfg(not(target_os = "macos"))]
fn is_installed(target: &Target) -> bool {
    target.binary.is_none_or(|(binary, _)| on_path(binary))
}

#[cfg(not(target_os = "macos"))]
fn on_path(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| std::fs::metadata(dir.join(binary)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    })
}

/// The editors fork into the background and exit; the file manager is opened through xdg-open.
#[cfg(not(target_os = "macos"))]
fn command(target: &Target, path: &Path) -> Command {
    let (program, args) = target.binary.unwrap_or(("xdg-open", &[]));
    let mut command = Command::new(program);
    command.args(args).arg(path);
    command
}

/// Percent-encodes everything but unreserved characters and `/`; GitKraken decodes the path with
/// decodeURIComponent.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_end_of_stderr() {
        let mut kept = Vec::new();
        for n in 0..10_000u32 {
            keep_tail(&mut kept, format!("line {n}\n").as_bytes());
        }
        assert_eq!(kept.len(), STDERR_KEEP);
        assert!(kept.ends_with(b"line 9999\n"));
    }

    #[test]
    fn target_ids_are_unique() {
        let mut ids: Vec<_> = TARGETS.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TARGETS.len());
    }

    #[test]
    fn only_installed_targets_are_offered_in_order() {
        let offered = installed_with(|t| t.id != "vscode");
        assert_eq!(offered.iter().map(|t| t.id).collect::<Vec<_>>(), ["gitkraken", "files"]);
    }

    #[test]
    fn the_file_manager_is_always_offered() {
        assert!(installed().iter().any(|t| t.id == "files"));
    }

    #[test]
    fn rejects_unknown_targets_and_non_folders() {
        assert!(open("rm", "/tmp").unwrap_err().contains("unknown target"));
        assert!(open("files", "relative/dir").unwrap_err().contains("not a folder"));
        assert!(open("files", "/definitely/not/here").unwrap_err().contains("not a folder"));
    }

    #[test]
    fn encodes_paths_for_urls() {
        assert_eq!(encode_path(Path::new("/Users/me/dev/ftm")), "/Users/me/dev/ftm");
        assert_eq!(encode_path(Path::new("/a b/c#d?e%f/é")), "/a%20b/c%23d%3Fe%25f/%C3%A9");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_apps_by_bundle_id_without_spotlight() {
        let system = Path::new("/System/Applications");
        assert!(folder_has_app(system, "com.apple.calculator"));
        assert!(!folder_has_app(system, "com.example.not-installed"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_commands_pass_the_path_as_an_argument() {
        let target = |id| TARGETS.iter().find(|t| t.id == id).unwrap();
        let args = |id| {
            command(target(id), Path::new("/x/a b"))
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(args("vscode"), ["-b", "com.microsoft.VSCode", "/x/a b"]);
        assert_eq!(args("gitkraken"), ["gitkraken://repo/x/a%20b"]);
        assert_eq!(args("files"), ["/x/a b"]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_commands_pass_the_path_as_an_argument() {
        let target = |id| TARGETS.iter().find(|t| t.id == id).unwrap();
        let line = |id| {
            let c = command(target(id), Path::new("/x/a b"));
            let mut v = vec![c.get_program().to_string_lossy().into_owned()];
            v.extend(c.get_args().map(|a| a.to_string_lossy().into_owned()));
            v
        };
        assert_eq!(line("vscode"), ["code", "/x/a b"]);
        assert_eq!(line("gitkraken"), ["gitkraken", "-p", "/x/a b"]);
        assert_eq!(line("files"), ["xdg-open", "/x/a b"]);
    }
}
