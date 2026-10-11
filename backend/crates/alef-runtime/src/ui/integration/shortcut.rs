// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native manager and registration table are owned exclusively by App on the UI thread.
use super::shortcut_source::{native_id, Source, TaggedEvent};
use crate::{
    bridge::EventBus,
    ui::{event_json, Wake},
};
use alef_core::{
    ids::SessionId,
    registry::window::shortcut::{ShortcutCall, ShortcutToken},
    session::session::SessionManager,
};
use global_hotkey::{hotkey::HotKey, GlobalHotKeyManager, HotKeyState};
use serde_json::Value;
use std::{
    io,
    time::{Duration, Instant},
};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
#[cfg(target_os = "linux")]
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

pub(crate) const PRESSED: &str = "runtime.shortcut.pressed";
const MAX_REGISTERED: usize = 128;
const SWEEP: Duration = Duration::from_millis(250);

trait Backend {
    fn register(&self, key: HotKey) -> io::Result<()>;
    fn unregister(&self, key: HotKey) -> io::Result<()>;
    fn reuse_ids(&self) -> bool {
        cfg!(windows)
    }
    fn drain_native(&self) -> bool {
        #[cfg(windows)]
        {
            super::shortcut_source::drain_native()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

fn native_error(error: global_hotkey::Error) -> io::Error {
    match error {
        global_hotkey::Error::AlreadyRegistered(_) => {
            io::Error::new(io::ErrorKind::AlreadyExists, error)
        }
        // The pinned macOS backend discards OSStatus and exposes this exact message for
        // Carbon registration failure (including another process owning the shortcut).
        global_hotkey::Error::FailedToRegister(ref message)
            if message.starts_with("RegisterEventHotKey failed for ") =>
        {
            io::Error::new(io::ErrorKind::AlreadyExists, error)
        }
        global_hotkey::Error::OsError(error) => error,
        other => io::Error::other(other),
    }
}
impl Backend for GlobalHotKeyManager {
    fn register(&self, key: HotKey) -> io::Result<()> {
        self.register(key).map_err(native_error)
    }
    fn unregister(&self, key: HotKey) -> io::Result<()> {
        self.unregister(key).map_err(native_error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owner {
    window: u64,
    session: SessionId,
}
struct Registration {
    owner: Owner,
    token: ShortcutToken,
    key: HotKey,
    retiring: bool,
}
#[derive(Default)]
struct Table {
    native_next: u32,
    free: Vec<u32>,
    retired: Vec<u32>,
    source: Source,
    entries: Vec<Registration>,
}

impl Table {
    fn register(
        &mut self,
        backend: &impl Backend,
        owner: Owner,
        accelerator: &str,
    ) -> io::Result<Value> {
        if accelerator.is_empty()
            || accelerator.len() > 256
            || accelerator.chars().any(char::is_control)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid shortcut accelerator",
            ));
        }
        let mut key: HotKey = accelerator
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if self
            .entries
            .iter()
            .any(|entry| entry.key.mods == key.mods && entry.key.key == key.key)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Shortcut already registered",
            ));
        }
        if self.entries.len() >= MAX_REGISTERED {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Global shortcut limit reached",
            ));
        }
        if !self.retired.is_empty() && backend.drain_native() {
            self.free.append(&mut self.retired);
        }
        let next = ShortcutToken::allocate()?.0;
        let id = if let Some(id) = self.free.pop() {
            id
        } else {
            let maximum = if backend.reuse_ids() {
                0xBFFF
            } else {
                u32::MAX
            };
            self.native_next = native_id(self.native_next, maximum)?;
            self.native_next
        };
        key.id = id;
        self.source.insert(id, ShortcutToken(next));
        if let Err(error) = backend.register(key) {
            self.source.remove(id);
            if backend.reuse_ids() {
                self.free.push(id);
            }
            return Err(error);
        }
        self.entries.push(Registration {
            owner,
            token: ShortcutToken(next),
            key,
            retiring: false,
        });
        Ok(Value::from(next))
    }

    fn unregister(
        &mut self,
        backend: &impl Backend,
        owner: Owner,
        token: ShortcutToken,
    ) -> io::Result<Value> {
        let Some(index) = self.entries.iter().position(|entry| entry.token == token) else {
            return Ok(Value::Null);
        };
        if self.entries[index].owner != owner {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Shortcut does not belong to this document",
            ));
        }
        self.entries[index].retiring = true;
        let id = self.entries[index].key.id;
        self.source.remove(id);
        backend.unregister(self.entries[index].key)?;
        if backend.reuse_ids() {
            self.retired.push(id);
            if backend.drain_native() {
                self.free.append(&mut self.retired);
            }
        }
        self.entries.remove(index);
        Ok(Value::Null)
    }

    fn release_window(&mut self, backend: &impl Backend, window: u64) {
        self.sweep(backend, |owner| owner.window != window);
    }

    fn sweep(&mut self, backend: &impl Backend, live: impl Fn(Owner) -> bool) {
        let stale: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.retiring || !live(entry.owner))
            .map(|entry| (entry.owner, entry.token))
            .collect();
        for (owner, token) in stale {
            let _ = self.unregister(backend, owner, token);
        }
    }

    fn route(
        &self,
        event: TaggedEvent,
        live: impl Fn(Owner) -> bool,
    ) -> Option<(Owner, ShortcutToken)> {
        if event.state != HotKeyState::Pressed {
            return None;
        }
        self.entries
            .iter()
            .find(|entry| entry.token == event.token && !entry.retiring && live(entry.owner))
            .map(|entry| (entry.owner, entry.token))
    }
}

/// Whether `owner` is still the document the window shows.
fn live_owner(sessions: &SessionManager, window: u64, owner: SessionId) -> bool {
    sessions
        .current(window)
        .is_some_and(|current| current.id() == owner)
}

#[derive(Default)]
pub(crate) struct Shortcuts {
    backend: Option<GlobalHotKeyManager>,
    table: Table,
    next_sweep: Option<Instant>,
}

impl Shortcuts {
    fn ensure(
        &mut self,
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Wake>,
    ) -> io::Result<()> {
        if super::spike_owns_hotkeys() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The integration spike owns global shortcuts",
            ));
        }
        if !cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Global shortcuts are not supported",
            ));
        }
        #[cfg(target_os = "linux")]
        if !matches!(
            event_loop
                .display_handle()
                .map_err(io::Error::other)?
                .as_raw(),
            RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)
        ) || std::env::var_os("DISPLAY").is_none_or(|value| value.is_empty())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Global shortcuts require X11",
            ));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = event_loop;
        if self.backend.is_none() {
            self.table.source.activate(proxy)?;
            let backend = match GlobalHotKeyManager::new().map_err(native_error) {
                Ok(backend) => backend,
                Err(error) => {
                    self.table.source.deactivate();
                    return Err(error);
                }
            };
            self.backend = Some(backend);
        }
        Ok(())
    }

    pub(crate) fn call(
        &mut self,
        event_loop: &ActiveEventLoop,
        proxy: EventLoopProxy<Wake>,
        sessions: &SessionManager,
        window: u64,
        call: ShortcutCall,
    ) -> io::Result<Value> {
        match call {
            ShortcutCall::Register { owner, accelerator } => {
                if !live_owner(sessions, window, owner) {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Shortcut owner is no longer live",
                    ));
                }
                self.ensure(event_loop, proxy)?;
                let result = self.table.register(
                    self.backend.as_ref().expect("initialized"),
                    Owner {
                        window,
                        session: owner,
                    },
                    &accelerator,
                );
                if result.is_ok() {
                    self.next_sweep
                        .get_or_insert_with(|| Instant::now() + SWEEP);
                }
                result
            }
            ShortcutCall::Unregister { owner, token } => match self.backend.as_ref() {
                Some(backend) => self.table.unregister(
                    backend,
                    Owner {
                        window,
                        session: owner,
                    },
                    token,
                ),
                None => Ok(Value::Null),
            },
        }
    }

    pub(crate) fn tick(&mut self, sessions: &SessionManager, events: &EventBus, windows: &[u64]) {
        let live = |owner: Owner| {
            windows.contains(&owner.window) && live_owner(sessions, owner.window, owner.session)
        };
        if self.sweep_due(Instant::now()) {
            if let Some(backend) = &self.backend {
                self.table.sweep(backend, live);
            }
        }
        for event in self.table.source.drain() {
            if let Some((owner, token)) = self.table.route(event, live) {
                let payload = serde_json::json!({"owner": owner.session.0, "token": token.0});
                if let Ok(json) = event_json(PRESSED, &payload) {
                    events.publish_session(owner.window, owner.session, &json);
                }
            }
        }
    }

    pub(crate) fn release_window(&mut self, window: u64) {
        if let Some(backend) = &self.backend {
            self.table.release_window(backend, window);
        }
    }
    fn sweep_due(&mut self, now: Instant) -> bool {
        if self.table.entries.is_empty() {
            self.next_sweep = None;
            return false;
        }
        let deadline = self.next_sweep.get_or_insert(now + SWEEP);
        if now < *deadline {
            return false;
        }
        *deadline = now + SWEEP;
        true
    }

    pub(crate) fn next_wake(&self) -> Option<Instant> {
        if self.table.entries.is_empty() {
            None
        } else {
            self.next_sweep
        }
    }
    pub(crate) fn shutdown(&mut self) {
        if let Some(backend) = &self.backend {
            self.table.sweep(backend, |_| false);
        }
        self.table.source.deactivate();
        self.backend.take();
        self.table.entries.clear();
        self.next_sweep = None;
    }
}
impl Drop for Shortcuts {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "shortcut_tests.rs"]
mod reuse_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    #[derive(Default)]
    pub(super) struct Fake {
        pub(super) keys: RefCell<Vec<HotKey>>,
        pub(super) conflict: Cell<bool>,
        pub(super) fail_release: Cell<bool>,
        pub(super) barriers: Cell<usize>,
        pub(super) barrier_pending: Cell<bool>,
    }
    impl Backend for Fake {
        fn reuse_ids(&self) -> bool {
            true
        }
        fn drain_native(&self) -> bool {
            self.barriers.set(self.barriers.get() + 1);
            !self.barrier_pending.get()
        }
        fn register(&self, key: HotKey) -> io::Result<()> {
            if self.conflict.get() {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, "OS conflict"));
            }
            self.keys.borrow_mut().push(key);
            Ok(())
        }
        fn unregister(&self, key: HotKey) -> io::Result<()> {
            if self.fail_release.replace(false) {
                return Err(io::Error::other("retry"));
            }
            self.keys.borrow_mut().retain(|k| k.id != key.id);
            Ok(())
        }
    }
    pub(super) fn owner(session: u64) -> Owner {
        Owner {
            window: 1,
            session: SessionId(session),
        }
    }
    pub(super) fn register(
        table: &mut Table,
        backend: &Fake,
        who: Owner,
        key: &str,
    ) -> ShortcutToken {
        ShortcutToken(table.register(backend, who, key).unwrap().as_u64().unwrap())
    }
    #[test]
    fn sweep_deadline_survives_early_ticks_and_advances_only_when_due() {
        let mut shortcuts = Shortcuts::default();
        let backend = Fake::default();
        register(&mut shortcuts.table, &backend, owner(1), "Ctrl+A");
        let now = Instant::now();
        assert!(!shortcuts.sweep_due(now));
        let deadline = now + SWEEP;
        assert_eq!(shortcuts.next_wake(), Some(deadline));
        for offset in [1, 100, 249] {
            assert!(!shortcuts.sweep_due(now + Duration::from_millis(offset)));
            assert_eq!(shortcuts.next_wake(), Some(deadline));
            assert_eq!(shortcuts.next_wake(), Some(deadline));
        }
        assert!(shortcuts.sweep_due(deadline));
        assert_eq!(shortcuts.next_wake(), Some(deadline + SWEEP));
        assert!(!shortcuts.sweep_due(deadline));
        shortcuts.table.sweep(&backend, |_| false);
        assert_eq!(shortcuts.next_wake(), None);
        assert!(!shortcuts.sweep_due(deadline + SWEEP));
        assert_eq!(shortcuts.next_sweep, None);
    }

    #[test]
    fn canonical_duplicates_conflicts_limits_and_monotonic_tokens() {
        let (mut table, backend) = (Table::default(), Fake::default());
        let first = register(&mut table, &backend, owner(1), "Ctrl+Shift+A");
        assert_ne!(first.0, 0);
        assert_eq!(
            table
                .register(&backend, owner(2), "shift+control+KeyA")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        table.unregister(&backend, owner(1), first).unwrap();
        let second = register(&mut table, &backend, owner(2), "Ctrl+Shift+A");
        assert!(second.0 > first.0);
        backend.conflict.set(true);
        assert_eq!(
            table
                .register(&backend, owner(2), "Ctrl+B")
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(table.entries.len(), 1);
        backend.conflict.set(false);
        for _ in 1..MAX_REGISTERED {
            table.entries.push(Registration {
                owner: owner(2),
                token: second,
                key: backend.keys.borrow()[0],
                retiring: false,
            });
        }
        assert_eq!(
            table
                .register(&backend, owner(2), "Ctrl+B")
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn routing_is_pressed_live_owner_only_and_old_tokens_never_reused() {
        let (mut table, backend) = (Table::default(), Fake::default());
        let old = register(&mut table, &backend, owner(1), "Alt+A");
        let pressed = TaggedEvent {
            token: old,
            state: HotKeyState::Pressed,
        };
        assert_eq!(
            table.route(pressed, |who| who == owner(1)),
            Some((owner(1), old))
        );
        assert!(table.route(pressed, |_| false).is_none());
        assert!(table
            .route(
                TaggedEvent {
                    state: HotKeyState::Released,
                    ..pressed
                },
                |_| true
            )
            .is_none());
        table.sweep(&backend, |_| false);
        register(&mut table, &backend, owner(2), "Alt+A");
        assert!(table.route(pressed, |_| true).is_none());
    }
    #[test]
    fn original_owner_can_release_after_reload_and_retry_is_idempotent() {
        let (mut table, backend) = (Table::default(), Fake::default());
        let token = register(&mut table, &backend, owner(1), "Alt+A");
        assert_eq!(
            table
                .unregister(&backend, owner(2), token)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        backend.fail_release.set(true);
        assert!(table.unregister(&backend, owner(1), token).is_err());
        assert!(table
            .route(
                TaggedEvent {
                    token,
                    state: HotKeyState::Pressed
                },
                |_| true
            )
            .is_none());
        assert_eq!(
            table.unregister(&backend, owner(1), token).unwrap(),
            Value::Null
        );
        assert_eq!(
            table.unregister(&backend, owner(1), token).unwrap(),
            Value::Null
        );
        assert!(backend.keys.borrow().is_empty());
    }

    #[test]
    fn the_same_key_with_other_modifiers_is_another_shortcut() {
        let (mut table, backend) = (Table::default(), Fake::default());
        register(&mut table, &backend, owner(1), "Ctrl+A");
        register(&mut table, &backend, owner(1), "Alt+A");
        register(&mut table, &backend, owner(1), "Ctrl+B");
        assert_eq!(table.entries.len(), 3);
    }

    #[test]
    fn a_sweep_retries_a_release_that_failed_even_for_a_live_owner() {
        let (mut table, backend) = (Table::default(), Fake::default());
        let token = register(&mut table, &backend, owner(1), "Alt+A");
        backend.fail_release.set(true);
        assert!(table.unregister(&backend, owner(1), token).is_err());
        assert!(table.entries[0].retiring);
        table.sweep(&backend, |_| true);
        assert!(table.entries.is_empty());
        assert!(backend.keys.borrow().is_empty());
    }

    #[test]
    fn a_window_that_goes_loses_its_shortcuts_and_the_others_keep_theirs() {
        let (mut table, backend) = (Table::default(), Fake::default());
        let other = Owner {
            window: 2,
            session: SessionId(7),
        };
        register(&mut table, &backend, owner(1), "Ctrl+A");
        register(&mut table, &backend, owner(2), "Ctrl+B");
        let kept = register(&mut table, &backend, other, "Ctrl+C");
        table.release_window(&backend, 1);
        assert_eq!(table.entries.len(), 1);
        assert_eq!(table.entries[0].token, kept);
        assert_eq!(backend.keys.borrow().len(), 1);
    }

    #[test]
    fn the_native_error_of_a_taken_key_is_already_exists() {
        use global_hotkey::hotkey::Code;
        let key = HotKey::new(None, Code::KeyA);
        let taken = native_error(global_hotkey::Error::AlreadyRegistered(key));
        assert_eq!(taken.kind(), io::ErrorKind::AlreadyExists);
        let carbon =
            global_hotkey::Error::FailedToRegister("RegisterEventHotKey failed for 1".to_owned());
        assert_eq!(native_error(carbon).kind(), io::ErrorKind::AlreadyExists);
        let other = global_hotkey::Error::FailedToRegister("something else".to_owned());
        assert_eq!(native_error(other).kind(), io::ErrorKind::Other);
        let os = global_hotkey::Error::OsError(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(native_error(os).kind(), io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn an_owner_is_live_only_while_its_document_is_the_one_of_the_window() {
        use alef_core::protocol::call::Limits;
        let source: alef_core::session::TokenSource = std::sync::Arc::new(|| "token".to_owned());
        let sessions = SessionManager::new(source, Limits::default());
        let first = tokio::time::timeout(Duration::from_secs(10), sessions.begin_document(1))
            .await
            .expect("a document begins")
            .id();
        assert!(live_owner(&sessions, 1, first));
        assert!(!live_owner(&sessions, 2, first));
        let second = tokio::time::timeout(Duration::from_secs(10), sessions.begin_document(1))
            .await
            .expect("a document begins again")
            .id();
        assert!(live_owner(&sessions, 1, second));
        assert!(!live_owner(&sessions, 1, first));
    }
}
