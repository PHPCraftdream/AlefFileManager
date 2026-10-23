// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure broker-tray logic: defaults, the fixed two-item menu layout and the mapping
//! from menu item ids to commands. No GUI, no channels — everything is testable here.

use super::BrokerTrayCommand;

/// Stable menu ids shared by the native (muda) and Linux (dbusmenu) backends.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const SHOW_APPS_ID: &str = "broker-show-apps";
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) const QUIT_ID: &str = "broker-quit";
/// dbusmenu item ids: the root is 0, the two top-level items follow in display order.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const SHOW_APPS_ITEM: i32 = 1;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const QUIT_ITEM: i32 = 2;

pub(crate) const DEFAULT_TOOLTIP: &str = "Alef Broker";
pub(crate) const DEFAULT_SHOW_APPS: &str = "Список приложений";
pub(crate) const DEFAULT_QUIT: &str = "Выход";

/// One menu entry as data: the backend-independent shape of the fixed menu.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) struct Entry {
    pub id: &'static str,
    pub item: i32,
    pub label: String,
}

/// The fixed menu: two enabled top-level items, «Список приложений» before «Выход».
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn entries(show_apps: &str, quit: &str) -> [Entry; 2] {
    [
        Entry {
            id: SHOW_APPS_ID,
            item: SHOW_APPS_ITEM,
            label: show_apps.to_owned(),
        },
        Entry {
            id: QUIT_ID,
            item: QUIT_ITEM,
            label: quit.to_owned(),
        },
    ]
}

/// Native (muda) menu id to command.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn command(id: &str) -> Option<BrokerTrayCommand> {
    match id {
        SHOW_APPS_ID => Some(BrokerTrayCommand::ShowApps),
        QUIT_ID => Some(BrokerTrayCommand::Quit),
        _ => None,
    }
}

/// dbusmenu item id to command.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn command_item(item: i32) -> Option<BrokerTrayCommand> {
    match item {
        SHOW_APPS_ITEM => Some(BrokerTrayCommand::ShowApps),
        QUIT_ITEM => Some(BrokerTrayCommand::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_has_two_items_in_order() {
        let items = entries("Список приложений", "Выход");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "broker-show-apps");
        assert_eq!(items[0].item, 1);
        assert_eq!(items[0].label, "Список приложений");
        assert_eq!(items[1].id, "broker-quit");
        assert_eq!(items[1].item, 2);
        assert_eq!(items[1].label, "Выход");
    }

    #[test]
    fn menu_keeps_custom_labels() {
        let items = entries("Apps", "Quit");
        assert_eq!(items[0].label, "Apps");
        assert_eq!(items[1].label, "Quit");
    }

    #[test]
    fn native_ids_map_to_commands() {
        assert_eq!(
            command("broker-show-apps"),
            Some(BrokerTrayCommand::ShowApps)
        );
        assert_eq!(command("broker-quit"), Some(BrokerTrayCommand::Quit));
        assert_eq!(command("other"), None);
        assert_eq!(command(""), None);
    }

    #[test]
    fn dbusmenu_ids_map_to_commands() {
        assert_eq!(command_item(1), Some(BrokerTrayCommand::ShowApps));
        assert_eq!(command_item(2), Some(BrokerTrayCommand::Quit));
        assert_eq!(command_item(0), None); // the root
        assert_eq!(command_item(3), None);
    }
}
