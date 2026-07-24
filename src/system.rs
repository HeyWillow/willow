//! System-level hardware identification and restart operations.

use esp_idf_svc::hal::{delay::FreeRtos, reset};
use esp_idf_sys::esp_random;
use log::{debug, info};

const LOG_TARGET: &str = "WILLOW/SYSTEM";
const MAXIMUM_RESTART_DELAY_SECONDS: u32 = 6;
const MINIMUM_RESTART_DELAY_SECONDS: u32 = 3;
const RESTART_DELAY_RANGE: u32 = 9;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Hardware {
    Esp32S3Box,
    Esp32S3Box3,
    Esp32S3BoxLite,
    M5StackCoreS3,
    Unsupported,
}

const HARDWARE: Hardware = if cfg!(esp_idf_esp32_s3_box_board) {
    Hardware::Esp32S3Box
} else if cfg!(esp_idf_esp32_s3_box_lite_board) {
    Hardware::Esp32S3BoxLite
} else if cfg!(esp_idf_esp32_s3_box_3_board) {
    Hardware::Esp32S3Box3
} else if cfg!(esp_idf_m5stack_core_s3_board) {
    Hardware::M5StackCoreS3
} else {
    Hardware::Unsupported
};

impl Hardware {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Esp32S3Box => "ESP32-S3-BOX",
            Self::Esp32S3Box3 => "ESP32-S3-BOX-3",
            Self::Esp32S3BoxLite => "ESP32-S3-BOX-Lite",
            Self::M5StackCoreS3 => "M5Stack CoreS3",
            Self::Unsupported => "HW-UNSUPPORTED",
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
