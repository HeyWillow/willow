//! Firmware restart state shared with the remaining C implementation.

use core::sync::atomic::{AtomicBool, Ordering};

static RESTARTING: AtomicBool = AtomicBool::new(false);

pub(crate) fn is_restarting() -> bool {
    RESTARTING.load(Ordering::SeqCst)
}

/// Reports whether service shutdown for a restart has begun.
#[unsafe(no_mangle)]
pub extern "C" fn rust_state_is_restarting() -> bool {
    is_restarting()
}

/// Records that service shutdown for a restart has begun.
#[unsafe(no_mangle)]
pub extern "C" fn rust_state_mark_restarting() {
    RESTARTING.store(true, Ordering::SeqCst);
}
