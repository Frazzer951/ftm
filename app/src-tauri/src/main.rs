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
        .manage(daemon::Client::start())
        .manage(daemon::Events::default())
        .invoke_handler(tauri::generate_handler![request, send, subscribe, git_root])
        .run(tauri::generate_context!())
        .expect("running FTM");
}
