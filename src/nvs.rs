//! Typed access to Willow's NVS provisioning data.
//!
//! This module owns the default NVS partition, validates boot-time values and
//! complete WAS provisioning documents with `willow-schema`, and accesses the
//! stored namespaces through the safe `esp-idf-svc` NVS API. Temporary FFI
//! adapters copy boot values to C until their consumers migrate to Rust.

use core::{ffi::c_char, fmt, ptr};
use std::{
    ffi::{CStr, CString},
    string::String,
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
use willow_schema::nvs::v1::{Config, Was, Wifi, WifiPsk, WifiPskError, WifiSsid, WifiSsidError};

const APPLY_LOG_TARGET: &str = "WILLOW/WAS";
const READ_LOG_TARGET: &str = "WILLOW/MAIN";

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

pub(crate) enum ReadError {
    InvalidOutput(&'static str),
    Missing(&'static str),
    Nvs(EspError),
    WifiPsk(WifiPskError),
    WifiSsid(WifiSsidError),
}

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOutput(field) => {
                write!(formatter, "output buffer for {field} is null or too small")
            }
            Self::Missing(field) => write!(formatter, "missing NVS value {field}"),
            Self::Nvs(error) => write!(formatter, "NVS operation failed: {error}"),
            Self::WifiPsk(error) => write!(formatter, "invalid WIFI/PSK: {error}"),
            Self::WifiSsid(error) => write!(formatter, "invalid WIFI/SSID: {error}"),
        }
    }
}

impl From<EspError> for ReadError {
    fn from(error: EspError) -> Self {
        Self::Nvs(error)
    }
}

/// Copies a Rust string into a caller-owned C buffer.
///
/// # Safety
///
/// `output` must point to writable storage for `output_len` bytes.
unsafe fn copy_string(
    field: &'static str,
    value: &str,
    output: *mut c_char,
    output_len: usize,
) -> Result<(), ReadError> {
    if output.is_null() || value.len() >= output_len {
        return Err(ReadError::InvalidOutput(field));
    }

    unsafe {
        ptr::copy_nonoverlapping(value.as_ptr(), output.cast(), value.len());
        output.add(value.len()).write(0);
    }

    Ok(())
}

fn default_partition() -> EspDefaultNvsPartition {
    DEFAULT_PARTITION
        .get()
        .expect("default NVS partition must be initialized before use")
        .clone()
}

fn read_string<const LENGTH: usize>(
    namespace: &EspDefaultNvs,
    field: &'static str,
    key: &str,
) -> Result<String, ReadError> {
    let mut buffer = [0; LENGTH];
    let value = namespace
        .get_str(key, &mut buffer)?
        .ok_or(ReadError::Missing(field))?;

    Ok(value.to_owned())
}

pub(crate) fn read_was() -> Result<Was, ReadError> {
    let namespace = EspNvs::new(default_partition(), "WAS", false)?;
    let url = read_string::<2048>(&namespace, "WAS/URL", "URL")?;

    Ok(Was { url })
}

pub(crate) fn read_wifi() -> Result<Wifi, ReadError> {
    let namespace = EspNvs::new(default_partition(), "WIFI", false)?;
    let psk = read_string::<64>(&namespace, "WIFI/PSK", "PSK")?;
    let ssid = read_string::<33>(&namespace, "WIFI/SSID", "SSID")?;

    Ok(Wifi {
        psk: WifiPsk::try_from(psk).map_err(ReadError::WifiPsk)?,
        ssid: WifiSsid::try_from(ssid).map_err(ReadError::WifiSsid)?,
    })
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

    let partition = default_partition();

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
/// it without reinitializing the global ESP-IDF service.
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

/// Reads the provisioned WAS URL into the existing C compatibility buffer.
///
/// Rust owns the NVS partition and validates the namespace through the shared
/// schema type. The URL is copied only because the remaining C WAS client
/// still consumes its existing global buffer.
///
/// # Safety
///
/// `output` must point to writable storage for `output_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_nvs_read_was_url(output: *mut c_char, output_len: usize) -> bool {
    let result =
        read_was().and_then(|was| unsafe { copy_string("WAS/URL", &was.url, output, output_len) });
    if let Err(error) = result {
        error!(target: READ_LOG_TARGET, "failed to read WAS NVS configuration: {error}");
        return false;
    }

    true
}

/// Reads provisioned Wi-Fi credentials into the existing C compatibility
/// buffers.
///
/// Rust owns the NVS partition and validates both values with the shared
/// schema types. The copies preserve the existing C startup boundary while
/// leaving Wi-Fi initialization and its error behavior unchanged.
///
/// # Safety
///
/// `psk` and `ssid` must point to writable storage for `psk_len` and
/// `ssid_len` bytes respectively.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_nvs_read_wifi(
    psk: *mut c_char,
    psk_len: usize,
    ssid: *mut c_char,
    ssid_len: usize,
) -> bool {
    let result = read_wifi().and_then(|wifi| unsafe {
        copy_string("WIFI/PSK", wifi.psk.as_str(), psk, psk_len)?;
        copy_string("WIFI/SSID", wifi.ssid.as_str(), ssid, ssid_len)
    });
    if let Err(error) = result {
        error!(target: READ_LOG_TARGET, "failed to read Wi-Fi NVS configuration: {error}");
        return false;
    }

    true
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
        error!(target: APPLY_LOG_TARGET, "cannot apply a null NVS document");
        return false;
    }

    let data = unsafe { CStr::from_ptr(data) };
    if let Err(error) = apply_document(data.to_bytes()) {
        error!(target: APPLY_LOG_TARGET, "failed to apply NVS document: {error}");
        return false;
    }

    true
}
