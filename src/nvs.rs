//! Validation and storage of NVS provisioning documents received from WAS.
//!
//! C still owns WebSocket dispatch, UI feedback, and restarting the device.
//! This module owns the complete provisioning document once it crosses the
//! FFI boundary, validates it with `willow-schema`, and writes its values
//! through the safe `esp-idf-svc` NVS API.

use core::{ffi::c_char, fmt};
use std::{
    ffi::{CStr, CString},
    sync::OnceLock,
};

use esp_idf_svc::{
    handle::RawHandle,
    nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs},
};
use esp_idf_sys::{
    ESP_ERR_NVS_NO_FREE_PAGES, EspError, esp_err_t, nvs_commit, nvs_flash_erase, nvs_set_str,
};
use log::error;
use willow_schema::nvs::v1::Config;

const LOG_TARGET: &str = "WILLOW/WAS";

static DEFAULT_PARTITION: OnceLock<EspDefaultNvsPartition> = OnceLock::new();

struct NvsBatch<'a> {
    namespace: &'a EspDefaultNvs,
}

impl<'a> NvsBatch<'a> {
    fn new(namespace: &'a EspDefaultNvs) -> Self {
        Self { namespace }
    }

    fn set_str(&mut self, key: &CStr, value: &CStr) -> Result<(), EspError> {
        check(unsafe { nvs_set_str(self.namespace.handle(), key.as_ptr(), value.as_ptr()) })
    }

    fn commit(self) -> Result<(), EspError> {
        check(unsafe { nvs_commit(self.namespace.handle()) })
    }
}

pub(crate) enum ApplyError {
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

fn check(result: esp_err_t) -> Result<(), EspError> {
    if let Some(error) = EspError::from(result) {
        Err(error)
    } else {
        Ok(())
    }
}

fn nvs_string(field: &'static str, value: &str) -> Result<CString, ApplyError> {
    CString::new(value).map_err(|_| ApplyError::InteriorNul(field))
}
/// Validates the complete wire document before changing either NVS namespace.
///
/// ESP-IDF stores these values as C strings, so reject interior NUL bytes up
/// front rather than discovering one after an earlier value has been written.
pub(crate) fn apply_document(data: &[u8]) -> Result<(), ApplyError> {
    let config: Config = serde_json::from_slice(data)?;
    let was_url = nvs_string("WAS URL", config.was.url.as_str())?;
    let wifi_psk = nvs_string("Wi-Fi PSK", config.wifi.psk.as_str())?;
    let wifi_ssid = nvs_string("Wi-Fi SSID", config.wifi.ssid.as_str())?;

    let partition = DEFAULT_PARTITION
        .get()
        .expect("default NVS partition must be initialized before use")
        .clone();

    let was = EspNvs::new(partition.clone(), "WAS", true)?;
    let mut was_batch = NvsBatch::new(&was);
    was_batch.set_str(c"URL", &was_url)?;
    was_batch.commit()?;

    let wifi = EspNvs::new(partition, "WIFI", true)?;
    let mut wifi_batch = NvsBatch::new(&wifi);
    wifi_batch.set_str(c"PSK", &wifi_psk)?;
    wifi_batch.set_str(c"SSID", &wifi_ssid)?;
    wifi_batch.commit()?;

    Ok(())
}

/// Initializes and retains the default NVS partition for the firmware's
/// lifetime.
///
/// Keeping the safe wrapper's ownership token here lets Rust NVS users clone
/// it without reinitializing the global ESP-IDF service. C may continue using
/// the already-initialized service through `nvs_open` during the migration.
pub(crate) fn initialize() -> Result<(), EspError> {
    let partition = match EspDefaultNvsPartition::take_with(false) {
        Ok(partition) => partition,
        Err(error) if error.code() == ESP_ERR_NVS_NO_FREE_PAGES => {
            check(unsafe { nvs_flash_erase() })?;
            EspDefaultNvsPartition::take_with(false)?
        }
        Err(error) => return Err(error),
    };
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
