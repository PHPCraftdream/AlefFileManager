// SPDX-License-Identifier: MIT OR Apache-2.0
//! The desktop modules (`app` lifecycle, `dialog`, `shell`, `window`) through the registry.
#[path = "../common/mod.rs"]
mod common;

mod consent;
mod console;
mod dialog;
mod lifecycle;
mod shell;
mod ui {
    mod menu;
    mod shortcut;
    mod window;
}
