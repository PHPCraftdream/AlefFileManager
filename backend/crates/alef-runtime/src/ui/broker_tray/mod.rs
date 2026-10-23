// SPDX-License-Identifier: MIT OR Apache-2.0
//! The broker tray: an independent notification-area icon for the windowless
//! `alef broker` process (no Servo, no document `Owner`, no session). The
//! document-bound tray integration under `ui/integration/tray/` stays untouched.
//!
//! API: `BrokerTray::new(config)` decodes the PNG and validates it, `handle()`
//! gives a `Clone + Send` handle whose `exit()` stops the tray from another
//! thread, and `run(self)` blocks the calling thread until `exit()` or failure.
//! The two fixed menu items surface as `BrokerTrayCommand`s on
//! `BrokerTrayConfig::commands` (a crossbeam sender, like the existing tray
//! worker). Without a tray backend (`run` on Linux without a session bus or a
//! StatusNotifierWatcher host) the call fails immediately with
//! `io::ErrorKind::Unsupported`; it never hangs.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(windows, target_os = "macos"))]
mod native;
mod pure;

use crate::ui::integration::tray_pure::{self, Icon};
use crossbeam_channel::Sender;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

/// A command chosen in the broker tray menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrokerTrayCommand {
    /// «Список приложений»: the broker opens its trusted application list.
    ShowApps,
    /// «Выход»: the broker stops; running applications keep working.
    Quit,
}

/// Everything the platform backend needs; built once in `BrokerTray::new`.
pub(crate) struct Prepared {
    icon: Icon,
    tooltip: String,
    show_apps_label: String,
    quit_label: String,
    commands: Sender<BrokerTrayCommand>,
}

/// Configuration of the broker tray icon. The defaults carry the Russian menu
/// labels required by the M11 document; `commands` in the default config is
/// disconnected, so unmapped tray clicks are simply dropped.
pub struct BrokerTrayConfig {
    /// PNG bytes of the icon (1..256 KiB, sides 1..256, as `tray_pure::decode`).
    pub icon_png: Vec<u8>,
    pub tooltip: String,
    pub show_apps_label: String,
    pub quit_label: String,
    /// Where chosen menu items are delivered. Send failures are ignored.
    pub commands: Sender<BrokerTrayCommand>,
}

impl Default for BrokerTrayConfig {
    fn default() -> Self {
        // The default sender is disconnected: sends are ignored by design.
        let (commands, _receiver) = crossbeam_channel::bounded(64);
        Self {
            icon_png: Vec::new(),
            tooltip: pure::DEFAULT_TOOLTIP.to_owned(),
            show_apps_label: pure::DEFAULT_SHOW_APPS.to_owned(),
            quit_label: pure::DEFAULT_QUIT.to_owned(),
            commands,
        }
    }
}

/// Shared stop state: an atomic flag plus a platform-armed wakeup. `exit()` from
/// any thread unblocks the loop of `run` without waiting for a poll tick.
#[derive(Default)]
pub(crate) struct Shutdown {
    requested: AtomicBool,
    notify: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Shutdown {
    /// Installs the platform wakeup; called once from the loop that will wait.
    pub(crate) fn arm(&self, notify: Box<dyn Fn() + Send + Sync>) {
        *self.notify.lock().expect("shutdown notify lock") = Some(notify);
    }

    pub(crate) fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::Release);
        if let Some(notify) = self.notify.lock().expect("shutdown notify lock").as_ref() {
            notify();
        }
    }
}

/// A `Clone + Send` stop handle for a running broker tray.
#[derive(Clone)]
pub struct BrokerTrayHandle(Arc<Shutdown>);

impl BrokerTrayHandle {
    /// Stops the tray. Idempotent; safe from any thread.
    pub fn exit(&self) {
        self.0.request();
    }
}

/// The broker tray. Created and driven from one thread (`run` blocks it);
/// stopped through the handle from any thread.
pub struct BrokerTray {
    prepared: Prepared,
    shutdown: Arc<Shutdown>,
}

impl BrokerTray {
    /// Decodes and validates the icon; an invalid PNG is `InvalidInput` here, so
    /// `run` can fail only with platform (un)availability.
    pub fn new(config: BrokerTrayConfig) -> io::Result<Self> {
        let icon = tray_pure::decode(&config.icon_png)?;
        Ok(Self {
            prepared: Prepared {
                icon,
                tooltip: config.tooltip,
                show_apps_label: config.show_apps_label,
                quit_label: config.quit_label,
                commands: config.commands,
            },
            shutdown: Arc::new(Shutdown::default()),
        })
    }

    /// The stop handle; call before `run` (it consumes `self`).
    pub fn handle(&self) -> BrokerTrayHandle {
        BrokerTrayHandle(self.shutdown.clone())
    }

    /// Blocks the calling thread until `exit()` or a platform failure. Without a
    /// tray backend on this system it returns `Unsupported` immediately.
    pub fn run(self) -> io::Result<()> {
        #[cfg(any(windows, target_os = "macos"))]
        return native::run(self.prepared, self.shutdown);
        #[cfg(target_os = "linux")]
        return linux::run(self.prepared, self.shutdown);
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = self.prepared;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Broker tray is not implemented on this platform",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn defaults_carry_russian_labels_and_disconnected_sender() {
        let config = BrokerTrayConfig::default();
        assert!(config.icon_png.is_empty());
        assert_eq!(config.tooltip, "Alef Broker");
        assert_eq!(config.show_apps_label, "Список приложений");
        assert_eq!(config.quit_label, "Выход");
        // Disconnected on purpose: the send below must not block or panic.
        assert!(config.commands.send(BrokerTrayCommand::Quit).is_err());
    }

    #[test]
    fn invalid_png_is_invalid_input() {
        let mut config = BrokerTrayConfig::default();
        config.icon_png = vec![0; 32];
        let error = BrokerTray::new(config).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn valid_png_is_accepted_and_handle_exits() {
        let mut config = BrokerTrayConfig::default();
        config.icon_png = png();
        let tray = BrokerTray::new(config).unwrap();
        let handle = tray.handle();
        assert!(!tray.shutdown.requested());
        handle.exit();
        assert!(tray.shutdown.requested());
        // Exit is idempotent and armed with no wakeup installed.
        handle.exit();
    }
}
