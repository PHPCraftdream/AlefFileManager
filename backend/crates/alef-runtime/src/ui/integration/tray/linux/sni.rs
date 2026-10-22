// SPDX-License-Identifier: MIT OR Apache-2.0
//! The zbus side of the Linux tray: an org.kde.StatusNotifierItem object and its
//! com.canonical.dbusmenu menu, exported on the session bus and registered at
//! org.kde.StatusNotifierWatcher. Everything is private to the Linux tray module.

use super::{not_available, Command, Event};
use crate::ui::integration::tray_pure::{MenuNode, TrayMenu};
use crossbeam_channel::Sender;
use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use zbus::{
    connection::Connection,
    object_server::{ObjectServer, SignalEmitter},
    proxy,
    zvariant::{ObjectPath, OwnedValue, Structure, Value},
};

/// Shared, mutable per-tray menu state read by the exported dbusmenu methods.
pub(crate) struct MenuState {
    pub menu: TrayMenu,
    pub revision: u32,
}

impl MenuState {
    pub(crate) fn new(menu: TrayMenu) -> Self {
        Self { menu, revision: 1 }
    }
}

struct ItemState {
    id: String,
    tooltip: String,
    pixmap: (i32, i32, Vec<u8>),
    menu_path: ObjectPath<'static>,
    menu: Arc<Mutex<MenuState>>,
    events: Sender<Event>,
}

struct ItemIface {
    state: Arc<Mutex<ItemState>>,
}

struct MenuIface {
    id: String,
    menu: Arc<Mutex<MenuState>>,
    events: Sender<Event>,
}

/// The item's worker-side handle: shared state and the emitter announcing changes.
struct Item {
    state: Arc<Mutex<ItemState>>,
    emitter: SignalEmitter<'static>,
    menu_emitter: SignalEmitter<'static>,
    menu_path: ObjectPath<'static>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl ItemIface {
    #[zbus(property)]
    fn category(&self) -> &'static str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> String {
        self.state.lock().expect("item state").id.clone()
    }

    #[zbus(property)]
    fn title(&self) -> &'static str {
        ""
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

    /// The object path of the exported com.canonical.dbusmenu object.
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'static> {
        self.state.lock().expect("item state").menu_path.clone()
    }

    /// Left click on the icon.
    fn activate(&self, _x: i32, _y: i32) {
        self.send("activate");
    }

    /// The menu request some hosts make for the right button.
    fn context_menu(&self, _x: i32, _y: i32) {
        self.send("context_menu");
    }

    /// Middle click, or right click on hosts that do not call `ContextMenu`.
    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.send("secondary_activate");
    }

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    pub async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    /// Declared for spec completeness; the item never changes status.
    #[allow(dead_code)]
    #[zbus(signal)]
    pub async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

impl ItemIface {
    fn send(&self, method: &str) {
        if let Some((name, button)) = crate::ui::integration::tray_pure::sni_event(method) {
            let state = self.state.lock().expect("item state");
            // A full queue drops events, exactly like the native forwarder.
            let _ = state.events.send(Event::Tray {
                id: state.id.clone(),
                name,
                button,
            });
        }
    }
}

/// The dbusmenu layout wire type: `(ia{sv}av)` per the spec, children boxed in variants.
#[derive(serde::Serialize, zbus::zvariant::Type)]
struct LayoutNode(i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn owned(value: Value<'static>) -> Result<OwnedValue, zbus::zvariant::Error> {
    OwnedValue::try_from(value)
}

fn props(node: &MenuNode) -> HashMap<String, OwnedValue> {
    use alef_core::registry::window::menu::MenuKind;
    let mut map = HashMap::new();
    let mut put = |key: &str, value: Value<'static>| {
        if let Ok(value) = owned(value) {
            map.insert(key.to_owned(), value);
        }
    };
    if !node.label.is_empty() {
        put("label", Value::from(node.label.clone()));
    }
    put("enabled", Value::from(node.enabled));
    put("visible", Value::from(true));
    match node.kind {
        MenuKind::Separator => put("type", Value::from("separator")),
        MenuKind::Check => {
            put("type", Value::from("standard"));
            put("toggle-type", Value::from("checkbox"));
            put("toggle-state", Value::from(i32::from(node.checked)));
        }
        MenuKind::Submenu => {
            put("type", Value::from("standard"));
            put("children-display", Value::from("submenu"));
        }
        MenuKind::Normal => put("type", Value::from("standard")),
    }
    map
}

/// A child of a layout: the node as the struct (ia{sv}av), boxed in a variant.
fn child_value(node: &MenuNode) -> Result<OwnedValue, zbus::zvariant::Error> {
    let children = node
        .children
        .iter()
        .map(child_value)
        .collect::<Result<Vec<_>, _>>()?;
    owned(Value::from(Structure::from((
        node.id,
        props(node),
        children,
    ))))
}

fn layout(node: &MenuNode) -> Result<LayoutNode, zbus::zvariant::Error> {
    let children = node
        .children
        .iter()
        .map(child_value)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LayoutNode(node.id, props(node), children))
}

fn failed(error: zbus::zvariant::Error) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

fn find<'a>(node: &'a MenuNode, id: i32) -> Option<&'a MenuNode> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, id))
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl MenuIface {
    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, LayoutNode)> {
        let state = self.menu.lock().expect("menu state");
        let root = &state.menu.root;
        let node = if parent_id == 0 {
            root
        } else {
            find(root, parent_id).unwrap_or(root)
        };
        Ok((state.revision, layout(node).map_err(failed)?))
    }

    /// Per the dbusmenu spec: `(u revision, a(ia{sv}))` — the revision and, for every
    /// requested id, the id paired with its properties.
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Vec<(i32, HashMap<String, OwnedValue>)>)> {
        let state = self.menu.lock().expect("menu state");
        let root = &state.menu.root;
        let nodes = ids
            .iter()
            .filter_map(|id| {
                if *id == 0 {
                    Some(root)
                } else {
                    find(root, *id)
                }
            })
            .map(|node| (node.id, props(node)))
            .collect::<Vec<_>>();
        Ok((state.revision, nodes))
    }

    fn about_to_show(&self, _item_id: i32) -> bool {
        false // the layout is pushed with LayoutUpdated instead
    }

    fn event(&self, item_id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        if event_id == "clicked" {
            let _ = self.events.send(Event::Menu {
                id: self.id.clone(),
                item: item_id,
            });
        }
    }

    fn event_group(&self, events: Vec<(i32, String, Value<'_>, u32)>) -> Vec<i32> {
        let mut handled = Vec::new();
        for (item_id, event_id, _, _) in events {
            if event_id == "clicked" {
                let _ = self.events.send(Event::Menu {
                    id: self.id.clone(),
                    item: item_id,
                });
                handled.push(item_id);
            }
        }
        handled
    }

    #[zbus(signal)]
    pub async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait Watcher {
    /// The SNI convention: the argument is the object path of the item to register.
    fn register_status_notifier_item(&self, service: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> zbus::Result<bool>;
}

fn to_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "Tray not found")
}

/// The type of `run_with`: how the worker obtains its session-bus connection. The
/// default blocks on `Connection::session` plus the SNI host check; tests substitute
/// a connector that fails without needing a bus.
pub(crate) type Connector = fn() -> Result<Connection, zbus::Error>;

/// Connects to the session bus and makes sure a StatusNotifierWatcher host is there.
/// Any failure means no tray: the caller reports the NOT_AVAILABLE error.
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

async fn dispatch(
    conn: &Connection,
    watcher: &WatcherProxy<'_>,
    events: &Sender<Event>,
    items: &mut HashMap<String, Item>,
    command: Command,
) -> io::Result<()> {
    let server: &ObjectServer = conn.object_server();
    match command {
        Command::Create {
            path,
            id,
            pixmap,
            tooltip,
            menu,
        } => {
            let menu_path: ObjectPath<'static> =
                ObjectPath::try_from(format!("{path}/menu")).map_err(to_io)?;
            let menu = Arc::new(Mutex::new(MenuState::new(menu)));
            let state = Arc::new(Mutex::new(ItemState {
                id: id.clone(),
                tooltip,
                pixmap,
                menu_path: menu_path.clone(),
                menu: menu.clone(),
                events: events.clone(),
            }));
            server
                .at(
                    path.as_str(),
                    ItemIface {
                        state: state.clone(),
                    },
                )
                .await
                .map_err(to_io)?;
            server
                .at(
                    menu_path.as_str(),
                    MenuIface {
                        id,
                        menu,
                        events: events.clone(),
                    },
                )
                .await
                .map_err(to_io)?;
            if let Err(error) = watcher.register_status_notifier_item(&path).await {
                // Roll back the exported objects: a half-registered tray must not stay
                // on the bus. Rollback errors are ignored on purpose.
                let _ = server.remove::<ItemIface, _>(path.as_str()).await;
                let _ = server.remove::<MenuIface, _>(menu_path.as_str()).await;
                return Err(to_io(error));
            }
            let emitter = SignalEmitter::from_parts(
                conn.clone(),
                ObjectPath::try_from(path.clone()).map_err(to_io)?,
            );
            // LayoutUpdated must come from the menu object, not from the item.
            let menu_emitter = SignalEmitter::from_parts(conn.clone(), menu_path.clone());
            items.insert(
                path,
                Item {
                    state,
                    emitter,
                    menu_emitter,
                    menu_path,
                },
            );
            Ok(())
        }
        Command::SetIcon { path, pixmap } => {
            let item = items.get(&path).ok_or_else(not_found)?;
            item.state.lock().expect("item state").pixmap = pixmap;
            ItemIface::new_icon(&item.emitter).await.map_err(to_io)?;
            Ok(())
        }
        Command::SetTooltip { path, tooltip } => {
            let item = items.get(&path).ok_or_else(not_found)?;
            item.state.lock().expect("item state").tooltip = tooltip;
            ItemIface::new_tool_tip(&item.emitter)
                .await
                .map_err(to_io)?;
            Ok(())
        }
        Command::SetMenu { path, menu } => {
            let item = items.get(&path).ok_or_else(not_found)?;
            // Take the menu `Arc` under a short-lived guard, then lock the menu state
            // on its own: no guard of the outer mutex stays alive across the inner
            // critical section.
            let menu_state = item.state.lock().expect("item state").menu.clone();
            let revision = {
                let mut state = menu_state.lock().expect("menu state");
                state.menu = menu;
                state.revision += 1;
                state.revision
            };
            MenuIface::layout_updated(&item.menu_emitter, revision, 0)
                .await
                .map_err(to_io)?;
            Ok(())
        }
        Command::Destroy { path } => {
            let menu_path = items
                .get(&path)
                .map(|item| item.menu_path.as_str().to_owned());
            server
                .remove::<ItemIface, _>(path.as_str())
                .await
                .map_err(to_io)?;
            if let Some(menu_path) = menu_path {
                server
                    .remove::<MenuIface, _>(menu_path.as_str())
                    .await
                    .map_err(to_io)?;
            }
            items.remove(&path);
            Ok(())
        }
    }
}

/// The worker loop: one thread, one connection, commands answered in order. A failed
/// connection reports NOT_AVAILABLE for the triggering call and for every later one.
pub(crate) fn run(
    incoming: crossbeam_channel::Receiver<super::Call>,
    events: Sender<Event>,
    failed: Arc<AtomicBool>,
) {
    run_with(connect, incoming, events, failed);
}

fn drain(incoming: crossbeam_channel::Receiver<super::Call>) {
    while let Ok(call) = incoming.recv() {
        let _ = call.reply.send(Err(not_available()));
    }
}

pub(crate) fn run_with(
    connect: Connector,
    incoming: crossbeam_channel::Receiver<super::Call>,
    events: Sender<Event>,
    failed: Arc<AtomicBool>,
) {
    // The flag goes up before anything is drained: from the very first failure every
    // later command must see the worker as unavailable.
    let conn = match connect() {
        Ok(conn) => conn,
        Err(_) => {
            failed.store(true, Ordering::Release);
            drain(incoming);
            return;
        }
    };
    let watcher = match async_io::block_on(WatcherProxy::new(&conn)) {
        Ok(watcher) => watcher,
        Err(_) => {
            failed.store(true, Ordering::Release);
            drain(incoming);
            return;
        }
    };
    let mut items = HashMap::new();
    while let Ok(call) = incoming.recv() {
        let result =
            async_io::block_on(dispatch(&conn, &watcher, &events, &mut items, call.command));
        let _ = call.reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::{find, layout, props, MenuState};
    use crate::ui::integration::tray_pure::TrayMenu;
    use alef_core::registry::window::menu::{MenuItem, MenuKind};
    use std::collections::HashMap;
    use std::{
        io,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    use zbus::zvariant::OwnedValue;

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

    fn menu() -> TrayMenu {
        let mut apps = item("apps", "Список приложений");
        apps.kind = Some(MenuKind::Submenu);
        let mut show = item("show", "Показать");
        show.kind = Some(MenuKind::Check);
        show.checked = Some(true);
        apps.items = Some(vec![show, item("hide", "Скрыть")]);
        let mut sep = MenuItem {
            kind: Some(MenuKind::Separator),
            ..item("sep", "ignored")
        };
        sep.id = None;
        sep.label = None; // separators carry no label
        TrayMenu::build(&[apps, sep, item("exit", "Выход")]).unwrap()
    }

    fn text<'a>(map: &'a HashMap<String, OwnedValue>, key: &str) -> &'a str {
        map.get(key)
            .and_then(|value| value.downcast_ref::<str>().ok())
            .unwrap_or_default()
    }

    fn flag(map: &HashMap<String, OwnedValue>, key: &str) -> bool {
        map.get(key)
            .and_then(|value| value.downcast_ref::<bool>().ok())
            .unwrap_or_default()
    }

    fn number(map: &HashMap<String, OwnedValue>, key: &str) -> i32 {
        map.get(key)
            .and_then(|value| value.downcast_ref::<i32>().ok())
            .unwrap_or_default()
    }

    #[test]
    fn get_layout_serializes_the_menu_tree() {
        let menu = menu();
        let state = MenuState::new(menu);
        assert_eq!(state.revision, 1);

        let node = &state.menu.root;
        let root = layout(node).expect("root layout");
        // The dbusmenu root: id 0 and every top-level item as a boxed child.
        assert_eq!(root.0, 0);
        // The root itself is advertised as a submenu container.
        assert_eq!(text(&root.1, "children-display"), "submenu");
        assert_eq!(root.2.len(), 3);

        let apps = &node.children[0];
        let apps_props = props(apps);
        assert_eq!(text(&apps_props, "label"), "Список приложений");
        assert_eq!(text(&apps_props, "type"), "standard");
        assert_eq!(text(&apps_props, "children-display"), "submenu");
        assert!(flag(&apps_props, "enabled"));
        assert!(flag(&apps_props, "visible"));
        let apps_node = layout(apps).expect("submenu layout");
        assert_eq!(apps_node.0, 1);
        assert_eq!(apps_node.2.len(), 2);

        let show = &apps.children[0];
        let show_props = props(show);
        assert_eq!(text(&show_props, "type"), "standard");
        assert_eq!(text(&show_props, "toggle-type"), "checkbox");
        assert_eq!(number(&show_props, "toggle-state"), 1);

        let separator_props = props(&node.children[1]);
        assert_eq!(text(&separator_props, "type"), "separator");
        assert!(!separator_props.contains_key("label"));

        let exit = &node.children[2];
        let exit_props = props(exit);
        // Preorder numbering: apps=1, show=2, hide=3, separator=4, exit=5.
        assert_eq!(exit.id, 5);
        assert_eq!(text(&exit_props, "label"), "Выход");
        assert_eq!(text(&exit_props, "type"), "standard");
        assert!(flag(&exit_props, "enabled"));
    }

    #[test]
    fn get_layout_finds_items_by_id() {
        let menu = menu();
        let state = MenuState::new(menu);
        let root = &state.menu.root;
        // The check item is a leaf: no children.
        assert_eq!(
            find(root, 2).map(|node| node.children.is_empty()),
            Some(true)
        );
        assert_eq!(find(root, 5).map(|node| node.label.as_str()), Some("Выход"));
        assert_eq!(find(root, 42).map(|node| node.id), None);
    }

    #[test]
    fn no_bus_reports_not_available_and_fails_immediately() {
        fn no_bus() -> Result<zbus::connection::Connection, zbus::Error> {
            Err(zbus::Error::Failure("no bus".into()))
        }
        let (calls, incoming) = crossbeam_channel::bounded::<super::Call>(1);
        let (events, _events) = crossbeam_channel::bounded::<super::Event>(16);
        let (reply, answers) = crossbeam_channel::bounded::<std::io::Result<()>>(1);
        let failed = Arc::new(AtomicBool::new(false));
        calls
            .send(super::Call {
                command: super::Command::SetIcon {
                    path: "/StatusNotifierItem/x".into(),
                    pixmap: (1, 1, vec![0, 0, 0, 0]),
                },
                reply,
            })
            .unwrap();
        super::run_with(no_bus, incoming, events, failed.clone());
        let error = answers.recv().unwrap().err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "Native trays need GTK, which a winit loop does not own"
        );
        assert!(failed.load(Ordering::Acquire));
    }
}
