//! Typed configuration ownership and update coordination.

use std::{
    fs,
    io::{self, ErrorKind},
    sync::OnceLock,
};

use log::{error, info};
use willow_schema::config::v1::Config;

static CONFIG: OnceLock<Config> = OnceLock::new();

const CONFIG_PATH: &str = "/spiffs/user/config/willow.json";
const LOG_TARGET: &str = "WILLOW/CONFIG";

/// Returns the configuration after successful startup parsing.
///
/// Keeping the document in a [`OnceLock`] makes its references stable and
/// reflects the firmware lifecycle: writing a new configuration schedules a
/// restart instead of mutating the active document.
pub(crate) fn config() -> Option<&'static Config> {
    CONFIG.get()
}

/// Loads, parses, and installs the configuration document exactly once.
///
/// Rust owns the file contents from the filesystem read through schema
/// deserialization. The two raw document prints intentionally preserve the
/// existing firmware's boot-time logging behavior.
pub(crate) fn load() {
    let metadata = match fs::metadata(CONFIG_PATH) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            info!(
                target: LOG_TARGET,
                "{CONFIG_PATH} does not exist, will be requested from WAS"
            );
            return;
        }
        Err(error) => {
            error!(target: LOG_TARGET, "failed to get file status for {CONFIG_PATH}: {error}");
            return;
        }
    };

    info!(target: LOG_TARGET, "opening {CONFIG_PATH}");
    info!(target: LOG_TARGET, "config file size: {}", metadata.len());

    let json = match fs::read_to_string(CONFIG_PATH) {
        Ok(json) => json,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to read {CONFIG_PATH}: {error}");
            return;
        }
    };

    info!(target: LOG_TARGET, "fread: {}", json.len());
    info!(target: LOG_TARGET, "config file content: {json}");

    let config = match serde_json::from_str(&json) {
        Ok(config) => config,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to parse configuration: {error}");
            error!(target: LOG_TARGET, "failed to parse config file");
            return;
        }
    };

    if CONFIG.set(config).is_err() {
        error!(target: LOG_TARGET, "configuration was already initialized");
        return;
    }

    info!(target: LOG_TARGET, "parsed config file:\n{json}");
}

pub(crate) fn is_valid() -> bool {
    CONFIG.get().is_some()
}

/// Writes a configuration document without parsing or rewriting its bytes.
pub(crate) fn write(data: &[u8]) -> io::Result<()> {
    fs::write(CONFIG_PATH, data)
}

/// Replaces the stored document and restarts after stopping active services.
pub(crate) fn replace(data: &[u8]) -> ! {
    crate::was::deinitialize();
    crate::audio::deinitialize();

    let updated = match write(data) {
        Ok(()) => true,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to write {CONFIG_PATH}: {error:#?}");
            false
        }
    };

    if updated {
        info!(target: LOG_TARGET, "{CONFIG_PATH} updated, restarting");
        crate::ui::show_center_message("Configuration Updated");
        if let Err(error) = crate::backlight::reset_display_timer(true) {
            error!(target: LOG_TARGET, "failed to pause display timer: {error:#?}");
        }
        crate::backlight::set(true, false);
    }

    crate::system::restart_delayed()
}
