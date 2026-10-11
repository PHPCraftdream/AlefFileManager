// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::ui::Wake;
use alef_core::registry::window::shortcut::ShortcutToken;
use crossbeam_channel::{bounded, Receiver, Sender};
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex, Once},
};
use winit::event_loop::EventLoopProxy;

const CAPACITY: usize = 256;
static INSTALL: Once = Once::new();
static ACTIVE: Mutex<Option<Source>> = Mutex::new(None);
static NATIVE_NEXT: Mutex<u32> = Mutex::new(0);

/// The next native id: from the counter of the process when ids are never reused, else from `previous`.
pub(super) fn native_id(previous: u32, maximum: u32) -> io::Result<u32> {
    let mut global = NATIVE_NEXT.lock().unwrap();
    let mut local = previous;
    let current = if maximum == u32::MAX {
        &mut *global
    } else {
        &mut local
    };
    *current = current
        .checked_add(1)
        .filter(|id| *id <= maximum)
        .ok_or_else(|| io::Error::other("Shortcut native id space exhausted"))?;
    Ok(*current)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TaggedEvent {
    pub(super) token: ShortcutToken,
    pub(super) state: HotKeyState,
}
struct Sink {
    identities: HashMap<u32, ShortcutToken>,
    send: Sender<TaggedEvent>,
    wake: Option<EventLoopProxy<Wake>>,
}
#[derive(Clone)]
pub(super) struct Source {
    sink: Arc<Mutex<Sink>>,
    events: Receiver<TaggedEvent>,
}
impl Default for Source {
    fn default() -> Self {
        let (send, events) = bounded(CAPACITY);
        Self {
            sink: Arc::new(Mutex::new(Sink {
                identities: HashMap::new(),
                send,
                wake: None,
            })),
            events,
        }
    }
}
impl Source {
    pub(super) fn activate(&self, proxy: EventLoopProxy<Wake>) -> io::Result<()> {
        let mut active = ACTIVE.lock().unwrap();
        if active.is_some() {
            return Err(io::Error::other("Shortcut source already owned"));
        }
        INSTALL.call_once(|| {
            GlobalHotKeyEvent::set_event_handler(Some(|event| {
                if let Some(source) = ACTIVE.lock().unwrap().as_ref() {
                    source.deliver(event);
                }
            }));
        });
        self.sink.lock().unwrap().wake = Some(proxy);
        *active = Some(self.clone());
        Ok(())
    }
    pub(super) fn deactivate(&self) {
        let mut active = ACTIVE.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(&source.sink, &self.sink))
        {
            *active = None;
            self.sink.lock().unwrap().wake = None;
        }
    }
    pub(super) fn insert(&self, id: u32, token: ShortcutToken) {
        self.sink.lock().unwrap().identities.insert(id, token);
    }
    pub(super) fn remove(&self, id: u32) {
        self.sink.lock().unwrap().identities.remove(&id);
    }
    pub(super) fn deliver(&self, event: GlobalHotKeyEvent) {
        if event.state != HotKeyState::Pressed {
            return;
        }
        let sink = self.sink.lock().unwrap();
        if let Some(&token) = sink.identities.get(&event.id) {
            let _ = sink.send.try_send(TaggedEvent {
                token,
                state: event.state,
            });
            if let Some(proxy) = &sink.wake {
                let _ = proxy.send_event(Wake);
            }
        }
    }
    pub(super) fn drain(&self) -> impl Iterator<Item = TaggedEvent> + '_ {
        self.events.try_iter().take(CAPACITY)
    }
}

#[cfg(windows)]
pub(super) fn drain_native() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY,
    };
    // SAFETY: called on the UI thread after unregister, without registering a new id.
    // MSG is initialized; PeekMessage fills it before dispatch. NULL selects this
    // thread's windows, including the pinned backend's private HWND. Other owners
    // are dispatched normally. Detached Released producers are ignored at source.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        for _ in 0..CAPACITY {
            if PeekMessageW(&mut message, 0, WM_HOTKEY, WM_HOTKEY, PM_REMOVE) == 0 {
                return true;
            }
            DispatchMessageW(&message);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pressed(id: u32) -> GlobalHotKeyEvent {
        GlobalHotKeyEvent {
            id,
            state: HotKeyState::Pressed,
        }
    }

    #[test]
    fn source_tags_before_queueing_and_drain_preserves_other_owners() {
        let source = Source::default();
        source.insert(1, ShortcutToken(10));
        source.insert(2, ShortcutToken(20));
        source.deliver(pressed(1));
        source.remove(1);
        source.deliver(pressed(1));
        source.deliver(pressed(2));
        source.insert(1, ShortcutToken(30));
        source.deliver(pressed(1));
        assert_eq!(
            source
                .drain()
                .map(|event| event.token.0)
                .collect::<Vec<_>>(),
            [10, 20, 30]
        );
    }

    #[test]
    fn bounded_queue_drops_newest_without_retagging_old_generation() {
        let source = Source::default();
        source.insert(1, ShortcutToken(10));
        for _ in 0..CAPACITY + 1 {
            source.deliver(pressed(1));
        }
        source.remove(1);
        source.insert(1, ShortcutToken(20));
        let events: Vec<_> = source.drain().collect();
        assert_eq!(events.len(), CAPACITY);
        assert!(events.iter().all(|event| event.token == ShortcutToken(10)));
        source.deliver(pressed(1));
        assert_eq!(source.drain().next().unwrap().token, ShortcutToken(20));
    }
}
