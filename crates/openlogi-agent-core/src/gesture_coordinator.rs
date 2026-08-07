//! Lock-free arbitration shared by OS-hook and dedicated HID++ gesture input.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic token source implementing newest-press-wins gesture arbitration.
///
/// Acquiring and validating a token use only atomic operations, so the OS input
/// callback never blocks on the HID watcher (or vice versa).
#[derive(Clone, Default)]
pub struct GestureCoordinator {
    current: Arc<AtomicU64>,
}

impl GestureCoordinator {
    /// Make a new gesture press current, invalidating every older token.
    #[must_use]
    pub fn acquire(&self) -> GestureToken {
        GestureToken(self.current.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }

    /// Whether `token` still represents the newest gesture press.
    #[must_use]
    pub fn is_current(&self, token: GestureToken) -> bool {
        self.current.load(Ordering::Acquire) == token.0
    }
}

/// Opaque identity of one gesture press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureToken(u64);
