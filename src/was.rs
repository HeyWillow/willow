//! Willow Application Server transport ownership.

mod notification;

use core::{
    ffi::{c_char, c_void},
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};
use std::{
    ffi::{CStr, CString},
    sync::{Arc, Mutex},
};

use esp_idf_svc::hal::delay::{FreeRtos, TickType};
use esp_idf_sys::{
    CONFIG_FREERTOS_NO_AFFINITY, ESP_ERR_INVALID_ARG, ESP_FAIL, ESP_OK, EspError,
    WAS_RECONNECT_TIMEOUT_MS, esp_efuse_mac_get_default, esp_err_t, esp_event_base_t,
    esp_log_level_set, esp_log_level_t_ESP_LOG_DEBUG, esp_websocket_client,
    esp_websocket_client_close, esp_websocket_client_config_t,
    esp_websocket_client_destroy_on_exit, esp_websocket_client_handle_t, esp_websocket_client_init,
    esp_websocket_client_is_connected, esp_websocket_client_send_text, esp_websocket_client_start,
    esp_websocket_client_stop, esp_websocket_event_id_t_WEBSOCKET_EVENT_ANY,
    esp_websocket_register_events, vTaskDelete, xTaskCreatePinnedToCore,
};
use log::{error, info, warn};
use willow_protocol::{
    was::v1::{
        Command, CommandResult, DeviceIdentity, Event, InboundCommand, InboundMessage,
        Notification, WakeResult,
    },
    wis::v1::SpeechToTextResponse,
};

use self::notification::{CancelOutcome, NotificationLease, NotificationState};
use crate::{audio, backlight, config, net, nvs, ota, state, system, ui};

const LOG_TARGET: &str = "WILLOW/WAS";
const DEINIT_DELAY_MS: u32 = 2_000;
const DEINIT_TASK_PRIORITY: u32 = 5;
const DEINIT_TASK_STACK_SIZE: u32 = 4_096;
const IDENTIFY_AUDIO_URL: &str = "spiffs://spiffs/user/audio/success.wav";
const IDENTIFY_TEXT: &str = "WAS Locate Active!";
const NOTIFICATION_TASK_CORE: i32 = 0;
const NOTIFICATION_TASK_PRIORITY: u32 = 4;
const NOTIFICATION_TASK_STACK_SIZE: u32 = 4_096;
const NOTIFICATION_DEFAULT_VOLUME: i32 = 90;
const NOTIFICATION_PLAYBACK_DELAY_MS: u32 = 1_000;
const STOP_TIMEOUT_MS: u64 = 5_000;
const TASK_CREATED: i32 = 1;
const USER_AGENT: &str = concat!("Willow/", env!("WILLOW_VERSION"));

static CLIENT: AtomicPtr<esp_websocket_client> = AtomicPtr::new(core::ptr::null_mut());

struct NotificationJob {
    audio_url: Option<String>,
    backlight: bool,
    backlight_max: bool,
    cancel: Arc<AtomicBool>,
    id: u64,
    repeat: i32,
    strobe_period_ms: i32,
    text: Option<String>,
    volume: i32,
}

struct NotificationTask {
    job: NotificationJob,
    lease: NotificationLease,
}

static NOTIFICATION_RUN: Mutex<()> = Mutex::new(());
static NOTIFICATION_STATE: NotificationState = NotificationState::new();

unsafe extern "C" {
    fn willow_was_event_handler(
        arg: *mut c_void,
        event_base: esp_event_base_t,
        event_id: i32,
        event_data: *mut c_void,
    );
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

pub(crate) fn send_endpoint(data: &SpeechToTextResponse) -> Result<usize, EspError> {
    // The old nc_skip argument was only ever false, so a failed connection
    // check still reports the UI error and then attempts the send.
    let _ = is_connected(true);

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

fn send_notification_done(id: u64) {
    if id == 1 || !is_connected(true) {
        return;
    }

    let message = match serde_json::to_string(&Event::NotifyDone(id)) {
        Ok(message) => message,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to serialize notify_done: {error:#?}");
            return;
        }
    };
    if let Err(error) = send_text(&message) {
        error!(target: LOG_TARGET, "failed to send WAS notify_done message: {error:#?}");
    }
}

fn run_notification(job: &NotificationJob) {
    info!(
        target: LOG_TARGET,
        "started notify task for notification with ID='{}'",
        job.id
    );
    ui::show_notification(job.text.as_deref());
    if let Err(error) = backlight::reset_display_timer(true) {
        error!(target: LOG_TARGET, "failed to pause display timer: {error:#?}");
    }
    backlight::set(job.backlight, job.backlight_max);

    let strobe_started = if job.strobe_period_ms > 0 {
        match u32::try_from(job.strobe_period_ms)
            .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_ARG>())
            .and_then(backlight::start_strobe)
        {
            Ok(()) => true,
            Err(error) => {
                error!(target: LOG_TARGET, "failed to start display backlight strobe: {error:#?}");
                false
            }
        }
    } else {
        false
    };

    if let Some(url) = job.audio_url.as_deref() {
        if let Err(source) = audio::set_volume(Some(job.volume)) {
            error!(target: LOG_TARGET, "failed to set notification volume: {source:#?}");
        }

        for _ in 0..job.repeat {
            if job.cancel.load(Ordering::Acquire) || ui::notification_cancelled() {
                break;
            }
            if let Err(source) = audio::play_sync(url) {
                error!(target: LOG_TARGET, "failed to play notification audio: {source:#?}");
            }
            FreeRtos::delay_ms(NOTIFICATION_PLAYBACK_DELAY_MS);
        }

        if let Err(source) = audio::set_volume(None) {
            error!(target: LOG_TARGET, "failed to restore configured volume: {source:#?}");
        }
    }

    ui::notification_end();
    if let Err(error) = backlight::reset_display_timer(false) {
        error!(target: LOG_TARGET, "failed to reset display timer: {error:#?}");
    }
    if strobe_started {
        backlight::stop_strobe();
    }
}

unsafe extern "C" fn notification_task(data: *mut c_void) {
    // SAFETY: `start_notification` transfers exactly one boxed task to a
    // successfully created FreeRTOS task and recovers it on creation failure.
    let task = unsafe { Box::from_raw(data.cast::<NotificationTask>()) };
    let run = match NOTIFICATION_RUN.lock() {
        Ok(run) => run,
        Err(poisoned) => poisoned.into_inner(),
    };

    if !task.job.cancel.load(Ordering::Acquire) {
        run_notification(&task.job);
    }

    send_notification_done(task.job.id);
    NOTIFICATION_STATE.clear(task.lease);
    drop(task);
    // `vTaskDelete` does not unwind this Rust stack, so release the execution
    // mutex explicitly before deleting the current task.
    drop(run);
    unsafe { vTaskDelete(core::ptr::null_mut()) };
}

fn start_notification(job: NotificationJob) -> Result<(), EspError> {
    let activation = NOTIFICATION_STATE.activate(job.id, Arc::clone(&job.cancel));
    if activation.replaced {
        if let Err(source) = audio::cancel_playback() {
            error!(target: LOG_TARGET, "failed to cancel replaced notification playback: {source:?}");
        }
    }

    let task = Box::into_raw(Box::new(NotificationTask {
        job,
        lease: activation.lease,
    }));
    let status = unsafe {
        xTaskCreatePinnedToCore(
            Some(notification_task),
            c"notify_task".as_ptr(),
            NOTIFICATION_TASK_STACK_SIZE,
            task.cast(),
            NOTIFICATION_TASK_PRIORITY,
            core::ptr::null_mut(),
            NOTIFICATION_TASK_CORE,
        )
    };
    if status == TASK_CREATED {
        Ok(())
    } else {
        unsafe { drop(Box::from_raw(task)) };
        NOTIFICATION_STATE.clear(activation.lease);
        Err(EspError::from_infallible::<ESP_FAIL>())
    }
}

fn cancel_notification(id: u64) -> bool {
    match NOTIFICATION_STATE.cancel(id) {
        CancelOutcome::NoActiveNotification => {
            warn!(target: LOG_TARGET, "trying to cancel notify_task but no notification is active");
            true
        }
        CancelOutcome::DifferentId => false,
        CancelOutcome::Cancelled => {
            info!(target: LOG_TARGET, "cancel active notify_task with ID='{id}'");
            if let Err(source) = audio::cancel_playback() {
                error!(target: LOG_TARGET, "failed to cancel notification playback: {source:#?}");
            }
            true
        }
    }
}

fn handle_notification(notification: Option<Notification>) {
    let Some(notification) = notification else {
        return;
    };
    let Some(id) = notification.id else {
        warn!(target: LOG_TARGET, "ignoring notification without ID");
        return;
    };

    if notification.cancel == Some(true) && cancel_notification(id) {
        return;
    }

    if let Some(url) = notification.audio_url.as_deref() {
        info!(target: LOG_TARGET, "audio URL in notify command: {url}");
    }
    if let Some(text) = notification.text.as_deref() {
        info!(target: LOG_TARGET, "text in notify command: {text}");
    }

    let job = NotificationJob {
        audio_url: notification.audio_url,
        backlight: notification.backlight.unwrap_or(true),
        backlight_max: notification.backlight_max.unwrap_or(true),
        cancel: Arc::new(AtomicBool::new(false)),
        id,
        repeat: notification.repeat.unwrap_or(1),
        strobe_period_ms: notification.strobe_period_ms.unwrap_or(0),
        text: notification.text,
        volume: notification.volume.unwrap_or(NOTIFICATION_DEFAULT_VOLUME),
    };
    if let Err(error) = start_notification(job) {
        error!(target: LOG_TARGET, "failed to start notification task: {error:#?}");
    }
}

fn start_identify() {
    let job = NotificationJob {
        audio_url: Some(IDENTIFY_AUDIO_URL.to_owned()),
        backlight: true,
        backlight_max: true,
        cancel: Arc::new(AtomicBool::new(false)),
        id: 1,
        repeat: 5,
        strobe_period_ms: 0,
        text: Some(IDENTIFY_TEXT.to_owned()),
        volume: NOTIFICATION_DEFAULT_VOLUME,
    };
    if let Err(error) = start_notification(job) {
        error!(target: LOG_TARGET, "failed to start identify task: {error:#?}");
    }
}

fn handle_message(message: &str) {
    let Ok(message) = serde_json::from_str::<InboundMessage>(message) else {
        return;
    };

    if let Some(result) = message.wake_result.as_ref() {
        handle_wake_result(result);
    } else if let Some(result) = message.result.as_ref() {
        handle_command_result(result);
    } else if let Some(document) = message.config {
        let document = match serde_json::to_vec_pretty(&document) {
            Ok(document) => document,
            Err(error) => {
                error!(target: LOG_TARGET, "failed to serialize configuration: {error:#?}");
                return;
            }
        };
        info!(
            target: LOG_TARGET,
            "found config in WebSocket message: {}",
            String::from_utf8_lossy(&document)
        );
        config::replace(&document)
    } else if let Some(document) = message.nvs {
        info!(target: LOG_TARGET, "found NVS provisioning document in WebSocket message");
        let document = match serde_json::to_vec(&document) {
            Ok(document) => document,
            Err(error) => {
                error!(target: LOG_TARGET, "failed to serialize NVS document: {error:#?}");
                return;
            }
        };
        if let Err(error) = nvs::apply_document(&document) {
            error!(target: LOG_TARGET, "failed to apply NVS document: {error:#?}");
            return;
        }

        info!(target: LOG_TARGET, "restarting to apply NVS changes");
        ui::show_center_message("Connectivity Updated");
        if let Err(error) = backlight::reset_display_timer(true) {
            error!(target: LOG_TARGET, "failed to pause display timer: {error:#?}");
        }
        backlight::set(true, false);
        deinitialize();
        system::restart_delayed()
    } else {
        match message.command {
            Some(InboundCommand::OtaStart) => {
                info!(target: LOG_TARGET, "found command in WebSocket message: ota_start");
                if let Some(url) = message.ota_url.as_deref() {
                    info!(target: LOG_TARGET, "OTA URL: {url}");
                    if let Err(error) = ota::start(url) {
                        error!(target: LOG_TARGET, "failed to start OTA task: {error:#?}");
                    }
                }
            }
            Some(InboundCommand::Restart) => {
                info!(target: LOG_TARGET, "found command in WebSocket message: restart");
                info!(target: LOG_TARGET, "restart command received. restart");
                ui::show_center_message("WAS Restart");
                backlight::set(true, false);
                deinitialize();
                system::restart_delayed()
            }
            Some(InboundCommand::Notify) => {
                info!(target: LOG_TARGET, "found command in WebSocket message: notify");
                info!(target: LOG_TARGET, "received notify command");
                handle_notification(message.notification);
            }
            Some(InboundCommand::Identify) => {
                info!(target: LOG_TARGET, "found command in WebSocket message: identify");
                info!(target: LOG_TARGET, "received identify command");
                start_identify();
            }
            Some(InboundCommand::Unknown(command)) => {
                info!(target: LOG_TARGET, "found command in WebSocket message: {command}");
            }
            None => {}
        }
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

/// Handles a complete text message for the retained C event callback.
///
/// # Safety
///
/// `message` must either be null or point to a valid NUL-terminated string for
/// the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_was_handle_message(message: *const c_char) {
    if message.is_null() {
        return;
    }

    let Ok(message) = (unsafe { CStr::from_ptr(message) }).to_str() else {
        return;
    };
    handle_message(message);
}

/// Requests configuration after the retained C event handler connects.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_request_config() {
    let _ = request_config();
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
    let Ok(response) = serde_json::from_str::<SpeechToTextResponse>(json) else {
        return ESP_OK;
    };

    send_endpoint(&response).map_or_else(
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
