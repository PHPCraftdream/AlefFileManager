// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{model::*, *};
use alef_core::{ids::SessionId, registry::window::menu::MenuItem};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
#[derive(Default)]
struct Fake {
    fail: Cell<bool>,
    ids: RefCell<Vec<String>>,
    menus: RefCell<Vec<muda::MenuId>>,
    drops: Rc<Cell<usize>>,
}
struct Object(Rc<Cell<usize>>);
impl Drop for Object {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}
impl Fake {
    fn result(&self) -> io::Result<()> {
        if self.fail.get() {
            Err(io::Error::other("fake failure"))
        } else {
            Ok(())
        }
    }
}
impl Backend for Fake {
    type Object = Object;
    fn create(
        &self,
        id: &str,
        _: Icon,
        _: &str,
        plan: &crate::ui::integration::menu::model::Plan,
    ) -> io::Result<Object> {
        self.result()?;
        self.ids.borrow_mut().push(id.into());
        self.menus.borrow_mut().push(plan.nodes[0].id.clone());
        Ok(Object(self.drops.clone()))
    }
    fn set_icon(&self, _: &mut Object, _: Icon) -> io::Result<()> {
        self.result()
    }
    fn set_tooltip(&self, _: &mut Object, _: &str) -> io::Result<()> {
        self.result()
    }
    fn set_menu(
        &self,
        _: &mut Object,
        plan: &crate::ui::integration::menu::model::Plan,
    ) -> io::Result<()> {
        self.result()?;
        self.menus.borrow_mut().push(plan.nodes[0].id.clone());
        Ok(())
    }
    fn destroy(&self, object: Object) {
        drop(object)
    }
}
fn owner() -> Owner {
    Owner {
        window: 7,
        session: SessionId(3),
    }
}
fn menu() -> Vec<MenuItem> {
    serde_json::from_value(json!([{"id":"action","label":"Action"}])).unwrap()
}
fn png() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}
fn create(token: &str) -> TrayCall {
    TrayCall::Create {
        owner: owner().session,
        token: token.into(),
        icon: png(),
        tooltip: "tip".into(),
        menu: menu(),
    }
}
fn destroy(session: SessionId) -> TrayCall {
    TrayCall::Destroy {
        owner: session,
        token: "t".into(),
    }
}
#[test]
fn events_require_live_owner_and_native_identity() {
    let backend = Fake::default();
    let mut table = Table::default();
    assert!(table.call(&backend, 7, create("t"), |_| false).is_err());
    assert_eq!(
        table.call(&backend, 7, create("t"), |_| true).unwrap(),
        Value::Null
    );
    let id = backend.ids.borrow()[0].clone();
    assert_eq!(table.route(&id, |_| true), Some((owner(), "t")));
    assert!(table.route(&id, |_| false).is_none());
    assert!(table.route("t", |_| true).is_none());
    let menu = backend.menus.borrow()[0].clone();
    assert_eq!(
        table.route_menu(&menu, |_| true),
        Some((owner(), "action", "t"))
    );
    assert!(table.route_menu(&menu, |_| false).is_none());
}
#[test]
fn replacement_and_destroy_eliminate_stale_menus_and_wrong_owner_cannot_destroy() {
    let backend = Fake::default();
    let mut table = Table::default();
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    let old = backend.menus.borrow()[0].clone();
    table
        .call(&backend, 7, destroy(SessionId(4)), |_| true)
        .unwrap();
    assert!(table.route_menu(&old, |_| true).is_some());
    table
        .call(
            &backend,
            7,
            TrayCall::SetMenu {
                owner: owner().session,
                token: "t".into(),
                menu: menu(),
            },
            |_| true,
        )
        .unwrap();
    let new = backend.menus.borrow()[1].clone();
    assert!(table.route_menu(&old, |_| true).is_none());
    assert!(table.route_menu(&new, |_| true).is_some());
    table
        .call(&backend, 7, destroy(owner().session), |_| false)
        .unwrap();
    assert!(table.route_menu(&new, |_| true).is_none());
    assert_eq!(backend.drops.get(), 1);
}
#[test]
fn native_failures_preserve_prior_routes_and_failed_create_is_not_inserted() {
    let backend = Fake::default();
    let mut table = Table::default();
    backend.fail.set(true);
    assert!(table.call(&backend, 7, create("t"), |_| true).is_err());
    assert!(table.is_empty());
    backend.fail.set(false);
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    let old = backend.menus.borrow()[0].clone();
    backend.fail.set(true);
    assert!(table
        .call(
            &backend,
            7,
            TrayCall::SetMenu {
                owner: owner().session,
                token: "t".into(),
                menu: menu()
            },
            |_| true
        )
        .is_err());
    assert!(table
        .call(
            &backend,
            7,
            TrayCall::SetIcon {
                owner: owner().session,
                token: "t".into(),
                icon: png()
            },
            |_| true
        )
        .is_err());
    assert!(table
        .call(
            &backend,
            7,
            TrayCall::SetTooltip {
                owner: owner().session,
                token: "t".into(),
                tooltip: "new".into()
            },
            |_| true
        )
        .is_err());
    assert!(table.route_menu(&old, |_| true).is_some());
    assert_eq!(backend.drops.get(), 0);
}
#[test]
fn sweep_window_reload_and_shutdown_drop_objects() {
    let backend = Fake::default();
    let mut table = Table::default();
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    table.release_window(&backend, 8);
    assert_eq!(backend.drops.get(), 0, "another window keeps its trays");
    table.release_window(&backend, 7);
    assert_eq!(backend.drops.get(), 1);
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    table.sweep(&backend, |o| o.session == SessionId(4));
    assert_eq!(backend.drops.get(), 2);
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    table.release_all(&backend);
    assert!(table.is_empty());
    assert_eq!(backend.drops.get(), 3);
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    drop(table);
    assert_eq!(backend.drops.get(), 4);
}
#[test]
fn a_token_cannot_be_created_twice() {
    let backend = Fake::default();
    let mut table = Table::default();
    table.call(&backend, 7, create("t"), |_| true).unwrap();
    let error = table.call(&backend, 7, create("t"), |_| true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(backend.ids.borrow().len(), 1, "the second one made nothing");
}
#[test]
fn click_filters_press_and_motion_and_names_buttons() {
    use tray_icon::{
        dpi::{PhysicalPosition, PhysicalSize},
        Rect,
    };
    let event = TrayIconEvent::Click {
        id: "native".into(),
        position: PhysicalPosition::new(0.0, 0.0),
        rect: Rect {
            position: PhysicalPosition::new(0.0, 0.0),
            size: PhysicalSize::new(1, 1),
        },
        button: MouseButton::Right,
        button_state: MouseButtonState::Down,
    };
    assert!(click(&event).is_none());
    let mut event = event;
    if let TrayIconEvent::Click { button_state, .. } = &mut event {
        *button_state = MouseButtonState::Up;
    }
    assert_eq!(
        click(&event),
        Some(("native", "runtime.tray.right-click", "right"))
    );
    let double = TrayIconEvent::DoubleClick {
        id: "native".into(),
        position: PhysicalPosition::new(0.0, 0.0),
        rect: Rect::default(),
        button: MouseButton::Left,
    };
    assert_eq!(
        click(&double),
        Some(("native", "runtime.tray.double-click", "left"))
    );
    let motion = TrayIconEvent::Move {
        id: "native".into(),
        position: PhysicalPosition::new(0.0, 0.0),
        rect: Rect::default(),
    };
    assert!(click(&motion).is_none());
}

#[test]
fn defensive_limits_and_invalid_png() {
    assert_eq!(
        decode(&[]).err().unwrap().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        decode(&[0; 32]).err().unwrap().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(decode(&vec![0; 256 * 1024 + 1]).is_err());
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(257, 1)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    assert!(decode(&bytes.into_inner()).is_err());
    let backend = Fake::default();
    let mut table = Table::default();
    for n in 0..8 {
        table
            .call(&backend, 7, create(&n.to_string()), |_| true)
            .unwrap();
    }
    assert_eq!(
        table
            .call(&backend, 7, create("ninth"), |_| true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
}
