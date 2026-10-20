// SPDX-License-Identifier: MIT OR Apache-2.0
//! Platform-backed system power event source.
use super::power::{PowerBackend, PowerSink, PowerWatch};
use alef_core::AlefError;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
use alef_core::ErrorCode;
#[cfg(target_os = "linux")]
#[path = "../linux.rs"]
mod linux;
#[cfg(target_os = "macos")]
#[path = "../macos.rs"]
mod macos;
#[cfg(windows)]
#[path = "../windows.rs"]
mod windows;
#[derive(Debug, Default)]
pub struct SystemPower;
impl PowerBackend for SystemPower {
    fn watch(&self, sink: PowerSink) -> Result<PowerWatch, AlefError> {
        #[cfg(windows)]
        {
            windows::WindowsPower.watch(sink)
        }
        #[cfg(target_os = "macos")]
        {
            macos::MacOsPower.watch(sink)
        }
        #[cfg(target_os = "linux")]
        {
            linux::LinuxPower.watch(sink)
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = sink;
            Err(AlefError::new(
                ErrorCode::NotAvailable,
                "power notifications unsupported on this platform",
            ))
        }
    }
}
