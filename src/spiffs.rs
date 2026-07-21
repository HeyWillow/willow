//! Mounting and lifetime ownership for Willow's user SPIFFS partition.

use esp_idf_svc::{fs::spiffs::Spiffs, io::vfs::MountedSpiffs};
use esp_idf_sys::{ESP_ERR_INVALID_STATE, ESP_OK, EspError, esp_err_t};
use log::{error, info};
use std::sync::OnceLock;

const LOG_TARGET: &str = "WILLOW/MAIN";
const MAX_OPEN_FILES: usize = 5;
const MOUNT_PATH: &str = "/spiffs/user";
const PARTITION_LABEL: &str = "user";

static USER_SPIFFS: OnceLock<MountedSpiffs<Spiffs>> = OnceLock::new();

fn mount() -> Result<(), EspError> {
    if USER_SPIFFS.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    // This module is the sole owner of the user partition mount. ESP-IDF's
    // safe wrapper cannot express that flash-partition ownership in its type.
    let spiffs = unsafe { Spiffs::new(PARTITION_LABEL)? };
    let mounted = MountedSpiffs::mount(spiffs, MOUNT_PATH, MAX_OPEN_FILES)?;

    USER_SPIFFS
        .set(mounted)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())?;

    Ok(())
}

/// Mounts the user SPIFFS partition and retains it for the firmware lifetime.
///
/// C calls this at the existing mount point in its startup sequence. The
/// returned error deliberately remains advisory because the previous C caller
/// also continued startup after a mount failure.
#[unsafe(no_mangle)]
pub extern "C" fn rust_spiffs_mount() -> esp_err_t {
    match mount() {
        Ok(()) => {
            info!(target: LOG_TARGET, "SPIFFS mounted");
            ESP_OK
        }
        Err(error) => {
            error!(target: LOG_TARGET, "failed to mount SPIFFS user partition: {error}");
            error.code()
        }
    }
}
