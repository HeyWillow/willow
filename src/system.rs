//! System-level operations shared with the retained C application.

use core::ffi::{CStr, c_char};

use esp_idf_svc::hal::{delay::FreeRtos, reset};
use esp_idf_sys::esp_random;
use log::{debug, info};

const LOG_TARGET: &str = "WILLOW/SYSTEM";
const MAXIMUM_RESTART_DELAY_SECONDS: u32 = 6;
const MINIMUM_RESTART_DELAY_SECONDS: u32 = 3;
const RESTART_DELAY_RANGE: u32 = 9;

#[derive(Clone, Copy)]
pub(crate) enum Hardware {
    Esp32S3Box,
    Esp32S3Box3,
    Esp32S3BoxLite,
    Unsupported,
}

const HARDWARE: Hardware = if cfg!(esp_idf_esp32_s3_box_board) {
    Hardware::Esp32S3Box
} else if cfg!(esp_idf_esp32_s3_box_lite_board) {
    Hardware::Esp32S3BoxLite
} else if cfg!(esp_idf_esp32_s3_box_3_board) {
    Hardware::Esp32S3Box3
} else {
    Hardware::Unsupported
};

impl Hardware {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Esp32S3Box => "ESP32-S3-BOX",
            Self::Esp32S3Box3 => "ESP32-S3-BOX-3",
            Self::Esp32S3BoxLite => "ESP32-S3-BOX-Lite",
            Self::Unsupported => "HW-UNSUPPORTED",
        }
    }

    const fn c_name(self) -> &'static CStr {
        match self {
            Self::Esp32S3Box => c"ESP32-S3-BOX",
            Self::Esp32S3Box3 => c"ESP32-S3-BOX-3",
            Self::Esp32S3BoxLite => c"ESP32-S3-BOX-Lite",
            Self::Unsupported => c"HW-UNSUPPORTED",
        }
    }
}

pub(crate) const fn hardware() -> Hardware {
    HARDWARE
}

pub(crate) fn log_hardware() {
    let hardware = hardware();
    debug!(target: LOG_TARGET, "hardware type {}", hardware.name());
}

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

/// Returns the hardware name on behalf of callers not yet migrated from C.
#[unsafe(no_mangle)]
pub extern "C" fn rust_system_hardware_name() -> *const c_char {
    hardware().c_name().as_ptr()
}
