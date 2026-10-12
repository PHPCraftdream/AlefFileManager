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
#[cfg(feature = "spike-origin")]
pub(crate) mod origin;

#[cfg(not(feature = "spike-origin"))]
pub(crate) mod origin {
    pub(crate) fn enabled() -> bool {
        false
    }

    pub(crate) fn entry_url() -> Option<url::Url> {
        None
    }

    pub(crate) fn preferences() -> servo::Preferences {
        servo::Preferences::default()
    }

    pub(crate) fn set_assets(_: &std::path::Path) {}

    /// Dropping the load keeps Servo's normal, nonintercepted fetch path.
    pub(crate) fn web_resource(_: servo::WebResourceLoad) {}

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn feature_off_keeps_the_normal_origin_and_preferences() {
            assert!(!enabled());
            assert!(entry_url().is_none());
            assert_eq!(
                preferences().dom_indexeddb_enabled,
                servo::Preferences::default().dom_indexeddb_enabled
            );
        }
    }
}
