#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;

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
    events.restart(move |event| {
        let _ = app.emit("ftm-event", event);
    });
}

/// (x, y, width, height) in physical pixels.
type Rect = (i32, i32, u32, u32);

/// Whether at least 100x20 px of the window's title bar (its top 30 px) is on some display, so it can be grabbed.
fn title_bar_visible(window: Rect, displays: &[Rect]) -> bool {
    let (x, y, w, _) = window;
    let (right, bottom) = (x + w as i32, y + 30);
    displays.iter().any(|&(dx, dy, dw, dh)| {
        let width = right.min(dx + dw as i32) - x.max(dx);
        let height = bottom.min(dy + dh as i32) - y.max(dy);
        width >= 100 && height >= 20
    })
}

/// The window shrunk to at most 90% of `display` and centered on it.
fn centered_on(window: Rect, display: Rect) -> Rect {
    let (dx, dy, dw, dh) = display;
    let (w, h) = (window.2.min(dw * 9 / 10), window.3.min(dh * 9 / 10));
    (dx + ((dw - w) / 2) as i32, dy + ((dh - h) / 2) as i32, w, h)
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
        "FTM",
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

fn main() {
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu);
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(daemon::Client::start())
        .manage(daemon::Events::default())
        .invoke_handler(tauri::generate_handler![request, send, subscribe, git_root, ensure_on_screen])
        .run(tauri::generate_context!())
        .expect("running FTM");
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

    #[test]
    fn centering_fits_the_display() {
        assert_eq!(centered_on((-9000, -9000, 2560, 1640), MAIN), (448, 297, 2560, 1640));
        assert_eq!(centered_on((0, 0, 8000, 4000), MAIN), (173, 112, 3110, 2010));
    }
}
