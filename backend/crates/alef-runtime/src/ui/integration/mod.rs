// SPDX-License-Identifier: MIT OR Apache-2.0
//! UI-thread integrations. The spike retains its own receiver while explicitly enabled.
pub(crate) mod forward;
pub(crate) mod shortcut;

pub(crate) fn spike_owns_hotkeys() -> bool {
    cfg!(feature = "spike-integration")
        && std::env::var("ALEF_SPIKE_INTEGRATION").is_ok_and(|value| value == "1")
}
