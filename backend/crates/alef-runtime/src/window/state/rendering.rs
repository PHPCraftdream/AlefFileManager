// SPDX-License-Identifier: MPL-2.0
//! Rendering for native windows, including hidden end-to-end windows.
use std::io;
use std::rc::Rc;

use servo::{RenderingContext, WindowRenderingContext};
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

#[cfg(feature = "e2e")]
fn software_enabled(quiet: bool) -> bool {
    quiet
        && super::super::software_from(
            &|name| std::env::var(name).ok(),
            cfg!(target_os = "windows"),
        )
}

#[cfg(feature = "e2e")]
fn dispatch<T>(
    software: bool,
    size: winit::dpi::PhysicalSize<u32>,
    software_factory: impl FnOnce(winit::dpi::PhysicalSize<u32>) -> io::Result<T>,
    native_factory: impl FnOnce(winit::dpi::PhysicalSize<u32>) -> io::Result<T>,
) -> io::Result<T> {
    if software {
        software_factory(size)
    } else {
        native_factory(size)
    }
}

pub(in crate::window) fn new(
    event_loop: &ActiveEventLoop,
    window: &Window,
    _quiet: bool,
) -> io::Result<Rc<dyn RenderingContext>> {
    let size = window.inner_size();
    let native = |size| -> io::Result<Rc<dyn RenderingContext>> {
        Ok(Rc::new(
            WindowRenderingContext::new(
                event_loop.display_handle().map_err(io::Error::other)?,
                window.window_handle().map_err(io::Error::other)?,
                size,
            )
            .map_err(|error| {
                io::Error::other(format!("Cannot create Servo graphics context: {error:?}"))
            })?,
        ))
    };
    #[cfg(feature = "e2e")]
    {
        dispatch(
            software_enabled(_quiet),
            size,
            |size| {
                Ok(
                    Rc::new(servo::SoftwareRenderingContext::new(size).map_err(|error| {
                        io::Error::other(format!(
                            "Cannot create Servo software graphics context: {error:?}"
                        ))
                    })?) as Rc<dyn RenderingContext>,
                )
            },
            native,
        )
    }
    #[cfg(not(feature = "e2e"))]
    native(size)
}

#[cfg(all(test, feature = "e2e"))]
mod tests {
    use super::*;
    use std::cell::Cell;
    use winit::dpi::PhysicalSize;

    #[test]
    fn software_dispatch_forwards_physical_size_without_native_calls() {
        let size = PhysicalSize::new(937, 521);
        let software_calls = Cell::new(0);
        let result = dispatch(
            true,
            size,
            |received| {
                software_calls.set(software_calls.get() + 1);
                assert_eq!(received, size);
                Ok(received)
            },
            |_| panic!("quiet software dispatch must not call native constructor"),
        );
        assert_eq!(result.unwrap(), size);
        assert_eq!(software_calls.get(), 1);
    }

    #[test]
    fn native_dispatch_forwards_physical_size_without_software_calls() {
        let size = PhysicalSize::new(813, 479);
        let native_calls = Cell::new(0);
        let result = dispatch(
            false,
            size,
            |_| panic!("native dispatch must not call software constructor"),
            |received| {
                native_calls.set(native_calls.get() + 1);
                assert_eq!(received, size);
                Ok(received)
            },
        );
        assert_eq!(result.unwrap(), size);
        assert_eq!(native_calls.get(), 1);
    }

    #[test]
    fn quiet_reveal_never_calls_native_even_with_native_rendering() {
        let mut quiet = super::super::Pretended::default();
        let native_context = dispatch(
            false,
            PhysicalSize::new(937, 521),
            |_| panic!("expected native rendering"),
            Ok,
        )
        .unwrap();
        assert_eq!(native_context, PhysicalSize::new(937, 521));
        super::super::reveal_window(Some(&mut quiet), || {
            panic!("quiet reveal must never show or focus the native window")
        });
        assert!(quiet.visible);
        let calls = Cell::new(0);
        super::super::reveal_window(None, || calls.set(calls.get() + 1));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn software_constructor_error_does_not_fall_back_to_native() {
        let result: io::Result<()> = dispatch(
            true,
            PhysicalSize::new(937, 521),
            |_| Err(io::Error::other("software failure")),
            |_| panic!("failed software constructor must not call native"),
        );
        assert_eq!(result.unwrap_err().to_string(), "software failure");
    }
}
