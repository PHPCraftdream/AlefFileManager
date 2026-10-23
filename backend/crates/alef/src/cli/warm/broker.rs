// SPDX-License-Identifier: MIT OR Apache-2.0
//! `alef broker` (docs/stages/m11-warm-start.md, «Архитектура реализации»):
//! the constant per-user process. The main thread owns the tray (or waits
//! for the stop signal without one); a worker thread drives the [`Broker`]
//! pump loop and the shutdown, and the IPC server runs on its own thread.
//! One broker per session: a second start reports "already running" and
//! exits successfully.

use std::io;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use alef_broker::broker::{Broker, ProcessSpawner};
use alef_broker::ipc::server::{ExpectedIdentity, Handler, Server};
use alef_broker::ipc::token::load_or_create_token;
use alef_broker::ipc::ReplyTo;
use alef_broker::pool::{Jitter, PoolConfig, SystemClock};
use alef_broker::protocol::message::{ClientMsg, OsBytes};
use alef_broker::protocol::warm::{decode_warm_admin_msg, WarmAdminMsg};
use alef_runtime::{BrokerTray, BrokerTrayCommand, BrokerTrayConfig};

use super::control::spawn_detached;
use super::endpoint::endpoint;
use crate::cli::failure::Failure;

/// The pump loop sleeps on the pool's wake; the channel is the real wakeup
/// for shutdown, this is only a backstop.
const NO_WAKE: Duration = Duration::from_secs(3600);

type AppBroker = Broker<ProcessSpawner, SystemClock, NoJitter>;

/// Deterministic jitter: the wiring provides one; none is fine.
struct NoJitter;
impl Jitter for NoJitter {
    fn fraction(&mut self) -> f64 {
        0.0
    }
}

/// Admin shutdown arrives through `on_raw_frame`; the broker's own handler
/// answers it, this wrapper only tells the pump loop to finish the process.
struct BrokerHandler {
    broker: Arc<AppBroker>,
    stop: Sender<()>,
}

impl Handler for BrokerHandler {
    fn on_message(&self, conn: &mut dyn ReplyTo, msg: ClientMsg) {
        self.broker.on_message(conn, msg)
    }

    fn on_raw_frame(&self, conn: &mut dyn ReplyTo, conn_id: u64, body: &[u8]) -> io::Result<()> {
        if matches!(
            decode_warm_admin_msg(body).as_ref(),
            Ok(WarmAdminMsg::Shutdown(_))
        ) {
            let _ = self.stop.send(());
        }
        self.broker.on_raw_frame(conn, conn_id, body)
    }
}

/// Runs the broker; blocks the caller until the tray quits or an admin
/// shutdown arrives. The socket file is removed when the server is dropped.
pub(crate) fn run_broker(no_tray: bool) -> Result<u8, Failure> {
    let exe = std::env::current_exe()
        .map_err(|e| Failure::Runtime(format!("current executable: {e}")))?;
    let Some(ep) = endpoint() else {
        return Err(Failure::Runtime(
            "WARM_UNAVAILABLE: the broker entry point does not fit the system limits; \
             warm start is unavailable on this system"
                .to_owned(),
        ));
    };
    // One broker per session: an answer means one is already there.
    if super::control::broker_is_running() {
        println!("already running");
        return Ok(0);
    }
    let (token, _) =
        load_or_create_token(&ep).map_err(|e| Failure::Runtime(format!("broker token: {e}")))?;
    let server = Arc::new(
        Server::bind(&ep).map_err(|e| Failure::Runtime(format!("WARM_BIND_FAILED: {e}")))?,
    );
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let identity = ExpectedIdentity {
        build: version.clone(),
        alef_home: std::env::var_os("ALEF_HOME").map(|v| OsBytes::from_os_string(&v)),
        alef_app_home: std::env::var_os("ALEF_APP_HOME").map(|v| OsBytes::from_os_string(&v)),
    };
    let broker: Arc<AppBroker> = Arc::new(Broker::new(
        // The windowless slot is off in this version: the measured gain was
        // ≈ 5–16 ms (docs, «Замер холодного запуска»).
        PoolConfig {
            level: alef_broker::pool::Level::Full,
            windowless: false,
            ..PoolConfig::default()
        },
        ProcessSpawner::new(exe.clone(), Vec::new(), ep),
        SystemClock::new(),
        NoJitter,
        version,
    ));
    let (stop_tx, stop_rx) = channel::<()>();
    let (done_tx, done_rx) = channel::<()>();

    // The IPC server: one endpoint serves clients, spares and the admin.
    let server_thread = {
        let server = Arc::clone(&server);
        let token = Arc::new(token);
        let handler = Arc::new(BrokerHandler {
            broker: Arc::clone(&broker),
            stop: stop_tx.clone(),
        });
        std::thread::Builder::new()
            .name("broker-ipc".into())
            .spawn(move || server.run(token, Some(identity), handler))
            .map_err(|e| Failure::Runtime(format!("broker thread: {e}")))?
    };

    // The tray is created on the main thread; the pump thread exits it.
    let tray = if no_tray {
        None
    } else {
        let (commands, tray_rx) = crossbeam_channel::bounded::<BrokerTrayCommand>(16);
        let config = BrokerTrayConfig {
            icon_png: crate::cli::DEFAULT_ICON.to_vec(),
            commands,
            ..BrokerTrayConfig::default()
        };
        match BrokerTray::new(config) {
            Ok(tray) => {
                let handle = tray.handle();
                // The tray commands: «Список приложений» and «Выход».
                let dispatcher_exe = exe.clone();
                let dispatcher_stop = stop_tx.clone();
                let _ = std::thread::Builder::new()
                    .name("broker-tray".into())
                    .spawn(move || {
                        while let Ok(command) = tray_rx.recv() {
                            match command {
                                BrokerTrayCommand::ShowApps => {
                                    // The broker's own trusted list: argv array,
                                    // no shell, detached from this process.
                                    let _ = spawn_detached(&dispatcher_exe, &["apps", "--ui"]);
                                }
                                BrokerTrayCommand::Quit => {
                                    let _ = dispatcher_stop.send(());
                                    break;
                                }
                            }
                        }
                    });
                Some((tray, handle))
            }
            Err(error) => {
                eprintln!("alef: the tray is unavailable ({error}); waiting for `alef warm stop`");
                None
            }
        }
    };

    // The pump loop: pool wakeups only, no timers of its own. After the
    // stop: the pool shutdown (running applications are untouched), then
    // the server, and the tray goes away with it.
    let worker = {
        let broker = Arc::clone(&broker);
        let server = Arc::clone(&server);
        let tray_handle = tray.as_ref().map(|(_, handle)| handle.clone());
        std::thread::Builder::new()
            .name("broker-pump".into())
            .spawn(move || {
                loop {
                    let wake = broker.pump();
                    match stop_rx.recv_timeout(wake.unwrap_or(NO_WAKE)) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
                broker.shutdown();
                server.stop();
                if let Some(handle) = tray_handle {
                    handle.exit();
                }
                let _ = done_tx.send(());
            })
            .map_err(|e| Failure::Runtime(format!("broker thread: {e}")))?
    };

    // The main thread: the tray blocks here, or the process waits quietly.
    match tray {
        Some((tray, _)) => match tray.run() {
            // A quit through the handle: go on to the shutdown below.
            Ok(()) => {}
            Err(error) => {
                eprintln!("alef: the tray is unavailable ({error}); waiting for `alef warm stop`");
                let _ = done_rx.recv();
            }
        },
        None => {
            if no_tray {
                eprintln!(
                    "alef: the tray is disabled with --no-tray; waiting for `alef warm stop`"
                );
            }
            let _ = done_rx.recv();
        }
    }
    let _ = stop_tx.send(());
    let _ = worker.join();
    let _ = server_thread.join();
    drop(server);
    Ok(0)
}
