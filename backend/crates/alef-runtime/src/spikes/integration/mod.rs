// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(feature = "spike-integration")]
//! M0.3: tray, menu, global hotkey and dialogs with the winit loop (docs/stages/m0-spikes.md).
//! Modes: ALEF_SPIKE_INTEGRATION=1 (full or +NO_DIALOG=1), ALEF_SPIKE_RESIZE_ONLY=1 (baseline).
mod dialog;
mod forward;
mod objects;
#[cfg(test)]
mod tests;
mod verdict;

use std::cell::RefCell;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;
#[cfg(target_os = "windows")]
use muda::ContextMenu;
use muda::{Menu, PredefinedMenuItem};
use tray_icon::TrayIcon;
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::HasWindowHandle;
use winit::window::Window;

use crate::ui::Wake;
use forward::SpikeEvent;
use verdict::Verdict;

type IntegrationObjects = (
    Option<(Menu, isize)>,
    Option<TrayIcon>,
    Option<(GlobalHotKeyManager, HotKey)>,
);

const CYCLE_START_DELAY: Duration = Duration::from_millis(1200);
const CYCLE_STEP_DELAY: Duration = Duration::from_millis(350);
const CYCLE_STEPS: u32 = 8;
const CYCLE_B_DELAY: Duration = Duration::from_millis(500);
const LOG_PREFIX: &str = "spike-integration:";

pub(crate) struct Cycle {
    pub(crate) next_at: Instant,
    step: u32,
    grow: bool,
}

impl Cycle {
    fn new(start: Instant) -> Self {
        Self {
            next_at: start,
            step: 0,
            grow: true,
        }
    }
    fn size(&self) -> winit::dpi::Size {
        let (width, height) = if self.grow { (1327, 900) } else { (980, 640) };
        winit::dpi::LogicalSize::new(width, height).into()
    }
}

pub(crate) struct Spike {
    pub(crate) started: Instant,
    pub(crate) events: Receiver<SpikeEvent>,
    pub(crate) timer_next: Arc<Mutex<Option<Instant>>>,
    pub(crate) menu: Option<(Menu, isize)>,
    pub(crate) tray: Option<TrayIcon>,
    pub(crate) hotkey_ctrl: Option<(GlobalHotKeyManager, HotKey)>,
    pub(crate) dialog: dialog::Dialog,
    pub(crate) cycle: Option<Cycle>,
    pub(crate) cycle_b: Option<Cycle>,
    pub(crate) verdict: Verdict,
    pub(crate) stops: Vec<Arc<AtomicBool>>,
    pub(crate) handles: Vec<JoinHandle<()>>,
    pub(crate) summary_done: bool,
}

fn integration() -> bool {
    std::env::var("ALEF_SPIKE_INTEGRATION").is_ok_and(|value| value == "1")
}

fn resize_only() -> bool {
    std::env::var("ALEF_SPIKE_RESIZE_ONLY").is_ok_and(|value| value == "1")
}

fn active() -> bool {
    integration() || resize_only()
}

fn log(message: &str) {
    eprintln!("{LOG_PREFIX} {message}");
}

thread_local! {
    static SPIKE: RefCell<Option<Spike>> = const { RefCell::new(None) };
}

fn with_spike<R>(run: impl FnOnce(&mut Spike) -> R) -> Option<R> {
    SPIKE.with(|spike| spike.borrow_mut().as_mut().map(run))
}

pub(crate) fn spawn_thread(
    name: &'static str,
    run: impl FnOnce() + Send + 'static,
) -> JoinHandle<()> {
    match std::thread::Builder::new()
        .name(format!("alef-spike-{name}"))
        .spawn(run)
    {
        Ok(handle) => handle,
        Err(error) => {
            log(&format!("thread {name} failed to spawn: {error}"));
            std::thread::spawn(|| {})
        }
    }
}

/// Counts every presented frame and every resize timeout (hooked from `State::trace_present`).
pub(crate) fn note_present(timed_out: bool) {
    if !active() {
        return;
    }
    with_spike(|spike| {
        let during = spike.dialog.enabled
            && spike.dialog.found_at.is_some()
            && spike.dialog.gone_at.is_none();
        spike.verdict.note_present(timed_out, during);
    });
}

/// Called on the main thread right after the window exists.
pub(crate) fn activate(window: &Window, proxy: EventLoopProxy<Wake>) {
    if !active() || with_spike(|_| ()).is_some() {
        return;
    }
    let full = integration() && !dialog_skipped();
    log(&format!(
        "activate pid={} mode={}",
        std::process::id(),
        if integration() {
            if full {
                "integration (full)"
            } else {
                "integration (no dialog)"
            }
        } else {
            "resize-only"
        }
    ));
    let started = Instant::now();
    let (events_tx, events_rx) = channel::<SpikeEvent>();
    let timer_next = Arc::new(Mutex::new(None));
    let mut verdict = Verdict::default();
    let mut stops: Vec<Arc<AtomicBool>> = Vec::new();
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    let (menu, tray, hotkey_ctrl) = integration_objects(
        window,
        &proxy,
        &events_tx,
        &mut verdict,
        &mut stops,
        &mut handles,
    );
    let timer_stop = Arc::new(AtomicBool::new(false));
    handles.push(forward::spawn_timer(
        proxy.clone(),
        timer_next.clone(),
        timer_stop.clone(),
    ));
    stops.push(timer_stop);
    if full {
        dialog::open_dialog_async(window);
    }
    let cycle = Cycle::new(started + CYCLE_START_DELAY);
    let spike = Spike {
        started,
        events: events_rx,
        timer_next,
        menu,
        tray,
        hotkey_ctrl,
        dialog: dialog::Dialog::new(full, started),
        cycle: Some(cycle),
        cycle_b: None,
        verdict,
        stops,
        handles,
        summary_done: false,
    };
    SPIKE.with(|cell| *cell.borrow_mut() = Some(spike));
    log("scheduled: resize cycle A at +1200ms (timer wakes the loop; no ControlFlow changes)");
}

fn integration_objects(
    window: &Window,
    proxy: &EventLoopProxy<Wake>,
    events_tx: &std::sync::mpsc::Sender<SpikeEvent>,
    verdict: &mut Verdict,
    stops: &mut Vec<Arc<AtomicBool>>,
    handles: &mut Vec<JoinHandle<()>>,
) -> IntegrationObjects {
    if !integration() {
        log("integration objects not created (resize-only mode)");
        return (None, None, None);
    }
    let attached = objects::build_menu(window);
    verdict.created_menu = Some(attached.is_ok());
    let menu = attached.as_ref().map_or_else(
        |error| {
            log(&format!("menu init failed: {error}"));
            fallback_menu()
        },
        |(menu, _)| menu.clone(),
    );
    let tray = objects::build_tray(menu).map_or_else(
        |error| {
            log(&format!("tray init failed: {error}"));
            None
        },
        Some,
    );
    verdict.created_tray = Some(tray.is_some());
    let hotkey = HotKey::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::F20,
    );
    let hotkey_ctrl = match GlobalHotKeyManager::new() {
        Ok(manager) => match manager.register(hotkey) {
            Ok(()) => {
                log("hotkey registered: Ctrl+Alt+Shift+F20");
                Some((manager, hotkey))
            }
            Err(error) => {
                log(&format!("hotkey registration failed: {error}"));
                None
            }
        },
        Err(error) => {
            log(&format!("hotkey manager failed: {error}"));
            None
        }
    };
    verdict.created_hotkey = Some(hotkey_ctrl.is_some());
    let watchers = forward::spawn_watchers(events_tx, proxy);
    stops.extend(watchers.stops);
    handles.extend(watchers.handles);
    let probe_stop = Arc::new(AtomicBool::new(false));
    handles.push(forward::spawn_probe(
        events_tx.clone(),
        proxy.clone(),
        probe_stop.clone(),
    ));
    stops.push(probe_stop);
    (attached.ok(), tray, hotkey_ctrl)
}
fn dialog_skipped() -> bool {
    std::env::var("ALEF_SPIKE_INTEGRATION_NO_DIALOG").is_ok_and(|value| value == "1")
}

fn fallback_menu() -> Menu {
    let open = muda::MenuItem::with_id("integration-open-dialog", "Spike: open dialog", true, None);
    let quit = muda::MenuItem::with_id("integration-quit", "Spike: quit", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]);
    menu
}

#[allow(dead_code)]
fn build_menu(window: &Window) -> Result<(Menu, isize), Box<dyn std::error::Error>> {
    let menu = fallback_menu();
    #[cfg(target_os = "windows")]
    {
        let raw = window
            .window_handle()
            .map_err(|error| error.to_string())?
            .as_raw();
        let hwnd = match raw {
            winit::raw_window_handle::RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            other => return Err(format!("unsupported raw window handle {other:?}").into()),
        };
        unsafe { menu.init_for_hwnd(hwnd)? };
        let popup = menu.hpopupmenu();
        log(&format!(
            "menu attached to the window (init_for_hwnd), hpopupmenu={popup:#x}"
        ));
        Ok((menu, hwnd))
    }
    #[cfg(not(target_os = "windows"))]
    {
        log("menu init_for_hwnd skipped: not on Windows");
        Ok((menu, 0))
    }
}

/// Called on every loop wake: drains OUR channel (never the crate receivers) and
/// advances the scheduled work.
pub(crate) fn poll(window: &Window) {
    if !active() {
        return;
    }
    let now = Instant::now();
    with_spike(|spike| {
        drain_events(spike);
        dialog::poll_dialog(&mut spike.dialog, &mut spike.cycle_b, spike.started, now);
        if advance_cycle(window, &mut spike.cycle, now, "A", &mut spike.verdict)
            && !spike.dialog.enabled
        {
            spike.cycle_b = Some(Cycle::new(now + CYCLE_B_DELAY));
        }
        advance_cycle(window, &mut spike.cycle_b, now, "B", &mut spike.verdict);
        let next = [spike.cycle.as_ref(), spike.cycle_b.as_ref()]
            .into_iter()
            .flatten()
            .map(|cycle| cycle.next_at)
            .chain(dialog::deadline(&spike.dialog))
            .filter(|at| *at > now)
            .min();
        if let Ok(mut guard) = spike.timer_next.lock() {
            *guard = next;
        }
        if spike.cycle.is_none() && spike.cycle_b.is_none() && !spike.summary_done {
            spike.summary_done = true;
            log(&spike.verdict.summary(spike.started, now));
        }
    });
}

fn drain_events(spike: &mut Spike) {
    while let Ok(event) = spike.events.try_recv() {
        match event {
            SpikeEvent::Tray(event) => {
                log(&format!("tray event {event:?}"));
                spike.verdict.tray_events += 1;
            }
            SpikeEvent::Menu(event) => {
                log(&format!("menu event {}", event.id.0));
                spike.verdict.menu_events += 1;
            }
            SpikeEvent::Hotkey(event) => {
                log(&format!(
                    "hotkey event id={} state={:?}",
                    event.id, event.state
                ));
                spike.verdict.hotkey_events += 1;
            }
            SpikeEvent::Probe(index) => {
                log(&format!("probe {index} arrived via the forwarding channel"));
                spike.verdict.observe_probe(index);
            }
        }
    }
}

/// Returns true when the cycle just finished.
fn advance_cycle(
    window: &Window,
    cycle: &mut Option<Cycle>,
    now: Instant,
    tag: &str,
    verdict: &mut Verdict,
) -> bool {
    let Some(state) = cycle else {
        return false;
    };
    if now < state.next_at {
        return false;
    }
    let synchronous = window.request_inner_size(state.size()).is_some();
    log(&format!(
        "cycle {tag} step {} requested (synchronous={synchronous})",
        state.step
    ));
    verdict.resize_requested += 1;
    state.grow = !state.grow;
    state.step += 1;
    if state.step >= CYCLE_STEPS {
        log(&format!("cycle {tag} done: {CYCLE_STEPS} requested"));
        *cycle = None;
        true
    } else {
        state.next_at = now + CYCLE_STEP_DELAY;
        false
    }
}

/// Runs BEFORE the window/State is dropped (caller order in `exiting`): muda's
/// `remove_for_hwnd` needs a valid HWND, so cleanup must precede window destruction.
/// Records cleanup results into the verdict, stops and joins the spike threads, and
/// returns an error (non-zero process exit) when any check failed.
pub(crate) fn deactivate() -> Option<io::Error> {
    if !active() {
        return None;
    }
    let mut spike = SPIKE.with(|cell| cell.borrow_mut().take())?;
    let integration_mode = integration();
    if integration_mode {
        if let Some(tray) = spike.tray.as_ref() {
            spike.verdict.cleanup.tray = Some(tray.set_visible(false).is_ok());
        }
    }
    drop(spike.tray);
    log("tray icon removed");
    if let Some((menu, hwnd)) = spike.menu {
        #[cfg(target_os = "windows")]
        {
            let result = unsafe { menu.remove_for_hwnd(hwnd) };
            spike.verdict.cleanup.menu = Some(result.is_ok());
            log(&format!("menu detach: {result:?}"));
        }
        #[cfg(not(target_os = "windows"))]
        let _ = (menu, hwnd);
    }
    if let Some((manager, hotkey)) = spike.hotkey_ctrl {
        let result = manager.unregister(hotkey);
        spike.verdict.cleanup.hotkey = Some(result.is_ok());
        log(&format!("hotkey unregister: {result:?}"));
    }
    for stop in &spike.stops {
        stop.store(true, Ordering::Relaxed);
    }
    for handle in spike.handles.drain(..) {
        let _ = handle.join();
    }
    log("spike threads stopped and joined");
    let (json, failed) = spike
        .verdict
        .finalize(integration_mode, spike.dialog.enabled);
    log(&format!("verdict: {json}"));
    if let Ok(path) = std::env::var("ALEF_SPIKE_VERDICT_FILE") {
        let _ = std::fs::write(path, &json);
    }
    (!failed.is_empty()).then(|| {
        io::Error::other(format!(
            "spike-integration checks failed: {}",
            failed.join(", ")
        ))
    })
}
