// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session-owned global shortcut requests. The Host::ui caller is still a window id.
use crate::ids::SessionId;
use std::{
    io,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn allocate_from(next: &AtomicU64) -> io::Result<ShortcutToken> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        value
            .checked_add(1)
            .filter(|value| *value <= ShortcutToken::MAX)
    })
    .map(|previous| ShortcutToken(previous + 1))
    .map_err(|_| io::Error::other("Shortcut token space exhausted"))
}

/// A process-unique event identity shared by native and substituted registrations.
/// Zero is reserved and must never be returned by registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutToken(pub u64);

impl ShortcutToken {
    pub const MAX: u64 = (1 << 53) - 1;

    pub fn allocate() -> io::Result<Self> {
        allocate_from(&NEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn allocation_starts_at_one_and_consumes_every_identity_once() {
        let next = AtomicU64::new(0);
        for expected in 1..=4 {
            assert_eq!(allocate_from(&next).unwrap(), ShortcutToken(expected));
        }
    }

    #[test]
    fn allocation_stops_at_the_js_safe_limit_without_wrapping() {
        let next = AtomicU64::new(ShortcutToken::MAX - 1);
        assert_eq!(
            allocate_from(&next).unwrap(),
            ShortcutToken(ShortcutToken::MAX)
        );
        assert!(allocate_from(&next).is_err());
        assert_eq!(next.load(Ordering::Relaxed), ShortcutToken::MAX);
        let invalid = AtomicU64::new(u64::MAX);
        assert!(allocate_from(&invalid).is_err());
        assert_eq!(invalid.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn shared_allocation_is_unique_across_concurrent_callers() {
        let mut values = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        (0..128)
                            .map(|_| ShortcutToken::allocate().unwrap().0)
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            threads
                .into_iter()
                .flat_map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        values.sort_unstable();
        assert_eq!(values.len(), 512);
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(values
            .iter()
            .all(|token| (1..=ShortcutToken::MAX).contains(token)));
    }
}

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
