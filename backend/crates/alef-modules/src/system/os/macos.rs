// SPDX-License-Identifier: MIT OR Apache-2.0
//! macOS power notifications; all Objective-C handles are owner-thread confined.
use super::super::power::{PowerBackend, PowerEvent, PowerSink, PowerWatch};
use alef_core::{AlefError, ErrorCode};
use std::{sync::mpsc, thread, time::Duration};
#[derive(Debug, Default)]
pub struct MacOsPower;
impl PowerBackend for MacOsPower {
    fn watch(&self, sink: PowerSink) -> Result<PowerWatch, AlefError> {
        // The worker reports readiness once the observers are registered. A panic in its setup
        // closes the channel and a stalled setup times out; both end in NOT_AVAILABLE below,
        // and a worker that may be stuck is never joined.
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), AlefError>>(1);
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("alef-power-macos".into())
            .spawn(move || {
                use block2::RcBlock;
                use objc2::runtime::AnyObject;
                use objc2_app_kit::{
                    NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification,
                };
                use objc2_foundation::NSNotification;
                use std::ptr::NonNull;
                let center = NSWorkspace::sharedWorkspace().notificationCenter();
                // The blocks run on the thread that posts the notification and only touch the
                // `PowerSink` (`Arc<dyn Fn + Send + Sync>`), so no `Send` bound is needed here;
                // `RcBlock::new` accepts any `Fn` and the observer tokens stay on this thread.
                let sleeping_block = RcBlock::new({
                    let sink = sink.clone();
                    move |_note: NonNull<NSNotification>| {
                        sink(PowerEvent::Suspend);
                    }
                });
                let waking_block = RcBlock::new(move |_note: NonNull<NSNotification>| {
                    sink(PowerEvent::Resume);
                });
                // SAFETY: static notification names and nil object/queue are valid; retained
                // center and blocks are held through unregistration to keep observers valid.
                let sleeping = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(NSWorkspaceWillSleepNotification),
                        None,
                        None,
                        &sleeping_block,
                    )
                };
                let waking = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(NSWorkspaceDidWakeNotification),
                        None,
                        None,
                        &waking_block,
                    )
                };
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
                // SAFETY: the typed observer handles come from addObserver and are valid
                // Objective-C objects for removal by the center that registered them.
                // `ProtocolObject` type-erases to `AnyObject` via `AsRef` (not `Deref`).
                let sleeping_ref: &AnyObject = sleeping.as_ref();
                let waking_ref: &AnyObject = waking.as_ref();
                unsafe {
                    center.removeObserver(sleeping_ref);
                    center.removeObserver(waking_ref);
                }
                drop((sleeping, waking, center, sleeping_block, waking_block));
            })
            .map_err(|e| AlefError::new(ErrorCode::NotAvailable, e.to_string()))?;
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(PowerWatch::new(move || {
                let _ = stop_tx.send(());
                let _ = thread.join();
            })),
            Ok(Err(error)) => {
                let _ = stop_tx.send(());
                Err(error)
            }
            Err(_) => {
                // The thread is detached: it stops itself if its setup ever finishes.
                let _ = stop_tx.send(());
                drop(thread);
                Err(AlefError::new(
                    ErrorCode::NotAvailable,
                    "macOS power notification setup failed",
                ))
            }
        }
    }
}
