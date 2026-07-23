//! Willow Application Server transport ownership.

use core::{
    ffi::{c_char, c_void},
    sync::atomic::{AtomicPtr, Ordering},
};
use std::{
    ffi::{CStr, CString},
    sync::OnceLock,
};

use esp_idf_svc::hal::delay::TickType;
use esp_idf_sys::{
    ESP_ERR_INVALID_ARG, ESP_ERR_NO_MEM, ESP_FAIL, ESP_OK, EspError, SemaphoreHandle_t,
    WAS_RECONNECT_TIMEOUT_MS, WILLOW_QUEUE_TYPE_MUTEX, esp_err_t, esp_event_base_t,
    esp_log_level_set, esp_log_level_t_ESP_LOG_DEBUG, esp_websocket_client,
    esp_websocket_client_config_t, esp_websocket_client_destroy_on_exit,
    esp_websocket_client_handle_t, esp_websocket_client_init, esp_websocket_client_is_connected,
    esp_websocket_client_send_text, esp_websocket_client_start,
    esp_websocket_event_id_t_WEBSOCKET_EVENT_ANY, esp_websocket_register_events, xQueueCreateMutex,
};
use log::{error, info, warn};
use serde_json::{Value, json};

use crate::{state, ui};

const LOG_TARGET: &str = "WILLOW/WAS";
const USER_AGENT: &str = concat!("Willow/", env!("WILLOW_VERSION"));

static CLIENT: AtomicPtr<esp_websocket_client> = AtomicPtr::new(core::ptr::null_mut());

struct NotificationMutex(SemaphoreHandle_t);

// The FreeRTOS mutex is designed to be shared by tasks. It lives for the
// firmware lifetime, so its handle remains valid after publication here.
unsafe impl Send for NotificationMutex {}
unsafe impl Sync for NotificationMutex {}

static NOTIFICATION_MUTEX: OnceLock<NotificationMutex> = OnceLock::new();

unsafe extern "C" {
    fn willow_was_event_handler(
        arg: *mut c_void,
        event_base: esp_event_base_t,
        event_id: i32,
        event_data: *mut c_void,
    );
}

fn notification_mutex() -> Result<SemaphoreHandle_t, EspError> {
    let mutex = NOTIFICATION_MUTEX.get_or_init(|| {
        NotificationMutex(unsafe { xQueueCreateMutex(WILLOW_QUEUE_TYPE_MUTEX as u8) })
    });

    if mutex.0.is_null() {
        Err(EspError::from_infallible::<ESP_ERR_NO_MEM>())
    } else {
        Ok(mutex.0)
    }
}

pub(crate) fn client_handle() -> esp_websocket_client_handle_t {
    CLIENT.load(Ordering::Acquire)
}

pub(crate) fn is_connected(wait: bool) -> bool {
    if unsafe { esp_websocket_client_is_connected(client_handle()) } {
        return true;
    }

    if !wait {
        return false;
    }

    // Preserve the old loop exactly: it checked five more times without a
    // delay because both the loop body and increment expression advanced i.
    let mut attempt = 0;
    let max = WAS_RECONNECT_TIMEOUT_MS / 1000;
    while attempt < max {
        if unsafe { esp_websocket_client_is_connected(client_handle()) } {
            return true;
        }
        attempt += 2;
    }

    ui::show_error("WAS disconnected", None);
    false
}

fn send_text(message: &str) -> Result<usize, EspError> {
    let length = i32::try_from(message.len())
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_ARG>())?;
    let sent = unsafe {
        esp_websocket_client_send_text(
            client_handle(),
            message.as_ptr().cast(),
            length,
            TickType::new_millis(2_000).0,
        )
    };

    if sent < 0 {
        Err(EspError::from_infallible::<ESP_FAIL>())
    } else {
        Ok(sent as usize)
    }
}

pub(crate) fn send_endpoint(data: &str) -> Result<usize, EspError> {
    // The old nc_skip argument was only ever false, so a failed connection
    // check still reports the UI error and then attempts the send.
    let _ = is_connected(true);

    let Ok(Value::Object(data)) = serde_json::from_str(data) else {
        // Preserve the old successful no-op for a malformed or non-object
        // response from WIS.
        return Ok(0);
    };

    let message = serde_json::to_string(&json!({
        "cmd": "endpoint",
        "data": data,
    }))
    .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    send_text(&message).inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to send message to WAS");
    })
}

pub(crate) fn initialize(url: &str) -> Result<(), EspError> {
    if state::is_restarting() {
        return Ok(());
    }

    // Preserve the advisory allocation failure. The retained C notification
    // paths will receive the same null handle that they did previously.
    let _ = notification_mutex();

    let url = CString::new(url).map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_ARG>())?;
    let user_agent = CString::new(USER_AGENT).unwrap();

    ui::show_connecting("Connecting to WAS...");

    unsafe {
        esp_log_level_set(c"WILLOW/WAS".as_ptr(), esp_log_level_t_ESP_LOG_DEBUG);
    }
    info!(
        target: LOG_TARGET,
        "initializing WebSocket client ({})",
        url.to_string_lossy()
    );

    let config = esp_websocket_client_config_t {
        buffer_size: 4096,
        reconnect_timeout_ms: WAS_RECONNECT_TIMEOUT_MS as i32,
        task_stack: 6 * 1024,
        uri: url.as_ptr(),
        user_agent: user_agent.as_ptr(),
        ..Default::default()
    };

    let client = unsafe { esp_websocket_client_init(&config) };
    CLIENT.store(client, Ordering::Release);

    if let Some(error) = EspError::from(unsafe { esp_websocket_client_destroy_on_exit(client) }) {
        warn!(target: LOG_TARGET, "failed to enable destroy on exit: {error}");
    }

    // Preserve the ignored event-registration result from the C initializer.
    let _ = unsafe {
        esp_websocket_register_events(
            client,
            esp_websocket_event_id_t_WEBSOCKET_EVENT_ANY,
            Some(willow_was_event_handler),
            core::ptr::null_mut(),
        )
    };

    if let Some(error) = EspError::from(unsafe { esp_websocket_client_start(client) }) {
        error!(target: LOG_TARGET, "failed to start WebSocket client: {error}");
        Err(error)
    } else {
        Ok(())
    }
}

/// Initializes the WAS transport from a URL borrowed from retained C startup.
///
/// # Safety
///
/// `url` must point to a valid NUL-terminated string for the duration of this
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_was_init(url: *const c_char) -> esp_err_t {
    if url.is_null() {
        return ESP_ERR_INVALID_ARG;
    }

    let Ok(url) = (unsafe { CStr::from_ptr(url) }).to_str() else {
        return ESP_ERR_INVALID_ARG;
    };

    initialize(url).map_or_else(|error| error.code(), |()| ESP_OK)
}

/// Returns the current raw client handle to the retained C transport users.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_client_handle() -> esp_websocket_client_handle_t {
    client_handle()
}

/// Reports connection state to retained C WAS message handlers.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_is_connected(wait: bool) -> bool {
    is_connected(wait)
}

/// Returns the firmware-lifetime mutex borrowed by the retained C notification
/// tasks.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_notify_mutex() -> SemaphoreHandle_t {
    notification_mutex().unwrap_or(core::ptr::null_mut())
}

/// Wraps a WIS response in the WAS endpoint command envelope.
///
/// # Safety
///
/// `json` must point to a valid NUL-terminated string for the duration of this
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_was_send_endpoint(json: *const c_char) -> esp_err_t {
    if json.is_null() {
        return ESP_ERR_INVALID_ARG;
    }

    let Ok(json) = (unsafe { CStr::from_ptr(json) }).to_str() else {
        return ESP_OK;
    };

    send_endpoint(json).map_or_else(
        |error| error.code(),
        |sent| i32::try_from(sent).unwrap_or(i32::MAX),
    )
}
