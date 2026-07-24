//! Willow Application Server transport ownership.

mod protocol;

use core::{
    ffi::{c_char, c_void},
    sync::atomic::{AtomicPtr, Ordering},
};
use std::{
    ffi::{CStr, CString},
    sync::OnceLock,
};

use esp_idf_svc::hal::delay::{FreeRtos, TickType};
use esp_idf_sys::{
    CONFIG_FREERTOS_NO_AFFINITY, ESP_ERR_INVALID_ARG, ESP_ERR_NO_MEM, ESP_FAIL, ESP_OK, EspError,
    SemaphoreHandle_t, WAS_RECONNECT_TIMEOUT_MS, WILLOW_QUEUE_TYPE_MUTEX,
    esp_efuse_mac_get_default, esp_err_t, esp_event_base_t, esp_log_level_set,
    esp_log_level_t_ESP_LOG_DEBUG, esp_websocket_client, esp_websocket_client_close,
    esp_websocket_client_config_t, esp_websocket_client_destroy_on_exit,
    esp_websocket_client_handle_t, esp_websocket_client_init, esp_websocket_client_is_connected,
    esp_websocket_client_send_text, esp_websocket_client_start, esp_websocket_client_stop,
    esp_websocket_event_id_t_WEBSOCKET_EVENT_ANY, esp_websocket_register_events, vTaskDelete,
    xQueueCreateMutex, xTaskCreatePinnedToCore,
};
use log::{error, info, warn};
use serde_json::Value;

use crate::{audio, backlight, config, net, state, system, ui};

use self::protocol::{Command, CommandResult, DeviceIdentity, Event, InboundMessage, WakeResult};

const LOG_TARGET: &str = "WILLOW/WAS";
const DEINIT_DELAY_MS: u32 = 2_000;
const DEINIT_TASK_PRIORITY: u32 = 5;
const DEINIT_TASK_STACK_SIZE: u32 = 4_096;
const STOP_TIMEOUT_MS: u64 = 5_000;
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

    let message = serde_json::to_string(&Command::Endpoint { data })
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    send_text(&message).inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to send message to WAS");
    })
}

pub(crate) fn request_config() -> Result<(), EspError> {
    if !is_connected(true) {
        return Ok(());
    }

    let message = serde_json::to_string(&Command::<()>::GetConfig)
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    send_text(&message).map(|_| ()).inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to send WAS get_config message");
    })
}

fn handle_wake_result(result: &WakeResult) {
    let Some(won) = result.won else {
        return;
    };

    if !won {
        info!(target: LOG_TARGET, "lost wake race, stopping pipelines");
    }
    if let Err(source) = audio::multiwake_result(won) {
        error!(target: LOG_TARGET, "failed to apply multiwake result: {source:#?}");
    }
}

fn handle_command_result(result: &CommandResult) {
    let Some(ok) = result.ok else {
        return;
    };

    let speech = result.speech.as_deref().filter(|speech| !speech.is_empty());
    let audio_text = speech.unwrap_or(if ok { "Success" } else { "Error" });
    if let Err(source) = audio::play_response(ok, Some(audio_text)) {
        error!(target: LOG_TARGET, "failed to play command response: {source:#?}");
    }

    if let Some(speech) = speech {
        ui::show_command_result("Response:", speech);
    } else {
        ui::show_command_result("Command status:", if ok { "Success!" } else { "Error" });
    }

    if let Err(error) = backlight::reset_display_timer(false) {
        error!(target: LOG_TARGET, "failed to reset display timer: {error:#?}");
    }
}

fn handle_results(message: &str) -> bool {
    let Ok(message) = serde_json::from_str::<InboundMessage>(message) else {
        return false;
    };

    if let Some(result) = message.wake_result.as_ref() {
        handle_wake_result(result);
        true
    } else if let Some(result) = message.result.as_ref() {
        handle_command_result(result);
        true
    } else {
        false
    }
}

fn multiwake_enabled() -> bool {
    config::config()
        .and_then(|config| config.multiwake)
        .unwrap_or(false)
}

#[derive(Clone, Copy)]
enum ConnectionAnnouncement {
    Goodbye,
    Hello,
}

impl ConnectionAnnouncement {
    const fn name(self) -> &'static str {
        match self {
            Self::Goodbye => "goodbye",
            Self::Hello => "hello",
        }
    }

    fn event(self, identity: DeviceIdentity) -> Event {
        match self {
            Self::Goodbye => Event::Goodbye(identity),
            Self::Hello => Event::Hello(identity),
        }
    }
}

fn send_connection_announcement(announcement: ConnectionAnnouncement) -> Result<(), EspError> {
    let name = announcement.name();
    info!(target: LOG_TARGET, "sending WAS {name}");

    if !is_connected(true) {
        return Ok(());
    }

    let hostname = net::hostname().inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to get hostname");
    })?;

    let mut mac_addr = [0; 6];
    if let Some(error) = EspError::from(unsafe { esp_efuse_mac_get_default(mac_addr.as_mut_ptr()) })
    {
        error!(target: LOG_TARGET, "failed to get MAC address from EFUSE");
        return Err(error);
    }

    let identity = DeviceIdentity::new(hostname, system::hardware().name(), mac_addr);
    let message = serde_json::to_string(&announcement.event(identity))
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    send_text(&message).map(|_| ()).inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to send WAS {name} message");
    })
}

pub(crate) fn send_goodbye() -> Result<(), EspError> {
    send_connection_announcement(ConnectionAnnouncement::Goodbye)
}

pub(crate) fn send_hello() -> Result<(), EspError> {
    send_connection_announcement(ConnectionAnnouncement::Hello)
}

fn stop() {
    let client = client_handle();
    info!(target: LOG_TARGET, "stopping WebSocket client");

    if EspError::from(unsafe {
        esp_websocket_client_close(client, TickType::new_millis(STOP_TIMEOUT_MS).0)
    })
    .is_some()
    {
        error!(target: LOG_TARGET, "failed to cleanly close WebSocket client");

        if let Some(error) = EspError::from(unsafe { esp_websocket_client_stop(client) }) {
            error!(target: LOG_TARGET, "failed to stop WebSocket client: {error}");
        }
    }
}

unsafe extern "C" fn deinit_task(_data: *mut c_void) {
    stop();
    unsafe {
        vTaskDelete(core::ptr::null_mut());
    }
}

pub(crate) fn deinitialize() {
    state::mark_restarting();
    let _ = send_goodbye();

    // The client cannot be stopped from its event-handler task. Preserve the
    // old unpinned FreeRTOS task, including its stack size and priority.
    let _ = unsafe {
        xTaskCreatePinnedToCore(
            Some(deinit_task),
            c"was_deinit_task".as_ptr(),
            DEINIT_TASK_STACK_SIZE,
            core::ptr::null_mut(),
            DEINIT_TASK_PRIORITY,
            core::ptr::null_mut(),
            CONFIG_FREERTOS_NO_AFFINITY as i32,
        )
    };

    info!(target: LOG_TARGET, "Delay for was_deinit_task");
    FreeRtos::delay_ms(DEINIT_DELAY_MS);
}

pub(crate) fn send_wake_end() -> Result<(), EspError> {
    if !multiwake_enabled() {
        return Ok(());
    }

    if !is_connected(false) {
        warn!(target: LOG_TARGET, "Websocket not connected - skipping wake end");
        return Ok(());
    }

    let message = serde_json::to_string(&Event::WakeEnd {})
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    send_text(&message).map(|_| ()).inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to send WAS wake_end message");
    })
}

pub(crate) fn send_wake_start(wake_volume: f32) -> Result<(), EspError> {
    if !multiwake_enabled() {
        return Ok(());
    }

    if !is_connected(false) {
        warn!(target: LOG_TARGET, "Websocket not connected - skipping wake start");
        return Ok(());
    }

    let message = serde_json::to_string(&Event::WakeStart { wake_volume })
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())?;

    send_text(&message).map(|_| ()).inspect_err(|_| {
        error!(target: LOG_TARGET, "failed to send WAS wake_start message");
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

/// Handles result messages before the retained C parser sees other messages.
///
/// # Safety
///
/// `message` must either be null or point to a valid NUL-terminated string for
/// the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_was_handle_results(message: *const c_char) -> bool {
    if message.is_null() {
        return false;
    }

    let Ok(message) = (unsafe { CStr::from_ptr(message) }).to_str() else {
        return false;
    };
    handle_results(message)
}

/// Returns the firmware-lifetime mutex borrowed by the retained C notification
/// tasks.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_notify_mutex() -> SemaphoreHandle_t {
    notification_mutex().unwrap_or(core::ptr::null_mut())
}

/// Requests configuration after the retained C event handler connects.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_request_config() {
    let _ = request_config();
}

/// Stops WAS on behalf of retained C restart and OTA paths.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_deinit() {
    deinitialize();
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

/// Sends a wake-end event from the retained C recorder callback.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_send_wake_end() {
    let _ = send_wake_end();
}

/// Sends a wake-start event from the retained C recorder callback.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_send_wake_start(wake_volume: f32) {
    let _ = send_wake_start(wake_volume);
}

/// Sends the device identity after the retained C event handler connects.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_send_hello() {
    let _ = send_hello();
}
