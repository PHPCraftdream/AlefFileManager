// SPDX-License-Identifier: MIT OR Apache-2.0
mod bridge;
mod headless;
mod spikes;
mod store;
mod ui;
mod window;

pub use bridge::commands::Commands;
pub use bridge::{
    prepare_assets, AssetInput, Bridge, BridgeOptions, ModuleInstaller, PreparedAssets,
};
pub use headless::run_headless;
pub use store::Store;
// The startup marks live under `window/state` to keep `src/` within its entry limit.
pub mod timing {
    pub use crate::window::timing::{format_mark, mark};
}
pub use timing::mark;
pub use ui::RuntimeHandle;
pub use window::{run, WindowOptions};

/// Delays `runtime.consent.request` in end-to-end mode; see `bridge::e2e`.
#[cfg(feature = "e2e")]
pub use bridge::e2e::e2e_hold_consent_request;
