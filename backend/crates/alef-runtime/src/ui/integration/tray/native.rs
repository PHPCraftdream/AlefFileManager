// SPDX-License-Identifier: MIT OR Apache-2.0
use super::model::{Backend, Icon};
use crate::ui::integration::menu::{model::Plan, native::build_menu};
use std::io;
use tray_icon::{TrayIcon, TrayIconBuilder};
pub(crate) struct Native;
fn icon(value: Icon) -> io::Result<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(value.rgba, value.width, value.height).map_err(io::Error::other)
}
impl Backend for Native {
    type Object = TrayIcon;
    fn create(&self, id: &str, value: Icon, tooltip: &str, plan: &Plan) -> io::Result<TrayIcon> {
        TrayIconBuilder::new()
            .with_id(id)
            .with_icon(icon(value)?)
            .with_tooltip(tooltip)
            .with_menu(Box::new(build_menu(plan)?))
            .with_menu_on_left_click(false)
            .build()
            .map_err(io::Error::other)
    }
    fn set_icon(&self, object: &mut TrayIcon, value: Icon) -> io::Result<()> {
        object
            .set_icon(Some(icon(value)?))
            .map_err(io::Error::other)
    }
    fn set_tooltip(&self, object: &mut TrayIcon, tooltip: &str) -> io::Result<()> {
        object.set_tooltip(Some(tooltip)).map_err(io::Error::other)
    }
    fn set_menu(&self, object: &mut TrayIcon, plan: &Plan) -> io::Result<()> {
        let menu = build_menu(plan)?;
        object.set_menu(Some(Box::new(menu)));
        Ok(())
    }
    fn destroy(&self, object: TrayIcon) {
        drop(object);
    }
}
