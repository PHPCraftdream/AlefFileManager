// SPDX-License-Identifier: MIT OR Apache-2.0
//! Event forwarding: one consumer thread per crate receiver, owned values into the
//! spike channel, timer wakes, and probe injection for the runtime self-check.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use global_hotkey::GlobalHotKeyEvent;
use muda::MenuEvent;
use tray_icon::TrayIconEvent;
use winit::event_loop::EventLoopProxy;

use crate::ui::Wake;

pub(crate) const PROBE_EVENTS: u32 = 4;
const PROBE_DELAY: Duration = Duration::from_millis(300);
const THREAD_IDLE: Duration = Duration::from_millis(100);
const TIMER_TICK: Duration = Duration::from_millis(50);

/// Owned event: the watcher thread is the ONLY consumer of a crate's global receiver
/// (one shared crossbeam queue; handles do not duplicate messages).
#[derive(Debug)]
pub(crate) enum SpikeEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    Hotkey(GlobalHotKeyEvent),
    Probe(u32),
}

/// Blocks on a crate receiver but wakes every `THREAD_IDLE` to honour `stop`, so the
/// thread is joinable on cleanup instead of blocked forever on a static channel.
pub(crate) struct CrateSource<T> {
    receiver: crossbeam_channel::Receiver<T>,
    stop: Arc<AtomicBool>,
}

impl<T> CrateSource<T> {
    pub(crate) fn new(receiver: crossbeam_channel::Receiver<T>, stop: Arc<AtomicBool>) -> Self {
        Self { receiver, stop }
    }
}

impl<T> Iterator for CrateSource<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            match self.receiver.recv_timeout(THREAD_IDLE) {
                Ok(item) => return Some(item),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// Generic one-consumer forwarder: converts each event to an owned value, pushes it
/// into the spike channel and wakes the winit loop; ends when the source ends or
/// `loop_closed` fires. Unit-tested with a fake source.
pub(crate) fn spawn_forwarder<T, I>(
    name: &'static str,
    events: I,
    convert: impl Fn(T) -> SpikeEvent + Send + 'static,
    out: Sender<SpikeEvent>,
    loop_closed: impl Fn() -> bool + Send + 'static,
) -> std::thread::JoinHandle<()>
where
    I: Iterator<Item = T> + Send + 'static,
    T: Send + 'static,
{
    super::spawn_thread(name, move || {
        for event in events {
            if out.send(convert(event)).is_err() || loop_closed() {
                break;
            }
        }
    })
}

fn forward_crate_events<T, I>(
    events: &Sender<SpikeEvent>,
    proxy: &EventLoopProxy<Wake>,
    name: &'static str,
    source: I,
    convert: fn(T) -> SpikeEvent,
) -> std::thread::JoinHandle<()>
where
    I: Iterator<Item = T> + Send + 'static,
    T: Send + 'static,
{
    let out = events.clone();
    let wake = proxy.clone();
    spawn_forwarder(name, source, convert, out, move || {
        wake.send_event(Wake).is_err()
    })
}

pub(crate) struct Watchers {
    pub(crate) stops: Vec<Arc<AtomicBool>>,
    pub(crate) handles: Vec<std::thread::JoinHandle<()>>,
}

/// One forwarder thread per crate receiver; the probe injects owned events into the
/// SAME channel, so it exercises the real channel -> Wake -> drain path.
pub(crate) fn spawn_watchers(
    events: &Sender<SpikeEvent>,
    proxy: &EventLoopProxy<Wake>,
) -> Watchers {
    let mut watchers = Watchers {
        stops: Vec::new(),
        handles: Vec::new(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let source = CrateSource::new(tray_icon::TrayIconEvent::receiver().clone(), stop.clone());
    watchers.handles.push(forward_crate_events(
        events,
        proxy,
        "tray",
        source,
        SpikeEvent::Tray,
    ));
    watchers.stops.push(stop);
    let stop = Arc::new(AtomicBool::new(false));
    let source = CrateSource::new(muda::MenuEvent::receiver().clone(), stop.clone());
    watchers.handles.push(forward_crate_events(
        events,
        proxy,
        "menu",
        source,
        SpikeEvent::Menu,
    ));
    watchers.stops.push(stop);
    let stop = Arc::new(AtomicBool::new(false));
    let source = CrateSource::new(GlobalHotKeyEvent::receiver().clone(), stop.clone());
    watchers.handles.push(forward_crate_events(
        events,
        proxy,
        "hotkey",
        source,
        SpikeEvent::Hotkey,
    ));
    watchers.stops.push(stop);
    watchers
}

/// Injects `Probe(0..count)` into the real spike channel. `ALEF_SPIKE_BREAK=forwarding`
/// sends one event fewer than expected, to prove the verdict really fails.
pub(crate) fn spawn_probe(
    out: Sender<SpikeEvent>,
    proxy: EventLoopProxy<Wake>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let count = if std::env::var("ALEF_SPIKE_BREAK").as_deref() == Ok("forwarding") {
        PROBE_EVENTS - 1
    } else {
        PROBE_EVENTS
    };
    super::spawn_thread("probe", move || {
        for index in 0..count {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(PROBE_DELAY);
            if out.send(SpikeEvent::Probe(index)).is_err() || proxy.send_event(Wake).is_err() {
                break;
            }
        }
    })
}

/// Wakes the loop when scheduled spike work is due; the spike never touches ControlFlow.
pub(crate) fn spawn_timer(
    proxy: EventLoopProxy<Wake>,
    next: Arc<Mutex<Option<Instant>>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    super::spawn_thread("timer", move || loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(TIMER_TICK);
        let due = match next.lock() {
            Ok(guard) => guard.is_some_and(|at| Instant::now() >= at),
            Err(poisoned) => poisoned.into_inner().is_some_and(|at| Instant::now() >= at),
        };
        if due && proxy.send_event(Wake).is_err() {
            break;
        }
    })
}
