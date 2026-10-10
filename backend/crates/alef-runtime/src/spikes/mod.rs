// SPDX-License-Identifier: MIT OR Apache-2.0
// Throwaway M0 spikes (docs/stages/m0-spikes.md); each lives in its own file.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) mod integration;
#[cfg(all(feature = "spike-integration", not(any(windows, target_os = "macos"))))]
pub(crate) mod integration {
    pub(crate) fn activate(
        _: &winit::window::Window,
        _: winit::event_loop::EventLoopProxy<crate::ui::Wake>,
    ) {
    }
    pub(crate) fn poll(_: &winit::window::Window) {}
    pub(crate) fn note_present(_: bool) {}
    pub(crate) fn deactivate() -> Option<std::io::Error> {
        None
    }
}
pub(crate) mod origin;
