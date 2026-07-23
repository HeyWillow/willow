//! Mounting and lifetime ownership for Willow's user SPIFFS partition.

use esp_idf_svc::{fs::spiffs::Spiffs, io::vfs::MountedSpiffs};
use esp_idf_sys::{ESP_ERR_INVALID_STATE, EspError};
use std::sync::OnceLock;

const MAX_OPEN_FILES: usize = 5;
const MOUNT_PATH: &str = "/spiffs/user";
const PARTITION_LABEL: &str = "user";

static USER_SPIFFS: OnceLock<MountedSpiffs<Spiffs>> = OnceLock::new();

/// Mounts the user SPIFFS partition and retains it for firmware lifetime.
pub(crate) fn mount() -> Result<(), EspError> {
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
