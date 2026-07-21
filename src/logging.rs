//! ESP-IDF logging initialization and Willow's target-level policy.

use esp_idf_svc::log::{self as esp_log, EspIdfLogFilter};
use esp_idf_sys::EspError;
use log::LevelFilter;

const WILLOW_TARGETS: [&str; 19] = [
    "WILLOW/AUDIO",
    "WILLOW/CONFIG",
    "WILLOW/DISPLAY",
    "WILLOW/ETHERNET",
    "WILLOW/HASS",
    "WILLOW/HTTP",
    "WILLOW/I2C",
    "WILLOW/INPUT",
    "WILLOW/LVGL",
    "WILLOW/MAIN",
    "WILLOW/NETWORK",
    "WILLOW/OPENHAB",
    "WILLOW/OTA",
    "WILLOW/REST",
    "WILLOW/RUST",
    "WILLOW/SYSTEM",
    "WILLOW/TIMER",
    "WILLOW/UI",
    "WILLOW/WAS",
];

/// Installs the Rust logger backed by ESP-IDF's target-level filter.
///
/// The returned filter lets [`apply_policy`] configure both Rust log targets
/// and native ESP-IDF C tags through the safe `esp-idf-svc` wrapper.
pub(crate) fn initialize() -> &'static EspIdfLogFilter {
    esp_log::init_from_esp_idf().filter()
}

/// Applies the logging policy formerly implemented by `main/log.c`.
///
/// Policy application remains separate from logger installation so Rust can
/// preserve the existing startup order while the rest of initialization is
/// still owned by C.
pub(crate) fn apply_policy(filter: &EspIdfLogFilter) -> Result<(), EspError> {
    let willow_level = if cfg!(esp_idf_willow_debug_log) {
        filter.set_target_level("*", LevelFilter::Debug)?;
        LevelFilter::Debug
    } else {
        filter.set_target_level("*", LevelFilter::Error)?;
        filter.set_target_level("AUDIO_RECORDER", LevelFilter::Info)?;
        LevelFilter::Info
    };

    for target in WILLOW_TARGETS {
        filter.set_target_level(target, willow_level)?;
    }

    Ok(())
}
