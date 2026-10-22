// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Linux broker tray: an org.kde.StatusNotifierItem with a com.canonical.dbusmenu
//! of two fixed items, exported on the session bus and registered at
//! org.kde.StatusNotifierWatcher. A minimal standalone reimplementation of
//! `ui/integration/tray/linux` (whose backends are bound to the document `Owner`
//! and to per-window menu plans): no GTK, one static item, no re-registration.
//! Without a session bus or a watcher host `run` fails with `Unsupported` at once;
//! when the watcher disappears the loop reports an error.

use super::{pure, Prepared, Shutdown};
use crate::ui::integration::tray_pure;
use crossbeam_channel::{bounded, Select, TryRecvError};
use std::{
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zbus::{
    connection::Connection,
    object_server::ObjectServer,
    proxy,
    zvariant::{ObjectPath, OwnedValue, Structure, Value},
};

const ITEM_PATH: &str = "/org/alef/broker/StatusNotifierItem";
const POLL: Duration = Duration::from_secs(2);
const CLICKS: usize = 64;

/// How the worker obtains its session-bus connection; tests substitute a
/// connector that fails without a bus, so nothing waits on the real environment.
pub(crate) type Connector = fn() -> Result<Connection, zbus::Error>;

#[proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait Watcher {
    /// The SNI convention: the argument is the object path of the item.
    fn register_status_notifier_item(&self, service: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> zbus::Result<bool>;
}

/// Connects to the session bus and requires a watcher host, like the app tray.
fn connect() -> Result<Connection, zbus::Error> {
    async_io::block_on(async {
        let conn = Connection::session().await?;
        let watcher = WatcherProxy::new(&conn).await?;
        if !watcher.is_status_notifier_host_registered().await? {
            return Err(zbus::Error::Failure("no SNI host".into()));
        }
        Ok(conn)
    })
}

struct ItemState {
    tooltip: String,
    pixmap: (i32, i32, Vec<u8>),
    menu_path: ObjectPath<'static>,
}

struct ItemIface {
    state: Arc<Mutex<ItemState>>,
}

struct MenuIface {
    clicks: crossbeam_channel::Sender<i32>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl ItemIface {
    #[zbus(property)]
    fn category(&self) -> &'static str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &'static str {
        "alef-broker"
    }

    #[zbus(property)]
    fn status(&self) -> &'static str {
        "Active"
    }

    #[zbus(property)]
    fn icon_name(&self) -> &'static str {
        ""
    }

    /// `a(iiay)`: width, height and ARGB32 rows per the SNI spec.
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        vec![self.state.lock().expect("item state").pixmap.clone()]
    }

    /// `(sa(iiay)ss)`: icon name, pixmaps, title and description.
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        let state = self.state.lock().expect("item state");
        (
            String::new(),
            vec![state.pixmap.clone()],
            state.tooltip.clone(),
            String::new(),
        )
    }

    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        self.state.lock().expect("item state").menu_path.clone()
    }

    fn activate(&self, _x: i32, _y: i32) {}

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}
}

/// The dbusmenu layout wire type: `(ia{sv}av)`, children boxed in variants.
#[derive(serde::Serialize, zbus::zvariant::Type)]
struct LayoutNode(
    i32,
    std::collections::HashMap<String, OwnedValue>,
    Vec<OwnedValue>,
);

fn owned(value: Value<'static>) -> Result<OwnedValue, zbus::zvariant::Error> {
    OwnedValue::try_from(value)
}

fn props(label: &str) -> std::collections::HashMap<String, OwnedValue> {
    let mut map = std::collections::HashMap::new();
    let mut put = |key: &str, value: Value<'static>| {
        if let Ok(value) = owned(value) {
            map.insert(key.to_owned(), value);
        }
    };
    put("label", Value::from(label.to_owned()));
    put("enabled", Value::from(true));
    put("visible", Value::from(true));
    put("type", Value::from("standard"));
    map
}

fn leaf(id: i32, label: &str) -> Result<OwnedValue, zbus::zvariant::Error> {
    owned(Value::from(Structure::from((
        id,
        props(label),
        Vec::<OwnedValue>::new(),
    ))))
}

fn layout() -> Result<LayoutNode, zbus::zvariant::Error> {
    let show = leaf(pure::SHOW_APPS_ITEM, pure::DEFAULT_SHOW_APPS)?;
    let quit = leaf(pure::QUIT_ITEM, pure::DEFAULT_QUIT)?;
    Ok(LayoutNode(0, props(""), vec![show, quit]))
}

fn failed(error: zbus::zvariant::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl MenuIface {
    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, LayoutNode)> {
        if parent_id != 0 {
            return Err(zbus::fdo::Error::Failed("no such menu id".into()));
        }
        Ok((1, layout().map_err(failed)?))
    }

    /// Per the dbusmenu spec: `(u revision, a(ia{sv}))`.
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(
        u32,
        Vec<(i32, std::collections::HashMap<String, OwnedValue>)>,
    )> {
        let nodes = ids
            .iter()
            .filter(|id| **id == pure::SHOW_APPS_ITEM || **id == pure::QUIT_ITEM)
            .map(|id| {
                (
                    *id,
                    props(if *id == pure::SHOW_APPS_ITEM {
                        pure::DEFAULT_SHOW_APPS
                    } else {
                        pure::DEFAULT_QUIT
                    }),
                )
            })
            .collect();
        Ok((1, nodes))
    }

    fn about_to_show(&self, _item_id: i32) -> bool {
        false // the layout is static
    }

    fn event(&self, item_id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        if event_id == "clicked" {
            let _ = self.clicks.send(item_id);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, Value<'_>, u32)>) -> Vec<i32> {
        events
            .iter()
            .filter(|(_, event_id, _, _)| event_id == "clicked")
            .map(|(item_id, _, _, _)| {
                let _ = self.clicks.send(*item_id);
                *item_id
            })
            .collect()
    }
}

fn to_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn unavailable(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("Broker tray is not available: {error}"),
    )
}

/// Exports the item and menu objects and registers the item at the watcher.
async fn export(
    conn: &Connection,
    prepared: &Prepared,
    clicks: crossbeam_channel::Sender<i32>,
) -> io::Result<()> {
    let server: &ObjectServer = conn.object_server();
    let menu_path: ObjectPath<'static> =
        ObjectPath::try_from(format!("{ITEM_PATH}/menu")).map_err(to_io)?;
    let state = Arc::new(Mutex::new(ItemState {
        tooltip: prepared.tooltip.clone(),
        pixmap: {
            let width = i32::try_from(prepared.icon.width).map_err(io::Error::other)?;
            let height = i32::try_from(prepared.icon.height).map_err(io::Error::other)?;
            (width, height, tray_pure::argb32(&prepared.icon))
        },
        menu_path: menu_path.clone(),
    }));
    server
        .at(ITEM_PATH, ItemIface { state })
        .await
        .map_err(to_io)?;
    server
        .at(menu_path.as_str(), MenuIface { clicks })
        .await
        .map_err(to_io)?;
    let watcher = WatcherProxy::new(conn).await.map_err(to_io)?;
    watcher
        .register_status_notifier_item(ITEM_PATH)
        .await
        .map_err(unavailable)?;
    Ok(())
}

async fn watcher_alive(conn: &Connection) -> bool {
    match WatcherProxy::new(conn).await {
        Ok(watcher) => watcher.is_status_notifier_host_registered().await.is_ok(),
        Err(_) => false,
    }
}

async fn unexport(conn: &Connection) {
    let server: &ObjectServer = conn.object_server();
    let _ = server.remove::<ItemIface, _>(ITEM_PATH).await;
    let _ = server
        .remove::<MenuIface, _>(concat!(ITEM_PATH, "/menu"))
        .await;
}

pub(crate) fn run(prepared: Prepared, shutdown: Arc<Shutdown>) -> io::Result<()> {
    run_with(connect, prepared, shutdown)
}

/// The loop: export, register, then wait for exit or clicks, checking the watcher
/// on every poll tick. A lost watcher (or bus) ends `run` with an error; `exit()`
/// wakes the select immediately and closes the connection.
pub(crate) fn run_with(
    connect: Connector,
    prepared: Prepared,
    shutdown: Arc<Shutdown>,
) -> io::Result<()> {
    let conn =
        connect().map_err(|error| unavailable(format!("no session bus or SNI host: {error}")))?;
    let (clicks, received) = bounded::<i32>(CLICKS);
    if let Err(error) = async_io::block_on(export(&conn, &prepared, clicks)) {
        async_io::block_on(unexport(&conn));
        return Err(error);
    }
    // The handle wakes the select through this channel; arm it before waiting.
    let (wake_send, wake_recv) = bounded::<()>(1);
    shutdown.arm(Box::new(move || {
        let _ = wake_send.send(());
    }));
    if shutdown.requested() {
        async_io::block_on(unexport(&conn));
        return Ok(());
    }
    let mut next_check = Instant::now() + POLL;
    let result = loop {
        let mut select = Select::new();
        let _wake = select.recv(&wake_recv);
        let _click = select.recv(&received);
        // Poll, never block forever: exit() and the watcher check stay live.
        let _ = select.select_timeout(POLL);
        // Drain whatever arrived; `select_timeout` may return a ready index we
        // did not keep, so both channels are read opportunistically.
        while let Ok(item) = received.try_recv() {
            if let Some(command) = pure::command_item(item) {
                let _ = prepared.commands.send(command);
            }
        }
        if shutdown.requested() {
            break Ok(());
        }
        if Instant::now() >= next_check {
            if !async_io::block_on(watcher_alive(&conn)) {
                break Err(unavailable("the StatusNotifierWatcher disappeared"));
            }
            next_check = Instant::now() + POLL;
        }
        if let Err(TryRecvError::Disconnected) = received.try_recv() {
            break Err(to_io("menu event channel closed"));
        }
    };
    async_io::block_on(unexport(&conn));
    drop(conn); // closing the connection drops the bus objects
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::broker_tray::{Prepared, Shutdown};
    use crate::ui::integration::tray_pure::Icon;

    fn prepared() -> Prepared {
        let (commands, _received) = bounded(8);
        Prepared {
            icon: Icon {
                rgba: vec![0; 4],
                width: 1,
                height: 1,
            },
            tooltip: "Alef Broker".into(),
            show_apps_label: "Список приложений".into(),
            quit_label: "Выход".into(),
            commands,
        }
    }

    #[test]
    fn no_bus_fails_immediately_with_unsupported() {
        fn no_bus() -> Result<Connection, zbus::Error> {
            Err(zbus::Error::Failure("no bus".into()))
        }
        let error = run_with(no_bus, prepared(), Arc::new(Shutdown::default()))
            .err()
            .expect("no bus must fail");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("no session bus"));
    }
}
