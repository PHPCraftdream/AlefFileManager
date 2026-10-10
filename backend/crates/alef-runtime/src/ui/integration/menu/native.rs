// SPDX-License-Identifier: MIT OR Apache-2.0
use super::model::{unsupported, Backend, Node, Plan, Target};
use alef_core::registry::window::menu::{MenuKind, MenuRole};
use muda::{CheckMenuItem, IsMenuItem, Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu};
use std::{collections::HashMap, io, rc::Rc};
#[cfg(target_os = "windows")]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::{event_loop::ActiveEventLoop, window::Window};
/// UI-thread-only boundary. Window Rcs keep HWNDs alive through detach.
/// Do not enable the integration spike's competing menu on these windows.
pub(crate) struct Native {
    #[cfg(target_os = "windows")]
    windows: HashMap<u64, Rc<Window>>,
    _ui: Rc<()>,
}
pub(crate) struct Tree {
    menu: Menu,
    attachment: Option<Attachment>,
}
enum Attachment {
    #[cfg(target_os = "windows")]
    Window(Rc<Window>),
    #[cfg(target_os = "macos")]
    Application,
}
impl Native {
    /// Construct on the active UI loop (main thread on macOS).
    pub(crate) fn new(_ui: &ActiveEventLoop, windows: HashMap<u64, Rc<Window>>) -> Self {
        #[cfg(not(target_os = "windows"))]
        let _ = windows;
        Self {
            #[cfg(target_os = "windows")]
            windows,
            _ui: Rc::new(()),
        }
    }
    pub(crate) fn update(&mut self, windows: HashMap<u64, Rc<Window>>) {
        #[cfg(target_os = "windows")]
        {
            self.windows = windows;
        }
        #[cfg(not(target_os = "windows"))]
        let _ = windows;
    }
    pub(crate) fn forget(&mut self, window: u64) {
        #[cfg(target_os = "windows")]
        self.windows.remove(&window);
        #[cfg(not(target_os = "windows"))]
        let _ = window;
    }
    /// Shows the plan as a context menu of `window`, at `at` (logical pixels of its client area)
    /// or at the pointer, and returns once it is closed: whether something was chosen. The choice
    /// itself comes as a menu event.
    #[cfg(target_os = "windows")]
    pub(crate) fn popup(
        &self,
        window: u64,
        plan: &Plan,
        at: Option<(f64, f64)>,
    ) -> io::Result<bool> {
        use muda::{
            dpi::{PhysicalPosition, Position},
            ContextMenu,
        };
        let window = self
            .windows
            .get(&window)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Menu window closed"))?;
        let handle = hwnd(window)?;
        let menu = Menu::with_id(plan.root.clone());
        for node in &plan.nodes {
            menu.append(item_ref(&build_node(node)?))
                .map_err(io::Error::other)?;
        }
        let position = match at {
            Some((x, y)) => {
                let origin = window.inner_position().map_err(io::Error::other)?;
                let scale = window.scale_factor();
                Some(Position::Physical(PhysicalPosition::new(
                    origin.x + (x * scale).round() as i32,
                    origin.y + (y * scale).round() as i32,
                )))
            }
            None => None,
        };
        // SAFETY: the Rc keeps the winit HWND live, this runs on the UI thread that owns it, and
        // the call returns when the menu is closed.
        Ok(unsafe { menu.show_context_menu_for_hwnd(handle, position) })
    }
    #[cfg(not(target_os = "windows"))]
    pub(crate) fn popup(
        &self,
        _window: u64,
        _plan: &Plan,
        _at: Option<(f64, f64)>,
    ) -> io::Result<bool> {
        Err(unsupported("Context menus are available on Windows only"))
    }
}
fn item_ref(item: &MenuItemKind) -> &dyn IsMenuItem {
    match item {
        MenuItemKind::MenuItem(i) => i,
        MenuItemKind::Check(i) => i,
        MenuItemKind::Submenu(i) => i,
        MenuItemKind::Predefined(i) => i,
        MenuItemKind::Icon(i) => i,
    }
}
pub(super) fn build_node(node: &Node) -> io::Result<MenuItemKind> {
    if let Some(role) = node.item.role {
        // A role cannot be disabled: muda has no switch for a predefined item.
        let text = node.item.label.as_deref();
        let item = match role {
            MenuRole::Copy => PredefinedMenuItem::copy(text),
            MenuRole::Cut => PredefinedMenuItem::cut(text),
            MenuRole::Paste => PredefinedMenuItem::paste(text),
            MenuRole::SelectAll => PredefinedMenuItem::select_all(text),
            MenuRole::Undo => PredefinedMenuItem::undo(text),
            MenuRole::Redo => PredefinedMenuItem::redo(text),
            MenuRole::Minimize => PredefinedMenuItem::minimize(text),
            // `quit` would end the process past `before-quit`, `about` needs what the
            // application is called: a menu item of the page does either with `app.quit()`.
            MenuRole::Quit | MenuRole::About => {
                return Err(unsupported("The roles quit and about are not available"))
            }
        };
        return Ok(MenuItemKind::Predefined(item));
    }
    let label = node.item.label.as_deref().unwrap_or_default();
    let enabled = node.item.enabled != Some(false);
    Ok(match node.item.effective_kind() {
        MenuKind::Normal => MenuItemKind::MenuItem(MenuItem::with_id(
            node.id.clone(),
            label,
            enabled,
            node.accelerator,
        )),
        MenuKind::Check => MenuItemKind::Check(CheckMenuItem::with_id(
            node.id.clone(),
            label,
            enabled,
            node.item.checked.unwrap_or(false),
            node.accelerator,
        )),
        // muda 0.19.3 offers no with_id for predefined separators; its own ID
        // is intentionally never entered in our actionable map (no events).
        MenuKind::Separator => MenuItemKind::Predefined(PredefinedMenuItem::separator()),
        MenuKind::Submenu => {
            let menu = Submenu::with_id(node.id.clone(), label, enabled);
            for child in &node.children {
                menu.append(item_ref(&build_node(child)?))
                    .map_err(io::Error::other)?;
            }
            MenuItemKind::Submenu(menu)
        }
    })
}
#[cfg(target_os = "windows")]
fn hwnd(window: &Window) -> io::Result<isize> {
    match window.window_handle().map_err(io::Error::other)?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get()),
        _ => Err(unsupported("Menu requires a Win32 window")),
    }
}
#[cfg(target_os = "windows")]
#[derive(Clone)]
pub(crate) struct AcceleratorHandle {
    pub hwnd: isize,
    pub haccel: isize,
    // Keep both the immutable menu tree and HWND alive across reentrant FFI.
    _menu: Menu,
    _window: Rc<Window>,
}
impl Tree {
    #[cfg(target_os = "windows")]
    pub(crate) fn accelerator(&self) -> Option<AcceleratorHandle> {
        let Some(Attachment::Window(window)) = &self.attachment else {
            return None;
        };
        Some(AcceleratorHandle {
            hwnd: hwnd(window).ok()?,
            haccel: self.menu.haccel(),
            _menu: self.menu.clone(),
            _window: window.clone(),
        })
    }
    fn detach(&mut self) -> io::Result<()> {
        match &self.attachment {
            #[cfg(target_os = "windows")]
            Some(Attachment::Window(window)) => {
                let hwnd = hwnd(window)?;
                // SAFETY: the Rc keeps the winit HWND live for this call. Tree
                // contains muda Rc state (!Send/!Sync), is created by Native on
                // the active UI loop and detached there before dropping that Rc.
                unsafe { self.menu.remove_for_hwnd(hwnd) }.map_err(io::Error::other)?;
            }
            #[cfg(target_os = "macos")]
            Some(Attachment::Application) => self.menu.remove_for_nsapp(),
            None => return Ok(()),
        }
        self.attachment = None;
        Ok(())
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = self.detach();
    }
}
impl Backend for Native {
    type Tree = Tree;
    fn build(&self, plan: &Plan) -> io::Result<Tree> {
        #[cfg(target_os = "macos")]
        if plan
            .nodes
            .iter()
            .any(|n| n.item.effective_kind() != MenuKind::Submenu)
        {
            return Err(unsupported("macOS application menu roots must be submenus"));
        }
        let menu = Menu::with_id(plan.root.clone());
        for item in &plan.nodes {
            menu.append(item_ref(&build_node(item)?))
                .map_err(io::Error::other)?;
        }
        Ok(Tree {
            menu,
            attachment: None,
        })
    }
    fn replace(&self, target: Target, next: &mut Tree, prior: Option<&mut Tree>) -> io::Result<()> {
        #[cfg(target_os = "windows")]
        {
            let Target::Window(id) = target else {
                return Err(unsupported(
                    "Windows application menus use the caller window",
                ));
            };
            let window = self
                .windows
                .get(&id)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "Menu target window closed")
                })?
                .clone();
            let handle = hwnd(&window)?; // validate BEFORE detaching prior
                                         // Pinned muda uses one subclass ID per HWND: detach old BEFORE init
                                         // new; removing old after init would erase the new subclass/menu.
            if let Some(prior) = prior {
                prior.detach()?;
                // SAFETY: current UI loop, Rc window remains live through
                // attachment and detach. muda menu.rs requires a valid HWND.
                if let Err(error) = unsafe { next.menu.init_for_hwnd(handle) } {
                    // SAFETY: same live HWND/UI thread; restore the old tree
                    // whose lifetime still covers the reinstalled subclass.
                    unsafe { prior.menu.init_for_hwnd(handle) }.map_err(io::Error::other)?;
                    prior.attachment = Some(Attachment::Window(window));
                    return Err(io::Error::other(error));
                }
            } else {
                // SAFETY: same UI-thread/live Rc HWND proof as above.
                unsafe { next.menu.init_for_hwnd(handle) }.map_err(io::Error::other)?;
            }
            next.attachment = Some(Attachment::Window(window));
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            if target != Target::Application {
                return Err(unsupported("macOS window menus are not supported"));
            }
            // Verified safe API: sets NSApplication.mainMenu; no NSView attach.
            next.menu.init_for_nsapp();
            next.attachment = Some(Attachment::Application);
            if let Some(prior) = prior {
                prior.attachment = None;
            }
            Ok(())
        }
    }
    fn active(&self, tree: &Tree) -> bool {
        tree.attachment.is_some()
    }
    fn remove(&self, _target: Target, tree: &mut Tree) -> io::Result<()> {
        tree.detach()
    }
}
