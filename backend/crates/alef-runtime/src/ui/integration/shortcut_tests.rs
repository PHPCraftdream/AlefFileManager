// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use global_hotkey::GlobalHotKeyEvent;

#[test]
fn reuse_rejects_source_tagged_old_events_and_failed_release() {
    let (mut table, backend) = (Table::default(), tests::Fake::default());
    let old = tests::register(&mut table, &backend, tests::owner(1), "Ctrl+A");
    let id = backend.keys.borrow()[0].id;
    table.source.deliver(GlobalHotKeyEvent {
        id,
        state: HotKeyState::Pressed,
    });
    backend.fail_release.set(true);
    assert!(table.unregister(&backend, tests::owner(1), old).is_err());
    assert!(table.free.is_empty());
    assert_eq!(backend.barriers.get(), 0);
    assert!(table.entries[0].retiring);
    table.unregister(&backend, tests::owner(1), old).unwrap();
    assert_eq!(table.free, [id]);
    assert_eq!(backend.barriers.get(), 1);
    let new = tests::register(&mut table, &backend, tests::owner(2), "Ctrl+A");
    assert_eq!(backend.keys.borrow()[0].id, id);
    assert_ne!(old, new);
    let queued = table.source.drain().next().unwrap();
    assert_eq!(queued.token, old);
    assert!(table.route(queued, |_| true).is_none());
    table.source.deliver(GlobalHotKeyEvent {
        id,
        state: HotKeyState::Released,
    });
    assert!(table.source.drain().next().is_none());
    table.source.deliver(GlobalHotKeyEvent {
        id,
        state: HotKeyState::Pressed,
    });
    assert_eq!(
        table.route(table.source.drain().next().unwrap(), |_| true),
        Some((tests::owner(2), new))
    );
    table.unregister(&backend, tests::owner(1), old).unwrap();
    assert_eq!(table.entries.len(), 1);
}

#[test]
fn a_press_of_an_unregistered_id_is_not_queued() {
    let (mut table, backend) = (Table::default(), tests::Fake::default());
    let token = tests::register(&mut table, &backend, tests::owner(1), "Ctrl+A");
    let id = backend.keys.borrow()[0].id;
    table.unregister(&backend, tests::owner(1), token).unwrap();
    table.source.deliver(GlobalHotKeyEvent {
        id,
        state: HotKeyState::Pressed,
    });
    assert!(table.source.drain().next().is_none());
}

#[test]
fn native_barrier_must_complete_before_reuse() {
    let (mut table, backend) = (Table::default(), tests::Fake::default());
    let old = tests::register(&mut table, &backend, tests::owner(1), "Ctrl+A");
    backend.barrier_pending.set(true);
    table.unregister(&backend, tests::owner(1), old).unwrap();
    assert!(table.free.is_empty());
    assert_eq!(table.retired, [1]);
    tests::register(&mut table, &backend, tests::owner(2), "Ctrl+B");
    assert_eq!(backend.keys.borrow()[0].id, 2);
    backend.barrier_pending.set(false);
    tests::register(&mut table, &backend, tests::owner(3), "Ctrl+C");
    assert_eq!(backend.keys.borrow()[1].id, 1);
    assert!(table.retired.is_empty());
}

#[test]
fn churn_must_not_exhaust_native_ids() {
    let (mut table, backend) = (Table::default(), tests::Fake::default());
    for _ in 0..=0xBFFF {
        let token = tests::register(&mut table, &backend, tests::owner(1), "Ctrl+A");
        table.unregister(&backend, tests::owner(1), token).unwrap();
    }
}

#[derive(Default)]
struct Linear(tests::Fake);
impl Backend for Linear {
    fn reuse_ids(&self) -> bool {
        false
    }
    fn drain_native(&self) -> bool {
        false
    }
    fn register(&self, key: HotKey) -> io::Result<()> {
        self.0.register(key)
    }
    fn unregister(&self, key: HotKey) -> io::Result<()> {
        self.0.unregister(key)
    }
}

#[test]
fn native_ids_are_never_reused_where_nothing_drains_the_queue() {
    let (mut table, backend) = (Table::default(), Linear::default());
    let first = table.register(&backend, tests::owner(1), "Ctrl+A").unwrap();
    let id = backend.0.keys.borrow()[0].id;
    table
        .unregister(
            &backend,
            tests::owner(1),
            ShortcutToken(first.as_u64().unwrap()),
        )
        .unwrap();
    assert!(table.free.is_empty() && table.retired.is_empty());
    table.register(&backend, tests::owner(1), "Ctrl+A").unwrap();
    assert!(backend.0.keys.borrow()[0].id > id);
}

#[test]
fn native_ids_do_not_repeat_across_managers_where_they_are_not_reused() {
    let backend = Linear::default();
    let mut first = Table::default();
    first.register(&backend, tests::owner(1), "Ctrl+A").unwrap();
    let id = backend.0.keys.borrow()[0].id;
    let mut second = Table::default();
    second
        .register(&backend, tests::owner(1), "Ctrl+B")
        .unwrap();
    assert!(backend.0.keys.borrow()[1].id > id);
}

#[test]
fn an_id_the_system_refused_is_taken_again() {
    let (mut table, backend) = (Table::default(), tests::Fake::default());
    backend.conflict.set(true);
    assert!(table.register(&backend, tests::owner(1), "Ctrl+A").is_err());
    backend.conflict.set(false);
    tests::register(&mut table, &backend, tests::owner(1), "Ctrl+A");
    assert_eq!(backend.keys.borrow()[0].id, 1);
}

#[test]
fn tokens_are_js_safe_not_win32_limited() {
    let (mut table, backend) = (Table::default(), tests::Fake::default());
    let standin = ShortcutToken::allocate().unwrap();
    let token = tests::register(&mut table, &backend, tests::owner(1), "Ctrl+A");
    assert_ne!(token, standin);
    assert!(token.0 > standin.0);
    assert!(token.0 <= ShortcutToken::MAX);
    assert!(backend.keys.borrow()[0].id <= 0xBFFF);
}
