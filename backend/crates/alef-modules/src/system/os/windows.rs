// SPDX-License-Identifier: MIT OR Apache-2.0
//! Windows power state notifications.
use std::{
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::atomic::{AtomicU8, Ordering},
};

use alef_core::{AlefError, ErrorCode};
use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Power::{
        PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
    },
    UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    },
};

use super::super::power::{PowerBackend, PowerEvent, PowerSink, PowerWatch};

const UNKNOWN: u8 = 0;
const SUSPENDED: u8 = 1;
const AWAKE: u8 = 2;

fn map_status(status: u32) -> Option<(PowerEvent, u8)> {
    match status {
        PBT_APMSUSPEND => Some((PowerEvent::Suspend, SUSPENDED)),
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Some((PowerEvent::Resume, AWAKE)),
        _ => None,
    }
}

/// Reads the callback status, swaps the dedup state and returns the event to deliver, or
/// `None` for an unknown status or a repeated state (no edge).
fn decide(state: &AtomicU8, status: u32) -> Option<PowerEvent> {
    let (event, next) = map_status(status)?;
    if state.swap(next, Ordering::AcqRel) != next {
        Some(event)
    } else {
        None
    }
}

#[derive(Debug, Default)]
pub struct WindowsPower;

struct Context {
    sink: PowerSink,
    state: AtomicU8,
}

unsafe extern "system" fn callback(
    raw: *const c_void,
    status: u32,
    _setting: *const c_void,
) -> u32 {
    // SAFETY: registration holds the Box<Context> allocation stable and alive until
    // PowerUnregisterSuspendResumeNotification has returned; Windows invokes callbacks
    // only for that registration. The opaque context pointer originated from Box::into_raw.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if raw.is_null() {
            return;
        }
        // SAFETY: checked non-null; registration's lifetime contract above keeps this
        // correctly aligned Context allocated, and callbacks may run concurrently only
        // while the context remains immutable except for its atomic state.
        let context = unsafe { &*(raw.cast::<Context>()) };
        if let Some(event) = decide(&context.state, status) {
            (context.sink)(event);
        }
    }));
    0
}

impl PowerBackend for WindowsPower {
    fn watch(&self, sink: PowerSink) -> Result<PowerWatch, AlefError> {
        let context = Box::new(Context {
            sink,
            state: AtomicU8::new(UNKNOWN),
        });
        let raw_context = Box::into_raw(context);
        let mut params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(callback),
            Context: raw_context.cast::<c_void>(),
        };
        let mut registration: *mut c_void = std::ptr::null_mut();
        // SAFETY: params and its callback context stay alive for the full registration;
        // raw_context is reclaimed if registration fails, otherwise only after unregister.
        let recipient = (&mut params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast::<c_void>();
        let status = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                recipient,
                &mut registration,
            )
        };
        if status != ERROR_SUCCESS {
            // SAFETY: registration failed, so Windows retained neither callback nor context.
            unsafe {
                drop(Box::from_raw(raw_context));
            }
            return Err(AlefError::new(
                ErrorCode::NotAvailable,
                format!("power notification registration failed: {status}"),
            ));
        }
        // The cleanup closure must be `Send`, so both values cross as integers (`usize`/
        // `isize` are `Send + Copy`) instead of raw pointers, whose captures trip the
        // auto-trait analysis. Both conversions are same-width and lossless; the integers
        // carry no provenance for the compiler, so the uniqueness contract below is what
        // makes their round-trip back into pointers sound.
        let context_address = raw_context as usize;
        let handle = registration as HPOWERNOTIFY; // `HPOWERNOTIFY = isize` in windows-sys 0.59
        Ok(PowerWatch::new(move || {
            // SAFETY: `handle` is the opaque registration returned by
            // PowerRegisterSuspendResumeNotification above and is unregistered here exactly
            // once; Windows invokes the callback only through this registration.
            unsafe { PowerUnregisterSuspendResumeNotification(handle) };
            // SAFETY: `context_address` is the address of the unique `Box<Context>`
            // allocation created above; no other reference exists, Windows used it only
            // through the registration just unregistered, and its type is reconstructed
            // exactly (`*mut Context`), so reclaiming the Box here is sound.
            unsafe {
                drop(Box::from_raw(context_address as *mut Context));
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_suspend_and_resume_status_codes() {
        assert_eq!(
            map_status(PBT_APMSUSPEND),
            Some((PowerEvent::Suspend, SUSPENDED))
        );
        assert_eq!(
            map_status(PBT_APMRESUMESUSPEND),
            Some((PowerEvent::Resume, AWAKE))
        );
        assert_eq!(
            map_status(PBT_APMRESUMEAUTOMATIC),
            Some((PowerEvent::Resume, AWAKE))
        );
        assert_eq!(map_status(u32::MAX), None);
    }
    #[test]
    fn deduplicates_repeated_state_notifications() {
        let state = AtomicU8::new(UNKNOWN);
        let events = [
            PBT_APMSUSPEND,
            PBT_APMSUSPEND,
            PBT_APMRESUMESUSPEND,
            PBT_APMRESUMESUSPEND,
        ]
        .into_iter()
        .filter_map(|status| decide(&state, status))
        .collect::<Vec<_>>();
        assert_eq!(events, [PowerEvent::Suspend, PowerEvent::Resume]);
    }
}
