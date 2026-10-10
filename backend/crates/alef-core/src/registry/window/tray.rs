// SPDX-License-Identifier: MIT OR Apache-2.0
//! Document-owned tray requests. Icons are owned PNG bytes, decoded again by the native host.
use super::menu::{check_items, MenuItem};
use crate::ids::SessionId;

pub const ICON_BYTES_LIMIT: usize = 256 * 1024;
pub const ICON_SIDE_LIMIT: u32 = 256;
pub const ICON_DECODED_LIMIT: u64 = 256 * 256 * 8;
pub const TOOLTIP_BYTES_LIMIT: usize = 128;
pub const SESSION_LIMIT: usize = 8;

/// Tokens are opaque, module-generated strings, never native numeric identifiers.
/// A tray event carries the same token so the client can map it to its Tray object.
#[derive(Debug, Clone, PartialEq)]
pub enum TrayCall {
    Create {
        owner: SessionId,
        token: String,
        icon: Vec<u8>,
        tooltip: String,
        menu: Vec<MenuItem>,
    },
    SetIcon {
        owner: SessionId,
        token: String,
        icon: Vec<u8>,
    },
    SetTooltip {
        owner: SessionId,
        token: String,
        tooltip: String,
    },
    SetMenu {
        owner: SessionId,
        token: String,
        menu: Vec<MenuItem>,
    },
    Destroy {
        owner: SessionId,
        token: String,
    },
}

pub fn check_tooltip(tooltip: &str) -> Result<(), String> {
    if tooltip.len() > TOOLTIP_BYTES_LIMIT || tooltip.chars().any(char::is_control) {
        return Err("tray tooltip must be at most 128 UTF-8 bytes without controls".into());
    }
    Ok(())
}

pub fn check_menu(menu: &[MenuItem]) -> Result<(), String> {
    check_items(menu)?;
    fn accelerators(items: &[MenuItem]) -> bool {
        items.iter().any(|item| {
            item.accelerator.is_some() || item.items.as_deref().is_some_and(accelerators)
        })
    }
    if accelerators(menu) {
        return Err("tray menus do not support accelerators".into());
    }
    Ok(())
}

impl TrayCall {
    /// Checks metadata and compressed size. PNG decoding is required separately on every platform.
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::Create {
                icon,
                tooltip,
                menu,
                ..
            } => {
                check_icon_size(icon)?;
                check_tooltip(tooltip)?;
                check_menu(menu)
            }
            Self::SetIcon { icon, .. } => check_icon_size(icon),
            Self::SetTooltip { tooltip, .. } => check_tooltip(tooltip),
            Self::SetMenu { menu, .. } => check_menu(menu),
            Self::Destroy { .. } => Ok(()),
        }
    }
}

fn check_icon_size(icon: &[u8]) -> Result<(), String> {
    if icon.is_empty() || icon.len() > ICON_BYTES_LIMIT {
        return Err("tray PNG must be 1..256 KiB".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tooltips_accept_empty_and_count_utf8_bytes_without_controls() {
        for text in ["".to_owned(), "é".repeat(64)] {
            assert!(check_tooltip(&text).is_ok());
        }
        for text in ["é".repeat(65), "x\n".into(), "x\u{7f}".into()] {
            assert!(check_tooltip(&text).is_err());
        }
    }

    #[test]
    fn tray_menus_validate_the_whole_tree_and_reject_nested_accelerators() {
        let menu: Vec<MenuItem> = serde_json::from_value(json!([
            {"kind":"submenu","id":"parent","label":"Parent","items":[
                {"id":"child","label":"Child","accelerator":"Ctrl+K"}]}]))
        .unwrap();
        assert!(check_menu(&menu).is_err());
        let mut valid = menu.clone();
        valid[0].items.as_mut().unwrap()[0].accelerator = None;
        assert!(check_menu(&valid).is_ok());
        valid[0].items.as_mut().unwrap()[0].id = Some("parent".into());
        assert!(check_menu(&valid).is_err());
        assert!(check_menu(&[]).is_ok());
    }

    #[test]
    fn tray_calls_enforce_icon_size_and_metadata_limits() {
        let mut call = TrayCall::Create {
            owner: SessionId(1),
            token: "opaque".into(),
            icon: vec![1; ICON_BYTES_LIMIT],
            tooltip: String::new(),
            menu: vec![],
        };
        assert!(call.check().is_ok());
        if let TrayCall::Create { icon, .. } = &mut call {
            icon.push(1);
        }
        assert!(call.check().is_err());
        assert!(TrayCall::SetIcon {
            owner: SessionId(1),
            token: "opaque".into(),
            icon: vec![]
        }
        .check()
        .is_err());
        assert!(TrayCall::SetTooltip {
            owner: SessionId(1),
            token: "opaque".into(),
            tooltip: "\0".into()
        }
        .check()
        .is_err());
        assert!(TrayCall::SetMenu {
            owner: SessionId(1),
            token: "opaque".into(),
            menu: vec![]
        }
        .check()
        .is_ok());
        assert!(TrayCall::Destroy {
            owner: SessionId(1),
            token: "opaque".into()
        }
        .check()
        .is_ok());
    }
}
