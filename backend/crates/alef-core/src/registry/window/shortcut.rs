// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session-owned global shortcut requests. The Host::ui caller is still a window id.
use crate::ids::SessionId;

/// A native registration token, allocated by the UI host (not a hotkey hash).
/// Zero is reserved and must never be returned by registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutToken(pub u64);

/// Registration replies are a JSON integer token; unregister replies are null.
/// Unregister must accept the original owner even after its document was replaced.
/// The host verifies (caller window, owner, token) against the registration, not the
/// current document. Unregister is idempotent; native release completes before its reply.
/// A registration error or malformed reply must leave no native registration behind.
/// The host must bound request completion and release all remaining registrations at shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutCall {
    Register {
        owner: SessionId,
        accelerator: String,
    },
    Unregister {
        owner: SessionId,
        token: ShortcutToken,
    },
}
