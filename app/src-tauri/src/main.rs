#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod crash_log;
mod daemon;
mod fonts;
mod open_in;
mod sounds;
mod themes;
mod update;

use ftm_core::config::{Channel, Settings};
use ftm_core::paths;
use ftm_core::protocol::{Request, Response};
use std::path::Path;
use std::process::Command;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
async fn request(client: State<'_, daemon::Client>, request: Request) -> Result<Response, String> {
    client.request(request).await
}

/// Fire-and-forget for keystrokes and resizes. A sync command runs in invocation order, which keeps input ordered.
#[tauri::command]
fn send(client: State<'_, daemon::Client>, request: Request) {
    client.fire(request);
}

#[tauri::command]
fn subscribe(app: AppHandle, events: State<'_, daemon::Events>) {
    let daemon = app.clone();
    events.restart(
        move |event| {
            let _ = app.emit("ftm-event", event);
        },
        move |version| {
            let _ = daemon.emit(
                "ftm-daemon",
                DaemonInfo {
                    version,
                    app_version: env!("CARGO_PKG_VERSION"),
                },
            );
        },
    );
}

#[derive(Clone, serde::Serialize)]
struct DaemonInfo {
    version: String,
    app_version: &'static str,
}

/// (x, y, width, height) in physical pixels.
type Rect = (i32, i32, u32, u32);

/// Whether at least 100x20 px of the window's title bar (its top 30 px) is on some display, so it can be grabbed.
fn title_bar_visible(window: Rect, displays: &[Rect]) -> bool {
    let (x, y, w, _) = window;
    let (right, bottom) = (x + w.cast_signed(), y + 30);
    displays.iter().any(|&(dx, dy, dw, dh)| {
        let width = right.min(dx + dw.cast_signed()) - x.max(dx);
        let height = bottom.min(dy + dh.cast_signed()) - y.max(dy);
        width >= 100 && height >= 20
    })
}

/// The window shrunk to at most 90% of `display` and centered on it.
fn centered_on(window: Rect, display: Rect) -> Rect {
    let (dx, dy, dw, dh) = display;
    let (w, h) = (window.2.min(dw * 9 / 10), window.3.min(dh * 9 / 10));
    (dx + ((dw - w) / 2).cast_signed(), dy + ((dh - h) / 2).cast_signed(), w, h)
}

/// Restored geometry can point at a display that is gone or rearranged, leaving the window where it cannot be seen
/// or dragged. Then fit and center it on the primary display.
#[tauri::command]
fn ensure_on_screen(window: tauri::WebviewWindow) -> Result<(), String> {
    let err = |e: tauri::Error| e.to_string();
    let (pos, size) = (window.outer_position().map_err(err)?, window.outer_size().map_err(err)?);
    let current = (pos.x, pos.y, size.width, size.height);
    let rect = |m: &tauri::Monitor| (m.position().x, m.position().y, m.size().width, m.size().height);
    let monitors = window.available_monitors().map_err(err)?;
    if title_bar_visible(current, &monitors.iter().map(rect).collect::<Vec<_>>()) {
        return Ok(());
    }
    let Some(primary) = window.primary_monitor().map_err(err)?.or_else(|| monitors.into_iter().next()) else {
        return Ok(());
    };
    let (x, y, w, h) = centered_on(current, rect(&primary));
    window.set_size(tauri::PhysicalSize::new(w, h)).map_err(err)?;
    window.set_position(tauri::PhysicalPosition::new(x, y)).map_err(err)
}

#[derive(serde::Serialize)]
struct GitInfo {
    root: String,
    /// The main repository's folder name, also inside a linked worktree (whose own root is named after the worktree).
    repo: String,
    branch: String,
}

/// Repo root, repo name and current branch (`HEAD` when detached) of `path`, or `None` outside a git repo.
#[tauri::command]
fn git_info(path: String) -> Option<GitInfo> {
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(Path::new(&path)).args(args).output().ok();
    let out = git(&["rev-parse", "--show-toplevel", "--git-common-dir"])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let (root, common) = (lines.next()?, lines.next()?);
    // Unlike `rev-parse --abbrev-ref HEAD`, this also names the branch of a repo with no commits yet.
    let branch = git(&["symbolic-ref", "--short", "-q", "HEAD"])
        .filter(|out| out.status.success())
        .map_or_else(
            || "HEAD".to_string(),
            |out| String::from_utf8_lossy(&out.stdout).trim().to_string(),
        );
    let common = Path::new(&path).join(common);
    let repo_dir = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent()?
    } else {
        &common
    };
    let repo = repo_dir.canonicalize().ok()?.file_name()?.to_string_lossy().into_owned();
    Some(GitInfo {
        root: root.to_string(),
        repo,
        branch,
    })
}

/// Enumerated once per app run, off the main thread.
#[tauri::command]
async fn list_fonts() -> Vec<fonts::FontFamily> {
    static FONTS: std::sync::OnceLock<Vec<fonts::FontFamily>> = std::sync::OnceLock::new();
    if let Some(cached) = FONTS.get() {
        return cached.clone();
    }
    let listed = tauri::async_runtime::spawn_blocking(fonts::families)
        .await
        .unwrap_or_default();
    FONTS.get_or_init(|| listed).clone()
}

/// Read once per app run, off the main thread.
#[tauri::command]
async fn list_themes() -> Vec<ftm_core::theme::ThemeEntry> {
    static THEMES: std::sync::OnceLock<Vec<ftm_core::theme::ThemeEntry>> = std::sync::OnceLock::new();
    if let Some(cached) = THEMES.get() {
        return cached.clone();
    }
    let listed = tauri::async_runtime::spawn_blocking(themes::all).await.unwrap_or_default();
    THEMES.get_or_init(|| listed).clone()
}

/// Detected once per app run, off the main thread: Spotlight can stall while it indexes.
#[tauri::command]
async fn open_targets() -> Vec<open_in::OpenTarget> {
    static TARGETS: std::sync::OnceLock<Vec<open_in::OpenTarget>> = std::sync::OnceLock::new();
    if let Some(cached) = TARGETS.get() {
        return cached.clone();
    }
    let found = tauri::async_runtime::spawn_blocking(open_in::installed)
        .await
        .unwrap_or_default();
    TARGETS.get_or_init(|| found).clone()
}

#[tauri::command]
async fn open_in(target: String, path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || open_in::open(&target, &path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn list_sounds() -> Vec<sounds::SoundEntry> {
    tauri::async_runtime::spawn_blocking(sounds::all).await.unwrap_or_default()
}

/// Raw bytes, which arrive in the webview as an `ArrayBuffer`.
#[tauri::command]
async fn sound_data(name: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || sounds::data(&name))
        .await
        .map_err(|e| e.to_string())?
        .map(tauri::ipc::Response::new)
}

#[tauri::command]
fn get_settings() -> Result<Settings, String> {
    Settings::load(&paths::settings_path()).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_settings(settings: Settings) -> Result<(), String> {
    settings.save(&paths::settings_path()).map_err(|e| e.to_string())
}

/// A newer release on the `[updates] channel` (else this build's), or the other channel's newest as a switch, if the
/// `[updates] check` setting (else whether this install can upgrade itself) asks to look.
/// Being offline or rate limited is only logged.
#[tauri::command]
async fn check_update() -> Option<update::Update> {
    tauri::async_runtime::spawn_blocking(|| {
        let updates = Settings::load(&paths::settings_path()).unwrap_or_default().updates;
        let installable = update::installable();
        if !updates.check.unwrap_or(installable) {
            return None;
        }
        let channel = updates.channel.unwrap_or_else(Channel::current);
        update::check(env!("CARGO_PKG_VERSION"), channel, installable)
            .inspect_err(|e| eprintln!("update check: {e}"))
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
fn git_root(path: String) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(Path::new(&path))
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The default macOS menu binds Cmd+W to Close Window; FTM uses it to close a pane. Edit stays for copy and paste.
#[cfg(target_os = "macos")]
fn menu(app: &AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, PredefinedMenuItem as Item, Submenu};
    let app_menu = Submenu::with_items(
        app,
        &app.package_info().name,
        true,
        &[
            &Item::about(app, None, None)?,
            &Item::separator(app)?,
            &Item::hide(app, None)?,
            &Item::quit(app, None)?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &Item::copy(app, None)?,
            &Item::paste(app, None)?,
            &Item::select_all(app, None)?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[&Item::minimize(app, None)?, &Item::fullscreen(app, None)?],
    )?;
    Menu::with_items(app, &[&app_menu, &edit, &window])
}

fn main_window(app: &AppHandle, focused: bool) -> tauri::Result<()> {
    let builder = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
        .title(&app.package_info().name)
        .inner_size(1280.0, 820.0)
        .focused(focused);
    #[cfg(target_os = "macos")]
    let builder = match webview_configuration() {
        Some(config) => builder.with_webview_configuration(config),
        None => builder,
    };
    builder.build().map(drop)
}

/// xterm takes keystrokes through a hidden textarea, so WebKit treats the terminal as a text field and macOS 27 floats
/// its Siri / Writing Tools affordance next to the cursor whenever typing pauses. WebKit only suppresses that when the
/// configuration's writingToolsBehavior is None, and WKWebView copies its configuration at init, so it has to be set
/// here rather than on the live webview.
#[cfg(target_os = "macos")]
fn webview_configuration() -> Option<objc2::rc::Retained<objc2_web_kit::WKWebViewConfiguration>> {
    use objc2::{msg_send, sel};

    const NS_WRITING_TOOLS_BEHAVIOR_NONE: isize = -1;
    let mtm = objc2::MainThreadMarker::new()?;
    let config = unsafe { objc2_web_kit::WKWebViewConfiguration::new(mtm) };
    // macOS 15.0+; older systems have no Writing Tools to turn off.
    let supported: bool = unsafe { msg_send![&*config, respondsToSelector: sel!(setWritingToolsBehavior:)] };
    if !supported {
        return None;
    }
    let _: () = unsafe { msg_send![&*config, setWritingToolsBehavior: NS_WRITING_TOOLS_BEHAVIOR_NONE] };
    Some(config)
}

/// Frontend errors, so they show up in the terminal running the app.
#[tauri::command]
fn log(message: String) {
    eprintln!("[frontend] {message}");
}

fn main() {
    crash_log::install();
    // FTM_BACKGROUND=1 (for scripted launches): never take focus from whatever the user is doing.
    let background = std::env::var("FTM_BACKGROUND").is_ok_and(|v| v == "1");
    // WebKitGTK's DMA-BUF renderer dies with "Error 71 (Protocol error) dispatching to Wayland display" on NVIDIA;
    // fall back to the shared-memory path unless the user chose otherwise. Set before any thread starts.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    let ftmd = daemon::Daemon::new();
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu).activate_ignoring_other_apps(!background);
    builder
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(daemon::Client::start(ftmd.clone()))
        .manage(daemon::Events::new(ftmd))
        .setup(move |app| {
            // Launch finishes by activating the app (tao calls activate); a Prohibited app cannot be activated, so
            // start as one and become an ordinary accessory once launched (see Ready below).
            #[cfg(target_os = "macos")]
            if background {
                app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
            }
            main_window(app.handle(), !background)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            request,
            send,
            subscribe,
            git_root,
            git_info,
            get_settings,
            save_settings,
            list_fonts,
            list_themes,
            list_sounds,
            sound_data,
            open_targets,
            open_in,
            log,
            ensure_on_screen,
            check_update
        ])
        .build(tauri::generate_context!())
        .expect("building FTM")
        .run(move |_app, event| {
            #[cfg(target_os = "macos")]
            if background && matches!(event, tauri::RunEvent::Ready) {
                let _ = _app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                use tauri::Manager;
                if let Some(window) = _app.get_webview_window("main") {
                    let _ = window.show();
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (background, event);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = (0, 0, 3456, 2234);
    const ABOVE: Rect = (450, -2880, 6880, 2880);

    #[test]
    fn title_bar_must_be_reachable() {
        assert!(title_bar_visible((100, 100, 2560, 1640), &[MAIN]));
        assert!(title_bar_visible((1000, -2000, 2560, 1640), &[MAIN, ABOVE]));
        // A display that was unplugged.
        assert!(!title_bar_visible((1000, -2000, 2560, 1640), &[MAIN]));
        // Only a sliver of the title bar, or only the window's lower part, is on screen.
        assert!(!title_bar_visible((-2500, 100, 2560, 1640), &[MAIN]));
        assert!(!title_bar_visible((100, -1000, 2560, 1640), &[MAIN]));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=ftm",
                "-c",
                "user.email=ftm@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn info(dir: &Path) -> (String, String, String) {
        let info = git_info(dir.to_string_lossy().into_owned()).expect("a git repo");
        (info.root, info.repo, info.branch)
    }

    #[test]
    fn git_info_names_the_repo_and_branch() {
        let base = std::env::temp_dir().join(format!("ftm-git-info-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        let root = repo.to_string_lossy().into_owned();

        git(&repo, &["init", "-q", "-b", "trunk"]);
        assert_eq!(info(&repo), (root.clone(), "project".into(), "trunk".into()));

        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        std::fs::create_dir(repo.join("sub")).unwrap();
        assert_eq!(info(&repo.join("sub")), (root.clone(), "project".into(), "trunk".into()));

        let worktree = base.join("feature-wt");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feature", &worktree.to_string_lossy()],
        );
        let worktree = worktree.canonicalize().unwrap();
        assert_eq!(
            info(&worktree),
            (worktree.to_string_lossy().into_owned(), "project".into(), "feature".into())
        );

        git(&repo, &["checkout", "-q", "--detach"]);
        assert_eq!(info(&repo), (root, "project".into(), "HEAD".into()));

        assert!(git_info(base.to_string_lossy().into_owned()).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn centering_fits_the_display() {
        assert_eq!(centered_on((-9000, -9000, 2560, 1640), MAIN), (448, 297, 2560, 1640));
        assert_eq!(centered_on((0, 0, 8000, 4000), MAIN), (173, 112, 3110, 2010));
    }
}
