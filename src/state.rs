//! Firmware restart state.

use core::sync::atomic::{AtomicBool, Ordering};

static RESTARTING: AtomicBool = AtomicBool::new(false);

pub(crate) fn is_restarting() -> bool {
    RESTARTING.load(Ordering::SeqCst)
}

/// Records that service shutdown for a restart has begun.
pub(crate) fn mark_restarting() {
    RESTARTING.store(true, Ordering::SeqCst);
}
