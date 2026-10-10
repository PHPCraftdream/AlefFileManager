// SPDX-License-Identifier: MIT OR Apache-2.0
use super::model::*;
use alef_core::ids::SessionId;
use alef_core::registry::window::menu::{MenuItem, MenuKind, MenuRole};
use muda::MenuId;
use std::{
    cell::{Cell, RefCell},
    io,
};
#[derive(Default)]
struct Fake {
    fail_build: Cell<bool>,
    fail_attach: Cell<bool>,
    fail_remove: Cell<bool>,
    fail_restore: Cell<bool>,
    attached: RefCell<Vec<(Target, MenuId)>>,
}
impl Backend for Fake {
    type Tree = MenuId;
    fn build(&self, plan: &Plan) -> io::Result<MenuId> {
        if self.fail_build.get() {
            Err(io::Error::other("build"))
        } else {
            Ok(plan.root.clone())
        }
    }
    fn replace(
        &self,
        target: Target,
        next: &mut MenuId,
        _prior: Option<&mut MenuId>,
    ) -> io::Result<()> {
        if self.fail_attach.get() {
            if self.fail_restore.get() {
                self.attached.borrow_mut().clear();
            }
            return Err(io::Error::other("attach"));
        }
        self.attached.borrow_mut().retain(|(t, _)| *t != target);
        self.attached.borrow_mut().push((target, next.clone()));
        Ok(())
    }
    fn active(&self, tree: &MenuId) -> bool {
        self.attached.borrow().iter().any(|(_, id)| id == tree)
    }
    fn remove(&self, target: Target, _: &mut MenuId) -> io::Result<()> {
        if self.fail_remove.get() {
            return Err(io::Error::other("detach"));
        }
        self.attached.borrow_mut().retain(|(t, _)| *t != target);
        Ok(())
    }
}
fn owner(window: u64, session: u64) -> Owner {
    Owner {
        window,
        session: SessionId(session),
    }
}
fn item(id: &str) -> MenuItem {
    MenuItem {
        id: Some(id.into()),
        label: Some(id.into()),
        ..Default::default()
    }
}
fn first_id(backend: &Fake) -> MenuId {
    let root = backend.attached.borrow()[0].1 .0.clone();
    MenuId::new(format!("{}1", root.strip_suffix('0').unwrap()))
}
#[test]
fn replacement_failures_preserve_prior_and_success_retires_ids() {
    let b = Fake::default();
    let mut t = Table::default();
    let a = owner(1, 1);
    t.set(&b, a, Target::Window(2), &[item("same")]).unwrap();
    let old = first_id(&b);
    for build in [true, false] {
        b.fail_build.set(build);
        b.fail_attach.set(!build);
        assert!(t
            .set(&b, owner(3, 2), Target::Window(2), &[item("same")])
            .is_err());
        assert_eq!(t.route(&old, |_| true), Some((a, "same")));
        assert_eq!(first_id(&b), old);
    }
    b.fail_attach.set(false);
    t.set(&b, owner(3, 2), Target::Window(2), &[item("same")])
        .unwrap();
    assert_ne!(first_id(&b), old);
    assert!(t.route(&old, |_| true).is_none());
    assert_eq!(
        t.route(&first_id(&b), |_| true),
        Some((owner(3, 2), "same"))
    );
    assert!(t.route(&MenuId::new("unknown"), |_| true).is_none());
}
#[test]
fn release_requires_original_owner_window_and_session() {
    let b = Fake::default();
    let mut t = Table::default();
    let a = owner(1, 1);
    t.set(&b, a, Target::Window(2), &[item("a")]).unwrap();
    for wrong in [owner(2, 1), owner(1, 2)] {
        t.release(&b, wrong).unwrap();
        t.set(&b, wrong, Target::Window(2), &[]).unwrap();
        assert_eq!(t.route(&first_id(&b), |_| true), Some((a, "a")));
    }
    t.release(&b, a).unwrap();
    t.release(&b, a).unwrap();
    assert!(b.attached.borrow().is_empty());
}
#[test]
fn release_window_matches_both_owner_and_target() {
    for window in [1, 2] {
        let b = Fake::default();
        let mut t = Table::default();
        t.set(&b, owner(1, 1), Target::Window(2), &[item("a")])
            .unwrap();
        t.set(&b, owner(3, 3), Target::Window(4), &[item("b")])
            .unwrap();
        t.release_window(&b, window);
        assert_eq!(b.attached.borrow().len(), 1);
        assert_eq!(t.route(&first_id(&b), |_| true), Some((owner(3, 3), "b")));
    }
}
#[test]
fn dead_and_retired_never_route_and_sweep_retries_detach() {
    let b = Fake::default();
    let mut t = Table::default();
    let a = owner(1, 1);
    t.set(&b, a, Target::Application, &[item("a")]).unwrap();
    let id = first_id(&b);
    assert!(t.route(&id, |_| false).is_none());
    b.fail_remove.set(true);
    t.sweep(&b, |_| false);
    assert!(t.route(&id, |_| true).is_none());
    b.fail_remove.set(false);
    t.sweep(&b, |_| true);
    assert!(b.attached.borrow().is_empty());
    assert!(t.route(&id, |_| true).is_none());
}
#[test]
fn mapping_ignores_disabled_ancestors_submenus_separators_and_roles() {
    let disabled = MenuItem {
        enabled: Some(false),
        ..item("disabled")
    };
    let branch = MenuItem {
        kind: Some(MenuKind::Submenu),
        enabled: Some(false),
        items: Some(vec![item("child")]),
        ..item("branch")
    };
    let check = MenuItem {
        kind: Some(MenuKind::Check),
        checked: Some(true),
        ..item("check")
    };
    let items = [
        item("normal"),
        disabled,
        branch,
        check,
        MenuItem {
            kind: Some(MenuKind::Separator),
            ..Default::default()
        },
        MenuItem {
            role: Some(MenuRole::Copy),
            ..Default::default()
        },
    ];
    let b = Fake::default();
    let mut t = Table::default();
    t.set(&b, owner(1, 1), Target::Window(1), &items).unwrap();
    let root = b.attached.borrow()[0].1 .0.clone();
    let prefix = root.strip_suffix('0').unwrap();
    for (ordinal, public) in [
        (0, None),
        (1, Some("normal")),
        (2, None),
        (3, None),
        (4, None),
        (5, Some("check")),
        (6, None),
        (7, None),
    ] {
        assert_eq!(
            t.route(&MenuId::new(format!("{prefix}{ordinal}")), |_| true)
                .map(|(_, id)| id),
            public
        );
    }
}
#[test]
fn an_enabled_submenu_is_not_chosen_but_its_children_are() {
    let branch = MenuItem {
        kind: Some(MenuKind::Submenu),
        items: Some(vec![item("child")]),
        ..item("branch")
    };
    let b = Fake::default();
    let mut t = Table::default();
    t.set(&b, owner(1, 1), Target::Window(1), &[branch])
        .unwrap();
    let root = b.attached.borrow()[0].1 .0.clone();
    let prefix = root.strip_suffix('0').unwrap();
    let routed = |ordinal: u8| {
        t.route(&MenuId::new(format!("{prefix}{ordinal}")), |_| true)
            .map(|(_, id)| id)
    };
    assert_eq!((routed(1), routed(2)), (None, Some("child")));
}
#[test]
fn validation_and_generation_of_ids() {
    let a = Plan::build(&[item("a")]).unwrap();
    let b = Plan::build(&[item("a")]).unwrap();
    assert_ne!(a.nodes[0].id, b.nodes[0].id);
    assert!(Plan::build(&[item("a"), item("a")]).is_err());
    let bad = MenuItem {
        accelerator: Some("not-a-key".into()),
        ..item("a")
    };
    assert_eq!(
        Plan::build(&[bad]).err().unwrap().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        target(true, 1, 2).unwrap(),
        if cfg!(target_os = "macos") {
            Target::Application
        } else {
            Target::Window(1)
        }
    );
}
#[test]
fn a_popup_is_at_both_coordinates_or_at_the_pointer() {
    assert_eq!(position(None, None).unwrap(), None);
    assert_eq!(position(Some(1.5), Some(-2.0)).unwrap(), Some((1.5, -2.0)));
    for (x, y) in [(Some(1.0), None), (None, Some(1.0))] {
        assert_eq!(
            position(x, y).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
#[test]
fn the_choice_of_a_popup_is_the_first_event_of_an_item_that_can_be_chosen() {
    let disabled = MenuItem {
        enabled: Some(false),
        ..item("disabled")
    };
    let plan = Plan::build(&[disabled, item("go"), item("other")]).unwrap();
    let id = |i: usize| plan.nodes[i].id.clone();
    let foreign = MenuId::new("not-ours");
    let mut events = vec![foreign.clone(), id(0), id(2), id(1)].into_iter();
    assert_eq!(chosen(&plan, || events.next()).as_deref(), Some("other"));
    let mut events = vec![foreign, id(0)].into_iter();
    assert_eq!(chosen(&plan, || events.next()), None);
    assert_eq!(chosen(&plan, || None), None);
}
#[cfg(target_os = "windows")]
#[test]
fn native_builder_kinds_accelerators_and_roles() {
    use super::native::build_node;
    let p = Plan::build(&[
        MenuItem {
            accelerator: Some("Ctrl+A".into()),
            ..item("a")
        },
        MenuItem {
            kind: Some(MenuKind::Check),
            checked: Some(true),
            ..item("b")
        },
        MenuItem {
            kind: Some(MenuKind::Submenu),
            items: Some(vec![item("c")]),
            ..item("branch")
        },
        MenuItem {
            kind: Some(MenuKind::Separator),
            ..Default::default()
        },
    ])
    .unwrap();
    let nodes: Vec<_> = p.nodes.iter().map(|n| build_node(n).unwrap()).collect();
    assert!(matches!(&nodes[0], muda::MenuItemKind::MenuItem(_)));
    assert!(matches!(&nodes[1], muda::MenuItemKind::Check(c) if c.is_checked()));
    assert!(matches!(&nodes[2], muda::MenuItemKind::Submenu(s) if s.items().len()==1));
    assert!(matches!(&nodes[3], muda::MenuItemKind::Predefined(_)));
    let role_item = |role| {
        let p = Plan::build(&[MenuItem {
            role: Some(role),
            ..Default::default()
        }])
        .unwrap();
        build_node(&p.nodes[0])
    };
    for role in [
        MenuRole::Copy,
        MenuRole::Paste,
        MenuRole::Cut,
        MenuRole::Undo,
        MenuRole::Redo,
        MenuRole::SelectAll,
        MenuRole::Minimize,
    ] {
        assert!(matches!(
            role_item(role),
            Ok(muda::MenuItemKind::Predefined(_))
        ));
    }
    for unavailable in [MenuRole::Quit, MenuRole::About] {
        assert_eq!(
            role_item(unavailable).err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
    }
    let named = Plan::build(&[MenuItem {
        role: Some(MenuRole::Copy),
        label: Some("Kopieren".into()),
        ..Default::default()
    }])
    .unwrap();
    assert!(matches!(
        build_node(&named.nodes[0]),
        Ok(muda::MenuItemKind::Predefined(p)) if p.text() == "Kopieren"
    ));
}

#[test]
fn failed_rollback_suppresses_prior_events_and_sweep_retires_it() {
    let b = Fake::default();
    let mut t = Table::default();
    t.set(&b, owner(1, 1), Target::Window(2), &[item("old")])
        .unwrap();
    let old = first_id(&b);
    b.fail_attach.set(true);
    b.fail_restore.set(true);
    assert!(t
        .set(&b, owner(1, 2), Target::Window(2), &[item("new")])
        .is_err());
    assert!(t.route(&old, |_| true).is_none());
    t.sweep(&b, |_| true);
    assert!(t.is_empty());
}

#[test]
fn sweep_deadline_does_not_slide_on_early_ticks() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut next = None;
    assert!(!super::sweep_due(false, &mut next, now));
    let deadline = now + super::SWEEP;
    for offset in [1, 100, 249] {
        assert!(!super::sweep_due(
            false,
            &mut next,
            now + Duration::from_millis(offset)
        ));
        assert_eq!(next, Some(deadline));
    }
    assert!(super::sweep_due(false, &mut next, deadline));
    assert_eq!(next, Some(deadline + super::SWEEP));
    assert!(!super::sweep_due(true, &mut next, deadline));
    assert_eq!(next, None);
}

#[tokio::test]
async fn live_owner_isolated_by_window_session_and_open_window() {
    use alef_core::{protocol::call::Limits, session::session::SessionManager};
    let sessions = SessionManager::new(
        std::sync::Arc::new(|| "token".to_owned()),
        Limits::default(),
    );
    let first = sessions.begin_document(1).await.id();
    let second_window = sessions.begin_document(2).await.id();
    let who = Owner {
        window: 1,
        session: first,
    };
    assert!(super::live_owner(&sessions, &[1, 2], who));
    assert!(!super::live_owner(&sessions, &[2], who));
    assert!(!super::live_owner(
        &sessions,
        &[1, 2],
        Owner {
            window: 2,
            session: first
        }
    ));
    assert!(!super::live_owner(
        &sessions,
        &[1, 2],
        Owner {
            window: 1,
            session: second_window
        }
    ));
    sessions.begin_document(1).await;
    assert!(!super::live_owner(&sessions, &[1, 2], who));
    assert!(super::live_owner(
        &sessions,
        &[1, 2],
        Owner {
            window: 2,
            session: second_window
        }
    ));
    sessions.close_window(2).await;
    assert!(!super::live_owner(
        &sessions,
        &[1, 2],
        Owner {
            window: 2,
            session: second_window
        }
    ));
}
