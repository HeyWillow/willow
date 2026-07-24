//! Rust audio ownership and migration support.
//!
//! Coordination resources serve the retained C engine while the inactive
//! capture modules prepare the atomic Rust audio cut-over.

mod board;
mod capture;
mod codec_ffi;
mod codecs;
mod es7210;
mod http_audio;
mod http_chunk;
mod http_playback;
mod i2s;
mod ogg_headers;
mod pcm;
mod playback;
mod player;
mod record_buffer;
mod record_upload;
mod recorder;
mod recorder_credit;
mod recorder_state;
mod response;
mod response_config;
mod spiffs_playback;
mod spiffs_uri;
mod stream_codec;
mod wis_config;
mod wis_encoder;
mod wis_framing;
mod wis_upload;

use core::{ffi::c_void, ptr::NonNull};
use std::{
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use esp_idf_svc::hal::task::queue::Queue;
use esp_idf_svc::timer::{EspTaskTimerService, EspTimer};
use esp_idf_sys::{
    ESP_ERR_INVALID_ARG, ESP_ERR_INVALID_STATE, ESP_ERR_NO_MEM, ESP_OK, EspError, QueueHandle_t,
    audio_rec_handle_t, audio_recorder_trigger_stop, esp_err_t, q_msg_MSG_STOP as MSG_STOP,
};
use log::{error, info};

const RECORDER_QUEUE_CAPACITY: usize = 3;
const SESSION_TIMER_LOG_TARGET: &str = "WILLOW/TIMER";

struct SessionTimer {
    armed: Arc<AtomicBool>,
    recorder: Arc<AtomicUsize>,
    timer: Mutex<EspTimer<'static>>,
}

static RECORDER_QUEUE: OnceLock<Queue<i32>> = OnceLock::new();
static RECORDING: AtomicBool = AtomicBool::new(false);
static SESSION_TIMER: OnceLock<SessionTimer> = OnceLock::new();

/// Creates and retains the recorder event queue for the firmware lifetime.
pub(crate) fn initialize_recorder_queue() -> Result<(), EspError> {
    if RECORDER_QUEUE.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let recorder_queue = Queue::new(RECORDER_QUEUE_CAPACITY);
    if recorder_queue.as_raw().is_null() {
        return Err(EspError::from_infallible::<ESP_ERR_NO_MEM>());
    }

    RECORDER_QUEUE
        .set(recorder_queue)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

/// Creates and retains the audio session timeout for the firmware lifetime.
pub(crate) fn initialize_session_timer() -> Result<(), EspError> {
    if SESSION_TIMER.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let armed = Arc::new(AtomicBool::new(false));
    let recorder = Arc::new(AtomicUsize::new(0));
    let timer = {
        let armed = Arc::clone(&armed);
        let recorder = Arc::clone(&recorder);
        EspTaskTimerService::new()?.timer(move || {
            if !armed.swap(false, Ordering::AcqRel) {
                return;
            }

            let recorder = recorder.swap(0, Ordering::AcqRel);
            if recorder == 0 || !is_recording() {
                return;
            }

            info!(target: SESSION_TIMER_LOG_TARGET, "session timer expired - forcing end stream");
            let _ = unsafe { audio_recorder_trigger_stop(recorder as audio_rec_handle_t) };
            if let Err(error) = send_stop_event() {
                error!(target: SESSION_TIMER_LOG_TARGET, "failed to send recorder stop event: {error}");
            }
        })?
    };

    SESSION_TIMER
        .set(SessionTimer {
            armed,
            recorder,
            timer: Mutex::new(timer),
        })
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

pub(crate) fn cancel_session_timeout() -> Result<(), EspError> {
    let session_timer = SESSION_TIMER
        .get()
        .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
    session_timer.armed.store(false, Ordering::Release);
    session_timer.recorder.store(0, Ordering::Release);
    session_timer
        .timer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .cancel()
        .map(|_| ())
}

/// Arms the session timeout with a borrowed recorder handle.
///
/// # Safety
///
/// `recorder` must remain valid until the timeout fires or is canceled.
pub(crate) unsafe fn schedule_session_timeout(
    recorder: NonNull<c_void>,
    timeout: Duration,
) -> Result<(), EspError> {
    let session_timer = SESSION_TIMER
        .get()
        .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
    let timer = session_timer
        .timer
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    session_timer.armed.store(false, Ordering::Release);
    session_timer.recorder.store(0, Ordering::Release);
    let _ = timer.cancel()?;

    session_timer
        .recorder
        .store(recorder.as_ptr() as usize, Ordering::Release);
    session_timer.armed.store(true, Ordering::Release);
    if let Err(error) = timer.after(timeout) {
        session_timer.armed.store(false, Ordering::Release);
        session_timer.recorder.store(0, Ordering::Release);
        return Err(error);
    }

    Ok(())
}

pub(crate) fn send_recorder_event(event: i32, timeout: u32) -> Result<(), EspError> {
    let recorder_queue = RECORDER_QUEUE
        .get()
        .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
    recorder_queue.send_back(event, timeout).map(|_| ())
}

/// Queues a recorder stop without blocking when the queue is full.
pub(crate) fn send_stop_event() -> Result<(), EspError> {
    send_recorder_event(MSG_STOP as i32, 0)
}

pub(crate) fn is_recording() -> bool {
    RECORDING.load(Ordering::Acquire)
}

pub(crate) fn set_recording(recording: bool) {
    RECORDING.store(recording, Ordering::Release);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(value) => value,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Returns the Rust-owned recorder queue as a borrowed FreeRTOS handle.
#[unsafe(no_mangle)]
pub extern "C" fn rust_audio_recorder_queue_handle() -> QueueHandle_t {
    RECORDER_QUEUE.get().map(Queue::as_raw).unwrap_or_default()
}

/// Returns the Rust-owned recorder state for retained C callers.
#[unsafe(no_mangle)]
pub extern "C" fn rust_audio_is_recording() -> bool {
    is_recording()
}

/// Cancels the Rust-owned audio session timeout for retained C callers.
#[unsafe(no_mangle)]
pub extern "C" fn rust_audio_session_timer_cancel() -> esp_err_t {
    match cancel_session_timeout() {
        Ok(()) => ESP_OK,
        Err(error) => error.code(),
    }
}

/// Converts the borrowed C recorder handle for the Rust session timer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_audio_session_timer_reset(
    recorder: *mut c_void,
    timeout_secs: u32,
) -> esp_err_t {
    let Some(recorder) = NonNull::new(recorder) else {
        return ESP_ERR_INVALID_ARG;
    };

    match unsafe { schedule_session_timeout(recorder, Duration::from_secs(timeout_secs.into())) } {
        Ok(()) => ESP_OK,
        Err(error) => error.code(),
    }
}

/// Updates the Rust-owned recorder state for retained C callers.
#[unsafe(no_mangle)]
pub extern "C" fn rust_audio_set_recording(recording: bool) {
    set_recording(recording);
}
