//! System-level operations shared with the retained C application.

use esp_idf_svc::hal::{delay::FreeRtos, reset};
use esp_idf_sys::esp_random;
use log::info;

const LOG_TARGET: &str = "WILLOW/SYSTEM";
const MAXIMUM_RESTART_DELAY_SECONDS: u32 = 6;
const MINIMUM_RESTART_DELAY_SECONDS: u32 = 3;
const RESTART_DELAY_RANGE: u32 = 9;

fn restart_delay_seconds() -> u32 {
    let random = unsafe { esp_random() };
    (random % RESTART_DELAY_RANGE)
        .clamp(MINIMUM_RESTART_DELAY_SECONDS, MAXIMUM_RESTART_DELAY_SECONDS)
}

/// Shows the restart state, waits for the randomized delay, and restarts.
pub(crate) fn restart_delayed() -> ! {
    let delay_seconds = restart_delay_seconds();
    info!(target: LOG_TARGET, "restarting after {delay_seconds} seconds");

    crate::ui::show_connecting(&format!("Restarting in {delay_seconds} seconds"));
    FreeRtos::delay_ms(delay_seconds * 1_000);
    reset::restart()
}

/// Restarts after a delay on behalf of callers not yet migrated from C.
#[unsafe(no_mangle)]
pub extern "C" fn rust_system_restart_delayed() {
    restart_delayed()
}
