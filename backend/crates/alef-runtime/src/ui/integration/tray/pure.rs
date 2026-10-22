// SPDX-License-Identifier: MIT OR Apache-2.0
//! Platform-independent tray logic: PNG decoding, ARGB32 conversion, a muda-free menu
//! plan for the Linux StatusNotifierItem backend, and SNI-to-event mapping. The native
//! Windows/macOS backend shares the decoder; the menu plan and SNI mapping are only
//! used on Linux (hence the scoped `allow(dead_code)` elsewhere).

use alef_core::{
    ids::SessionId,
    registry::window::{menu::check_items, tray},
};
use image::{ImageFormat, ImageReader, Limits};
use std::{
    collections::HashMap,
    io::{self, Cursor},
};

/// Same derives as `menu::model::Owner`: entries are compared by owner and the owner
/// is copied out of borrowed entries on `tick`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    pub window: u64,
    pub session: SessionId,
}

pub(crate) struct Icon {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub(crate) fn decode(png: &[u8]) -> io::Result<Icon> {
    if png.is_empty() || png.len() > tray::ICON_BYTES_LIMIT {
        return Err(invalid("tray PNG must be 1..256 KiB"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(tray::ICON_SIDE_LIMIT);
    limits.max_image_height = Some(tray::ICON_SIDE_LIMIT);
    limits.max_alloc = Some(tray::ICON_DECODED_LIMIT);
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    reader.limits(limits);
    let image = reader.decode().map_err(invalid)?.into_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || width > tray::ICON_SIDE_LIMIT || height > tray::ICON_SIDE_LIMIT
    {
        return Err(invalid("tray PNG dimensions must be 1..256"));
    }
    Ok(Icon {
        rgba: image.into_raw(),
        width,
        height,
    })
}

/// IconPixmap is ARGB32 in network byte order: one byte each of alpha, red, green, blue.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn argb32(icon: &Icon) -> Vec<u8> {
    icon.rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// Click events: Activate is the primary (left) click; the SNI menu request and the
/// secondary activation both surface as the right-click event, matching the native
/// mapping of the right button. SNI has no double-click event.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn sni_event(method: &str) -> Option<(&'static str, &'static str)> {
    match method {
        "activate" => Some(("runtime.tray.click", "left")),
        "secondary_activate" | "context_menu" => Some(("runtime.tray.right-click", "right")),
        _ => None,
    }
}

/// A menu tree without muda: tray menus forbid accelerators and roles, so the plan is
/// plain data ready for the com.canonical.dbusmenu serialization.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone)]
pub(crate) struct MenuNode {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub kind: alef_core::registry::window::menu::MenuKind,
    pub checked: bool,
    pub children: Vec<MenuNode>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone)]
pub(crate) struct TrayMenu {
    pub root: MenuNode,
    actions: HashMap<i32, String>,
}

impl TrayMenu {
    /// The same validation as the native plan (bound to 256 items). Accelerators and
    /// roles are rejected here too: `TrayCall::check` already refuses them for tray
    /// menus, and the plan keeps that contract even if the call check is bypassed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn build(items: &[alef_core::registry::window::menu::MenuItem]) -> io::Result<Self> {
        check_items(items).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        fn reject_tray_only(
            items: &[alef_core::registry::window::menu::MenuItem],
        ) -> Result<(), String> {
            items.iter().try_for_each(|item| {
                if item.accelerator.is_some() {
                    return Err("tray menus do not support accelerators".into());
                }
                if item.role.is_some() {
                    return Err("roles are not supported in tray menus on Linux".into());
                }
                reject_tray_only(item.items.as_deref().unwrap_or_default())
            })
        }
        reject_tray_only(items).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        use alef_core::registry::window::menu::MenuKind;
        fn visit(
            items: &[alef_core::registry::window::menu::MenuItem],
            enabled: bool,
            ordinal: &mut i32,
            actions: &mut HashMap<i32, String>,
        ) -> Vec<MenuNode> {
            items
                .iter()
                .map(|item| {
                    *ordinal += 1;
                    let id = *ordinal;
                    let enabled = enabled && item.enabled != Some(false);
                    if enabled
                        && item.role.is_none()
                        && matches!(item.effective_kind(), MenuKind::Normal | MenuKind::Check)
                    {
                        actions.insert(id, item.id.clone().expect("validated custom id"));
                    }
                    let children = visit(
                        item.items.as_deref().unwrap_or_default(),
                        enabled,
                        ordinal,
                        actions,
                    );
                    MenuNode {
                        id,
                        label: item.label.clone().unwrap_or_default(),
                        enabled,
                        kind: item.effective_kind(),
                        checked: item.checked.unwrap_or(false),
                        children,
                    }
                })
                .collect()
        }
        let mut actions = HashMap::new();
        let mut ordinal = 0;
        let children = visit(items, true, &mut ordinal, &mut actions);
        Ok(Self {
            root: MenuNode {
                id: 0,
                label: String::new(),
                enabled: true,
                kind: MenuKind::Submenu,
                checked: false,
                children,
            },
            actions,
        })
    }

    /// The page-visible id of an actionable item; `None` for separators and containers.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn action(&self, id: i32) -> Option<&str> {
        self.actions.get(&id).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::registry::window::menu::{MenuItem, MenuKind, MenuRole};

    fn item(id: &str, label: &str) -> MenuItem {
        MenuItem {
            kind: None,
            id: Some(id.into()),
            label: Some(label.into()),
            role: None,
            enabled: None,
            checked: None,
            accelerator: None,
            items: None,
        }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn decode_rejects_empty_oversized_and_invalid_png() {
        assert_eq!(
            decode(&[]).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            decode(&[0; 32]).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            decode(&vec![0; tray::ICON_BYTES_LIMIT + 1])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn decode_rejects_oversized_sides() {
        assert!(decode(&png(tray::ICON_SIDE_LIMIT + 1, 1)).is_err());
    }

    #[test]
    fn decode_reads_dimensions() {
        let icon = decode(&png(2, 3)).unwrap();
        assert_eq!((icon.width, icon.height), (2, 3));
        assert_eq!(icon.rgba.len(), 2 * 3 * 4);
    }

    #[test]
    fn argb32_packs_alpha_first_in_network_order() {
        let icon = Icon {
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            width: 2,
            height: 1,
        };
        assert_eq!(argb32(&icon), vec![4, 1, 2, 3, 8, 5, 6, 7]);
    }

    #[test]
    fn sni_methods_map_to_click_events() {
        assert_eq!(sni_event("activate"), Some(("runtime.tray.click", "left")));
        assert_eq!(
            sni_event("secondary_activate"),
            Some(("runtime.tray.right-click", "right"))
        );
        assert_eq!(
            sni_event("context_menu"),
            Some(("runtime.tray.right-click", "right"))
        );
        assert_eq!(sni_event("scroll"), None);
        assert_eq!(sni_event(""), None);
    }

    #[test]
    fn menu_plan_numbers_items_and_maps_actions() {
        let mut sub = item("inner", "Inner");
        sub.kind = Some(MenuKind::Submenu);
        sub.items = Some(vec![item("leaf", "Leaf")]);
        let mut off = item("off", "Disabled");
        off.enabled = Some(false);
        let mut sep = MenuItem {
            kind: Some(MenuKind::Separator),
            ..item("sep", "ignored")
        };
        sep.id = None;
        sep.label = None; // separators carry no label
        let plan = TrayMenu::build(&[sub, off, sep, item("plain", "Plain")]).unwrap();
        // Preorder numbering: the root is 0, then submenu=1, its leaf=2, disabled=3,
        // separator=4, plain=5.
        assert_eq!(plan.root.children[0].id, 1);
        assert_eq!(plan.action(2), Some("leaf"));
        assert_eq!(plan.action(3), None); // disabled items are not actionable
        assert_eq!(plan.action(4), None); // separator
        assert_eq!(plan.action(5), Some("plain"));
        assert_eq!(plan.action(0), None); // the root
        assert_eq!(plan.root.children.len(), 4);
        assert_eq!(plan.root.children[0].children.len(), 1);
    }

    #[test]
    fn menu_plan_rejects_accelerators_and_bad_trees() {
        let mut hot = item("hot", "Hot");
        hot.accelerator = Some("Ctrl+N".into());
        assert!(TrayMenu::build(&[hot]).is_err());
        assert!(TrayMenu::build(&[item("dup", "A"), item("dup", "B")]).is_err());
        assert!(TrayMenu::build(&[item("", "no id")]).is_err());
    }

    #[test]
    fn menu_plan_rejects_roles() {
        // A role item has neither an id nor a label (the menu validation), so only the role is set.
        let mut copy = item("", "");
        copy.id = None;
        copy.label = None;
        copy.role = Some(MenuRole::Copy);
        let error = TrayMenu::build(&[copy]).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("roles"));
    }
}
