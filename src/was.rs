//! Willow Application Server transport ownership.

use std::sync::OnceLock;

use esp_idf_sys::{
    ESP_ERR_NO_MEM, EspError, SemaphoreHandle_t, WILLOW_QUEUE_TYPE_MUTEX, xQueueCreateMutex,
};

struct NotificationMutex(SemaphoreHandle_t);

// The FreeRTOS mutex is designed to be shared by tasks. It lives for the
// firmware lifetime, so its handle remains valid after publication here.
unsafe impl Send for NotificationMutex {}
unsafe impl Sync for NotificationMutex {}

static NOTIFICATION_MUTEX: OnceLock<NotificationMutex> = OnceLock::new();

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

/// Returns the firmware-lifetime mutex borrowed by the retained C notification
/// tasks.
#[unsafe(no_mangle)]
pub extern "C" fn rust_was_notify_mutex() -> SemaphoreHandle_t {
    notification_mutex().unwrap_or(core::ptr::null_mut())
}
