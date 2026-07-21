//! Typed configuration ownership and the temporary legacy C adapter.
//!
//! Rust validates and owns the shared [`Config`] document. Existing C
//! consumers still request values by string key, so this module deliberately
//! maps those requests to typed fields. New Rust consumers should access
//! [`config`] directly instead of extending the string-based interface.

use core::ffi::c_char;
use std::{borrow::Cow, ffi::CStr, fs, io::ErrorKind, ptr, sync::OnceLock};

use log::{error, info};
use serde_json::Value;
use willow_schema::config::v1::{Config, VadMode};

static CONFIG: OnceLock<Config> = OnceLock::new();

const CONFIG_PATH: &str = "/spiffs/user/config/willow.json";
const LOG_TARGET: &str = "WILLOW/CONFIG";

/// Returns the configuration after successful startup parsing.
///
/// Keeping the document in a [`OnceLock`] makes its references stable and
/// reflects the firmware lifecycle: writing a new configuration schedules a
/// restart instead of mutating the active document.
fn config() -> Option<&'static Config> {
    CONFIG.get()
}

/// Adapts Boolean schema fields to the legacy string-keyed C getter.
fn bool_value(config: &Config, key: &str) -> Option<bool> {
    match key {
        "aec" => config.aec,
        "bss" => config.bss,
        "mqtt_tls" => config.mqtt_tls,
        "multiwake" => config.multiwake,
        "show_prereleases" => config.show_prereleases,
        "wake_confirmation" => config.wake_confirmation,
        _ => None,
    }
}

/// Adapts numeric schema fields to the legacy string-keyed C getter.
///
/// The signed return type reserves `-1` as the FFI missing-value sentinel;
/// configuration numbers are unsigned in the shared schema.
fn int_value(config: &Config, key: &str) -> Option<i64> {
    match key {
        "display_timeout" => config.display_timeout.map(i64::from),
        "lcd_brightness" => config.lcd_brightness.map(i64::from),
        "lvgl_lock_timeout" => config.lvgl_lock_timeout.map(i64::from),
        "mic_gain" => config.mic_gain.map(i64::from),
        "mqtt_port" => config.mqtt_port.map(i64::from),
        "record_buffer" => config.record_buffer.map(i64::from),
        "speaker_volume" => config.speaker_volume.map(i64::from),
        "stream_timeout" => config.stream_timeout.map(i64::from),
        "vad_mode" => config.vad_mode.map(|value| match value {
            VadMode::Mode0 => 0,
            VadMode::Mode1 => 1,
            VadMode::Mode2 => 2,
            VadMode::Mode3 => 3,
            VadMode::Mode4 => 4,
        }),
        "vad_timeout" => config.vad_timeout.map(i64::from),
        _ => None,
    }
}

/// Adapts string and string-valued enum fields to the legacy C getter.
///
/// Owned schema strings are borrowed. Enums are serialized through Serde so
/// their wire spelling continues to come from `willow-schema` rather than a
/// second manually maintained list in the firmware.
fn string_value<'a>(config: &'a Config, key: &str) -> Option<Cow<'a, str>> {
    macro_rules! serialized_enum {
        ($field:expr) => {{
            let value = $field.as_ref()?;
            match serde_json::to_value(value).ok()? {
                Value::String(value) => Some(Cow::Owned(value)),
                _ => None,
            }
        }};
    }

    match key {
        "audio_codec" => serialized_enum!(config.audio_codec),
        "audio_response_type" => serialized_enum!(config.audio_response_type),
        "mqtt_auth_type" => serialized_enum!(config.mqtt_auth_type),
        "mqtt_host" => config.mqtt_host.as_deref().map(Cow::Borrowed),
        "mqtt_password" => config.mqtt_password.as_deref().map(Cow::Borrowed),
        "mqtt_topic" => config.mqtt_topic.as_deref().map(Cow::Borrowed),
        "mqtt_username" => config.mqtt_username.as_deref().map(Cow::Borrowed),
        "ntp_config" => serialized_enum!(config.ntp_config),
        "ntp_host" => config.ntp_host.as_deref().map(Cow::Borrowed),
        "speech_rec_mode" => serialized_enum!(config.speech_rec_mode),
        "timezone" => config.timezone.as_deref().map(Cow::Borrowed),
        "timezone_name" => config.timezone_name.as_deref().map(Cow::Borrowed),
        "wake_mode" => serialized_enum!(config.wake_mode),
        "wake_word" => config.wake_word.as_deref().map(Cow::Borrowed),
        "wis_tts_url" => config.wis_tts_url.as_deref().map(Cow::Borrowed),
        "wis_tts_url_v2" => config.wis_tts_url_v2.as_deref().map(Cow::Borrowed),
        "wis_url" => config.wis_url.as_deref().map(Cow::Borrowed),
        _ => None,
    }
}

/// Borrows a UTF-8 key supplied by C.
///
/// # Safety
///
/// `key` must either be null or point to a valid NUL-terminated string for the
/// duration of the returned borrow.
unsafe fn key_from_ptr<'a>(key: *const c_char) -> Option<&'a str> {
    if key.is_null() {
        return None;
    }

    unsafe { CStr::from_ptr(key) }.to_str().ok()
}

/// Copies a configured string into storage allocated by the C caller.
///
/// This preserves the existing ownership contract: `config_get_char()`
/// returns storage that C may release with `free()`. The buffer must be at
/// least [`rust_config_get_char_len`] plus one byte for the terminating NUL.
///
/// # Safety
///
/// `key` must satisfy [`key_from_ptr`]. `output` must be writable for
/// `output_len` bytes when it is non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_config_copy_char(
    key: *const c_char,
    output: *mut c_char,
    output_len: usize,
) -> bool {
    let Some(key) = (unsafe { key_from_ptr(key) }) else {
        return false;
    };
    let Some(value) = config().and_then(|config| string_value(config, key)) else {
        return false;
    };
    let bytes = value.as_bytes();

    if output.is_null() || output_len <= bytes.len() {
        return false;
    }

    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast(), bytes.len());
        output.add(bytes.len()).write(0);
    }

    true
}

/// Returns `0` or `1` for a configured Boolean and `-1` when it is absent.
///
/// # Safety
///
/// `key` must satisfy [`key_from_ptr`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_config_get_bool(key: *const c_char) -> i8 {
    let Some(key) = (unsafe { key_from_ptr(key) }) else {
        return -1;
    };

    config()
        .and_then(|config| bool_value(config, key))
        .map_or(-1, |value| if value { 1 } else { 0 })
}

/// Returns a configured string's byte length or `-1` when it is absent.
///
/// The length excludes the terminating NUL expected by C.
///
/// # Safety
///
/// `key` must satisfy [`key_from_ptr`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_config_get_char_len(key: *const c_char) -> isize {
    let Some(key) = (unsafe { key_from_ptr(key) }) else {
        return -1;
    };

    config()
        .and_then(|config| string_value(config, key))
        .and_then(|value| isize::try_from(value.len()).ok())
        .unwrap_or(-1)
}

/// Returns a configured integer or `-1` when it is absent.
///
/// # Safety
///
/// `key` must satisfy [`key_from_ptr`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_config_get_int(key: *const c_char) -> i64 {
    let Some(key) = (unsafe { key_from_ptr(key) }) else {
        return -1;
    };

    config()
        .and_then(|config| int_value(config, key))
        .unwrap_or(-1)
}

/// Loads, parses, and installs the configuration document exactly once.
///
/// Rust owns the file contents from the filesystem read through schema
/// deserialization. The two raw document prints intentionally preserve the
/// existing firmware's boot-time logging behavior.
#[unsafe(no_mangle)]
pub extern "C" fn rust_config_load() -> bool {
    let metadata = match fs::metadata(CONFIG_PATH) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            info!(
                target: LOG_TARGET,
                "{CONFIG_PATH} does not exist, will be requested from WAS"
            );
            return false;
        }
        Err(error) => {
            error!(target: LOG_TARGET, "failed to get file status for {CONFIG_PATH}: {error}");
            return false;
        }
    };

    info!(target: LOG_TARGET, "opening {CONFIG_PATH}");
    info!(target: LOG_TARGET, "config file size: {}", metadata.len());

    let json = match fs::read_to_string(CONFIG_PATH) {
        Ok(json) => json,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to read {CONFIG_PATH}: {error}");
            return false;
        }
    };

    info!(target: LOG_TARGET, "fread: {}", json.len());
    info!(target: LOG_TARGET, "config file content: {json}");

    let config = match serde_json::from_str(&json) {
        Ok(config) => config,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to parse configuration: {error}");
            error!(target: LOG_TARGET, "failed to parse config file");
            return false;
        }
    };

    if CONFIG.set(config).is_err() {
        error!(target: LOG_TARGET, "configuration was already initialized");
        return false;
    }

    info!(target: LOG_TARGET, "parsed config file:\n{json}");

    true
}

/// Writes a configuration document supplied by the legacy C message handler.
///
/// Rust owns only the filesystem operation. C continues to coordinate service
/// shutdown, UI feedback, and the restart around this call. The bytes are not
/// parsed or rewritten, preserving the existing device-facing behavior.
///
/// # Safety
///
/// `data` must either be null or point to a valid NUL-terminated byte string
/// for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_config_write(data: *const c_char) -> bool {
    if data.is_null() {
        error!(target: LOG_TARGET, "cannot write a null configuration");
        return false;
    }

    let data = unsafe { CStr::from_ptr(data) };
    if let Err(error) = fs::write(CONFIG_PATH, data.to_bytes()) {
        error!(target: LOG_TARGET, "failed to write {CONFIG_PATH}: {error}");
        return false;
    }

    info!(target: LOG_TARGET, "{CONFIG_PATH} updated, restarting");

    true
}
