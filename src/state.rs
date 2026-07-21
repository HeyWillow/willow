//! Firmware state shared with the remaining C implementation.
//!
//! This deliberately preserves the existing model: startup progress and the
//! restart flag are independent. C uses semantic functions so it does not
//! duplicate Rust enum representations across the FFI boundary.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[repr(u8)]
enum State {
    // Declaration order preserves the C enum's startup-progress ordering.
    Init = 0,
    NvsOk = 1,
    ConfigOk = 2,
    Ready = 3,
}

static RESTARTING: AtomicBool = AtomicBool::new(false);
static STATE: AtomicU8 = AtomicU8::new(State::Init as u8);

fn current() -> State {
    match STATE.load(Ordering::SeqCst) {
        value if value == State::Init as u8 => State::Init,
        value if value == State::NvsOk as u8 => State::NvsOk,
        value if value == State::ConfigOk as u8 => State::ConfigOk,
        value if value == State::Ready as u8 => State::Ready,
        _ => unreachable!("invalid firmware state"),
    }
}

fn is_nvs_ok() -> bool {
    current() as u8 >= State::NvsOk as u8
}

pub(crate) fn is_restarting() -> bool {
    RESTARTING.load(Ordering::SeqCst)
}

/// Reports whether startup has successfully loaded the required NVS values.
#[unsafe(no_mangle)]
pub extern "C" fn rust_state_is_nvs_ok() -> bool {
    is_nvs_ok()
}

/// Reports whether service shutdown for a restart has begun.
#[unsafe(no_mangle)]
pub extern "C" fn rust_state_is_restarting() -> bool {
    is_restarting()
}

/// Resets startup progress to its initial state.
pub(crate) fn mark_init() {
    STATE.store(State::Init as u8, Ordering::SeqCst);
}

/// Records that startup successfully loaded the required NVS values.
#[unsafe(no_mangle)]
pub extern "C" fn rust_state_mark_nvs_ok() {
    STATE.store(State::NvsOk as u8, Ordering::SeqCst);
}

/// Records that service shutdown for a restart has begun.
#[unsafe(no_mangle)]
pub extern "C" fn rust_state_mark_restarting() {
    RESTARTING.store(true, Ordering::SeqCst);
}
