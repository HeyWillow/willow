//! Validation and storage of NVS provisioning documents received from WAS.
//!
//! C still owns WebSocket dispatch, UI feedback, and restarting the device.
//! This module owns the complete provisioning document once it crosses the
//! FFI boundary, validates it with `willow-schema`, and writes its values
//! through the safe `esp-idf-svc` NVS API.

use core::{ffi::c_char, fmt};
use std::{ffi::CStr, sync::OnceLock};

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_sys::EspError;
use log::error;
use willow_schema::nvs::v1::Config;

const LOG_TARGET: &str = "WILLOW/WAS";

static DEFAULT_PARTITION: OnceLock<EspDefaultNvsPartition> = OnceLock::new();

enum ApplyError {
    InteriorNul(&'static str),
    Json(serde_json::Error),
    Nvs(EspError),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InteriorNul(field) => write!(formatter, "{field} contains a NUL byte"),
            Self::Json(error) => write!(formatter, "invalid NVS document: {error}"),
            Self::Nvs(error) => write!(formatter, "NVS operation failed: {error}"),
        }
    }
}

impl From<EspError> for ApplyError {
    fn from(error: EspError) -> Self {
        Self::Nvs(error)
    }
}

impl From<serde_json::Error> for ApplyError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Validates the complete wire document before changing either NVS namespace.
///
/// ESP-IDF stores these values as C strings, so reject interior NUL bytes up
/// front rather than discovering one after an earlier value has been written.
fn apply_document(data: &[u8]) -> Result<(), ApplyError> {
    let config: Config = serde_json::from_slice(data)?;

    for (field, value) in [
        ("WAS URL", config.was.url.as_str()),
        ("Wi-Fi PSK", config.wifi.psk.as_str()),
        ("Wi-Fi SSID", config.wifi.ssid.as_str()),
    ] {
        if value.contains('\0') {
            return Err(ApplyError::InteriorNul(field));
        }
    }

    let partition = DEFAULT_PARTITION
        .get()
        .expect("default NVS partition must be initialized before use")
        .clone();

    let was = EspNvs::new(partition.clone(), "WAS", true)?;
    was.set_str("URL", &config.was.url)?;

    let wifi = EspNvs::new(partition, "WIFI", true)?;
    wifi.set_str("PSK", config.wifi.psk.as_str())?;
    wifi.set_str("SSID", config.wifi.ssid.as_str())?;

    Ok(())
}

/// Initializes and retains the default NVS partition for the firmware's
/// lifetime.
///
/// Keeping the safe wrapper's ownership token here lets Rust NVS users clone
/// it without reinitializing the global ESP-IDF service. C may continue using
/// the already-initialized service through `nvs_open` during the migration.
pub fn init() -> Result<(), EspError> {
    let partition = EspDefaultNvsPartition::take()?;
    assert!(
        DEFAULT_PARTITION.set(partition).is_ok(),
        "default NVS partition initialized more than once"
    );

    Ok(())
}

/// Applies a complete NVS provisioning document supplied by the C WebSocket
/// message handler.
///
/// The schema requires every value Willow needs to reconnect after C schedules
/// the existing restart. No document contents are logged here because the
/// Wi-Fi passphrase is confidential.
///
/// # Safety
///
/// `data` must either be null or point to a valid NUL-terminated byte string
/// for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_nvs_apply(data: *const c_char) -> bool {
    if data.is_null() {
        error!(target: LOG_TARGET, "cannot apply a null NVS document");
        return false;
    }

    let data = unsafe { CStr::from_ptr(data) };
    if let Err(error) = apply_document(data.to_bytes()) {
        error!(target: LOG_TARGET, "failed to apply NVS document: {error}");
        return false;
    }

    true
}
