//! Rust-owned coordination resources for the retained C audio engine.

use core::ptr;
use std::sync::OnceLock;

use esp_idf_svc::hal::task::queue::Queue;
use esp_idf_sys::{ESP_ERR_INVALID_STATE, ESP_ERR_NO_MEM, EspError, QueueHandle_t};

const RECORDER_QUEUE_CAPACITY: usize = 3;

static RECORDER_QUEUE: OnceLock<Queue<i32>> = OnceLock::new();

/// Creates and retains the recorder event queue for the firmware lifetime.
pub(crate) fn init() -> Result<(), EspError> {
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

pub(crate) fn send_recorder_event(event: i32) -> Result<(), EspError> {
    let recorder_queue = RECORDER_QUEUE
        .get()
        .ok_or_else(|| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())?;
    recorder_queue.send_back(event, u32::MAX).map(|_| ())
}

/// Returns the Rust-owned recorder queue as a borrowed FreeRTOS handle.
#[unsafe(no_mangle)]
pub extern "C" fn rust_audio_recorder_queue_handle() -> QueueHandle_t {
    RECORDER_QUEUE
        .get()
        .map(Queue::as_raw)
        .unwrap_or(ptr::null_mut())
}
