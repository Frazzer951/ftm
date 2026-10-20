//! The menu bar (macOS) / system tray (Linux, as a StatusNotifierItem) icon.

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const SHOW: &str = "show";
const QUIT: &str = "quit";

/// macOS tints a template image to match the menu bar; KDE and GNOME show it as is, so Linux gets a light mark for
/// their default dark panels.
#[cfg(target_os = "macos")]
const ICON: &[u8] = include_bytes!("../icons/tray/template.png");
#[cfg(not(target_os = "macos"))]
const ICON: &[u8] = include_bytes!("../icons/tray/light.png");

pub fn install(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let menu = Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, SHOW, "Show FTM", true, None::<&str>)?,
            &MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?,
        ],
    )?;
    TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(ICON)?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(&app.package_info().name)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            SHOW => show(app),
            QUIT => app.exit(0),
            _ => {}
        })
        // Linux trays never report clicks (left click opens the menu there), so this is macOS and Windows only.
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        })
        .build(app)
}

fn show(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        let _ = super::main_window(app, true);
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}
