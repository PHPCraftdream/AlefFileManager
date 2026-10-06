// SPDX-License-Identifier: MIT OR Apache-2.0
use muda::{Menu, PredefinedMenuItem};
use tray_icon::TrayIcon;
use winit::window::Window;

#[cfg(target_os = "windows")]
use muda::ContextMenu;
#[cfg(target_os = "windows")]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::log;

pub(super) fn fallback_menu() -> Menu {
    let open = muda::MenuItem::with_id("integration-open-dialog", "Spike: open dialog", true, None);
    let quit = muda::MenuItem::with_id("integration-quit", "Spike: quit", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]);
    menu
}

pub(super) fn build_menu(window: &Window) -> Result<(Menu, isize), Box<dyn std::error::Error>> {
    let menu = fallback_menu();
    #[cfg(target_os = "windows")]
    {
        let raw = window
            .window_handle()
            .map_err(|error| error.to_string())?
            .as_raw();
        let hwnd = match raw {
            RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            other => return Err(format!("unsupported raw window handle {other:?}").into()),
        };
        unsafe { menu.init_for_hwnd(hwnd)? };
        let popup = menu.hpopupmenu();
        log(&format!(
            "menu attached to the window (init_for_hwnd), hpopupmenu={popup:#x}"
        ));
        Ok((menu, hwnd))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        log("menu init_for_hwnd skipped: not on Windows");
        Ok((menu, 0))
    }
}

pub(super) fn build_tray(menu: Menu) -> Result<TrayIcon, Box<dyn std::error::Error>> {
    const ICON_PNG: &[u8] = include_bytes!("../../../../../../frontend/public/logo-32x32.png");
    let icon_rgba = image::load_from_memory(ICON_PNG)?.to_rgba8();
    let (width, height) = icon_rgba.dimensions();
    let icon = tray_icon::Icon::from_rgba(icon_rgba.into_raw(), width, height)?;
    let tray = tray_icon::TrayIconBuilder::new()
        .with_id("alef-integration-spike")
        .with_tooltip("Alef M0.3 integration spike")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()?;
    tray.set_visible(true)?;
    log("tray icon created");
    Ok(tray)
}
