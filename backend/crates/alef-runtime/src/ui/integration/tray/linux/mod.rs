// SPDX-License-Identifier: MIT OR Apache-2.0
//! Linux tray through org.kde.StatusNotifierItem and com.canonical.dbusmenu over D-Bus,
//! without GTK. The winit loop stays synchronous: a dedicated worker thread owns the
//! zbus session connection and serves commands from the UI thread; click and menu
//! events travel back through a `Forwarder` and are drained on `tick`, like the native
//! backend. Without a session bus or an org.kde.StatusNotifierWatcher (CI runners under
//! xvfb included) every call fails with the same NOT_AVAILABLE error the empty backend
//! reports.

mod sni;

use crate::ui::integration::{
    forward::Forwarder,
    tray_pure::{self, Owner, TrayMenu},
};
use crate::{
    bridge::EventBus,
    ui::{event_json, Wake},
};
use alef_core::{
    registry::window::{
        menu::MenuItem,
        tray::{self, TrayCall},
    },
    session::session::SessionManager,
};
use crossbeam_channel::{bounded, Sender};
use serde_json::{json, Value};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use winit::event_loop::EventLoopProxy;

const SWEEP: Duration = Duration::from_millis(250);
const SEND: Duration = Duration::from_secs(5);
const EVENTS: usize = 256;

fn not_available() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Native trays need GTK, which a winit loop does not own",
    )
}

fn live(sessions: &SessionManager, windows: &[u64], owner: Owner) -> bool {
    windows.contains(&owner.window)
        && sessions
            .current(owner.window)
            .is_some_and(|s| s.id() == owner.session)
}

fn identity(call: &TrayCall, window: u64) -> (Owner, &str) {
    let (owner, token) = match call {
        TrayCall::Create { owner, token, .. }
        | TrayCall::SetIcon { owner, token, .. }
        | TrayCall::SetTooltip { owner, token, .. }
        | TrayCall::SetMenu { owner, token, .. }
        | TrayCall::Destroy { owner, token } => (*owner, token.as_str()),
    };
    (
        Owner {
            window,
            session: owner,
        },
        token,
    )
}

fn pixmap(icon: &tray_pure::Icon) -> io::Result<(i32, i32, Vec<u8>)> {
    let width = i32::try_from(icon.width).map_err(io::Error::other)?;
    let height = i32::try_from(icon.height).map_err(io::Error::other)?;
    Ok((width, height, tray_pure::argb32(icon)))
}

/// Events from the worker thread. `Tray` mirrors the native click events; `Menu` is a
/// chosen dbusmenu item carrying the plan id of the actionable item.
pub(crate) enum Event {
    Tray {
        id: String,
        name: &'static str,
        button: &'static str,
    },
    Menu {
        id: String,
        item: i32,
    },
}

enum Command {
    Create {
        path: String,
        id: String,
        pixmap: (i32, i32, Vec<u8>),
        tooltip: String,
        menu: TrayMenu,
    },
    SetIcon {
        path: String,
        pixmap: (i32, i32, Vec<u8>),
    },
    SetTooltip {
        path: String,
        tooltip: String,
    },
    SetMenu {
        path: String,
        menu: TrayMenu,
    },
    Destroy {
        path: String,
    },
}

struct Call {
    command: Command,
    reply: Sender<io::Result<()>>,
}

struct Entry {
    owner: Owner,
    token: String,
    id: String,
    path: String,
    menu: TrayMenu,
}

struct Worker {
    calls: Option<Sender<Call>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.calls.take(); // the worker finishes its loop and drops the connection
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

fn generation() -> io::Result<u64> {
    GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| io::Error::other("Tray generation exhausted"))
}

pub(crate) struct Trays {
    events: Option<Sender<Event>>,
    received: Option<crossbeam_channel::Receiver<Event>>,
    forward: Option<Forwarder<Event>>,
    worker: Option<Worker>,
    failed: Option<Arc<AtomicBool>>,
    entries: Vec<Entry>,
    next_sweep: Option<Instant>,
}

impl Default for Trays {
    fn default() -> Self {
        let (events, received) = bounded(EVENTS);
        Self {
            events: Some(events),
            received: Some(received),
            forward: None,
            worker: None,
            failed: None,
            entries: Vec::new(),
            next_sweep: None,
        }
    }
}

impl Trays {
    pub(crate) fn call(
        &mut self,
        proxy: EventLoopProxy<Wake>,
        sessions: &SessionManager,
        windows: &[u64],
        caller: u64,
        call: TrayCall,
    ) -> io::Result<Value> {
        if crate::ui::integration::spike_owns_hotkeys() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The integration spike owns native trays",
            ));
        }
        call.check()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let (owner, token) = identity(&call, caller);
        let token = token.to_owned();
        if !matches!(call, TrayCall::Destroy { .. }) && !live(sessions, windows, owner) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Tray owner is no longer live",
            ));
        }
        let index = self
            .entries
            .iter()
            .position(|e| e.owner == owner && e.token == token);
        let result = match call {
            TrayCall::Create {
                icon,
                tooltip,
                menu,
                ..
            } => self.create(index.is_some(), owner, token, icon, tooltip, menu),
            TrayCall::SetIcon { icon, .. } => {
                let pixmap = pixmap(&tray_pure::decode(&icon)?)?;
                self.command(index, move |path| Command::SetIcon { path, pixmap })
            }
            TrayCall::SetTooltip { tooltip, .. } => {
                self.command(index, move |path| Command::SetTooltip { path, tooltip })
            }
            TrayCall::SetMenu { menu, .. } => {
                // The plan is built once: a clone goes to the worker, the original
                // replaces the entry's plan once the worker confirms, so `tick`
                // resolves menu clicks with the fresh ids.
                let plan = TrayMenu::build(&menu)?;
                let path = index
                    .and_then(|index| self.entries.get(index))
                    .map(|entry| entry.path.clone());
                let result = self.command(index, {
                    let plan = plan.clone();
                    move |path| Command::SetMenu { path, menu: plan }
                });
                if result.is_ok() {
                    if let Some(entry) = path
                        .as_deref()
                        .and_then(|path| self.entries.iter_mut().find(|entry| entry.path == path))
                    {
                        entry.menu = plan;
                    }
                }
                result
            }
            TrayCall::Destroy { .. } => {
                let result = self.command(index, |path| Command::Destroy { path });
                if result.is_ok() {
                    if let Some(index) = index {
                        self.entries.remove(index);
                    }
                }
                result
            }
        };
        if result.is_ok() {
            if self.forward.is_none() {
                let received = self
                    .received
                    .take()
                    .expect("the forwarder is started only once");
                self.forward = Some(Forwarder::start(received, move || {
                    proxy.send_event(Wake).is_ok()
                })?);
            }
            if !self.entries.is_empty() {
                self.next_sweep
                    .get_or_insert_with(|| Instant::now() + SWEEP);
            }
        }
        result.map(|()| Value::Null)
    }

    fn create(
        &mut self,
        duplicate: bool,
        owner: Owner,
        token: String,
        icon: Vec<u8>,
        tooltip: String,
        menu: Vec<MenuItem>,
    ) -> io::Result<()> {
        let icon = tray_pure::decode(&icon)?;
        if duplicate {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Tray token already exists",
            ));
        }
        if self.entries.iter().filter(|e| e.owner == owner).count() >= tray::SESSION_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Tray limit reached",
            ));
        }
        let plan = TrayMenu::build(&menu)?;
        let generation = generation()?;
        let id = format!("alef-runtime-tray-{}-{generation}", std::process::id());
        // Object path components forbid '-': the id stays a plain name, the path
        // replaces every hyphen with an underscore.
        let path = format!("/StatusNotifierItem/{}", id.replace('-', "_"));
        self.ensure_worker()?;
        self.ask(Call {
            command: Command::Create {
                path: path.clone(),
                id: id.clone(),
                pixmap: pixmap(&icon)?,
                tooltip,
                menu: plan.clone(),
            },
            reply: self.reply(),
        })
        .inspect(|()| {
            self.entries.push(Entry {
                owner,
                token,
                id,
                path,
                menu: plan,
            })
        })
    }

    fn command(
        &mut self,
        index: Option<usize>,
        make: impl FnOnce(String) -> Command,
    ) -> io::Result<()> {
        // Without a bus every call is NOT_AVAILABLE, even for a missing token: NotFound
        // is only meaningful while the tray is actually reachable.
        if self
            .failed
            .as_ref()
            .is_some_and(|failed| failed.load(Ordering::Acquire))
        {
            return Err(not_available());
        }
        let path = index
            .and_then(|index| self.entries.get(index))
            .map(|entry| entry.path.clone())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Tray not found"))?;
        self.ensure_worker()?;
        self.ask(Call {
            command: make(path),
            reply: self.reply(),
        })
    }

    fn reply(&self) -> Sender<io::Result<()>> {
        let (reply, _) = bounded(1);
        reply
    }

    fn ask(&self, mut call: Call) -> io::Result<()> {
        let (reply, answers) = bounded(1);
        call.reply = reply;
        let worker = self.worker.as_ref().expect("the worker is ensured");
        worker
            .calls
            .as_ref()
            .and_then(|calls| calls.send_timeout(call, SEND).ok())
            .ok_or_else(not_available)?;
        answers
            .recv_timeout(SEND)
            .unwrap_or_else(|_| Err(not_available()))
    }

    fn ensure_worker(&mut self) -> io::Result<()> {
        if self
            .failed
            .as_ref()
            .is_some_and(|failed| failed.load(Ordering::Acquire))
        {
            return Err(not_available());
        }
        if self.worker.is_none() {
            let (calls, incoming) = bounded::<Call>(32);
            let events = self
                .events
                .clone()
                .expect("the event channel lives as long as Trays");
            let failed = Arc::new(AtomicBool::new(false));
            let flag = failed.clone();
            let thread = std::thread::Builder::new()
                .name("alef-tray-sni".into())
                .spawn(move || sni::run(incoming, events, flag))?;
            self.worker = Some(Worker {
                calls: Some(calls),
                thread: Some(thread),
            });
            self.failed = Some(failed);
        }
        Ok(())
    }

    pub(crate) fn tick(&mut self, sessions: &SessionManager, events: &EventBus, windows: &[u64]) {
        let now = Instant::now();
        if self.next_sweep.is_some_and(|due| now >= due) {
            // Sweeping cannot fail to the caller (tick has no Result): an entry whose
            // Destroy command does not fit the channel stays in the table and is swept
            // again on the next tick instead of being lost silently.
            let mut alive = Vec::new();
            let mut dead = Vec::new();
            for entry in self.entries.drain(..) {
                if live(sessions, windows, entry.owner) {
                    alive.push(entry);
                } else {
                    dead.push(entry);
                }
            }
            self.entries = alive;
            for entry in dead {
                let sent = self.ensure_worker().is_ok()
                    && self
                        .worker
                        .as_ref()
                        .and_then(|worker| worker.calls.as_ref())
                        .is_some_and(|calls| {
                            calls
                                .try_send(Call {
                                    command: Command::Destroy {
                                        path: entry.path.clone(),
                                    },
                                    reply: self.reply(),
                                })
                                .is_ok()
                        });
                if !sent {
                    self.entries.push(entry);
                }
            }
            self.next_sweep = (!self.entries.is_empty()).then_some(now + SWEEP);
        }
        if let Some(forward) = &self.forward {
            for event in forward.drain() {
                match event {
                    Event::Tray { id, name, button } => {
                        if let Some(entry) = self
                            .entries
                            .iter()
                            .find(|e| e.id == id && live(sessions, windows, e.owner))
                        {
                            publish(
                                events,
                                &entry.owner,
                                name,
                                json!({"owner": entry.owner.session.0, "token": entry.token, "button": button}),
                            );
                        }
                    }
                    Event::Menu { id, item } => {
                        if let Some(entry) = self
                            .entries
                            .iter()
                            .find(|e| e.id == id && live(sessions, windows, e.owner))
                        {
                            if let Some(action) = entry.menu.action(item) {
                                publish(
                                    events,
                                    &entry.owner,
                                    "runtime.tray.menu-click",
                                    json!({"owner": entry.owner.session.0, "id": action, "token": entry.token}),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn release_window(&mut self, window: u64) {
        let mut dead = Vec::new();
        self.entries.retain(|entry| {
            if entry.owner.window == window {
                dead.push(entry.path.clone());
                false
            } else {
                true
            }
        });
        self.destroy(&dead);
    }

    /// The end of the process: no icon stays in the notification area.
    pub(crate) fn shutdown(&mut self) {
        let paths: Vec<String> = self.entries.drain(..).map(|entry| entry.path).collect();
        self.destroy(&paths);
    }

    /// Best-effort destroy used from `release_window` and `shutdown` (both lack a
    /// `Result` in the contract, like the native backend): errors of the try_send are
    /// tolerated here because the process is tearing the window or itself down. The
    /// user-facing `command()` path reports errors instead (see P2-1).
    fn destroy(&mut self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        if self.ensure_worker().is_ok() {
            for path in paths {
                if let Some(worker) = &self.worker {
                    if let Some(calls) = &worker.calls {
                        let _ = calls.try_send(Call {
                            command: Command::Destroy { path: path.clone() },
                            reply: self.reply(),
                        });
                    }
                }
            }
        }
    }

    pub(crate) fn next_wake(&self) -> Option<Instant> {
        if self.entries.is_empty() {
            None
        } else {
            self.next_sweep
        }
    }
}

impl Drop for Trays {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn publish(events: &EventBus, owner: &Owner, name: &str, payload: Value) {
    if let Ok(json) = event_json(name, &payload) {
        events.publish_session(owner.window, owner.session, &json);
    }
}
