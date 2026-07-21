//! Periodic FreeRTOS task runtime reporting.

use core::{ffi::c_void, ptr};

use esp_idf_svc::hal::{delay::FreeRtos, task};
use esp_idf_sys::EspError;

const INTERVAL_MS: u32 = 5_000;
const STACK_SIZE: usize = 4 * 1_024;
const STATS_BUFFER_SIZE: usize = 2_048;
const TASK_PRIORITY: u8 = 0;

extern "C" fn runtime_stats_task(_argument: *mut c_void) {
    loop {
        let mut buffer = [0; STATS_BUFFER_SIZE];

        // FreeRTOS provides no buffer length to this formatting function. The
        // fixed buffer deliberately matches the previous C task.
        unsafe {
            esp_idf_sys::vTaskGetRunTimeStats(buffer.as_mut_ptr());
            esp_idf_sys::printf(c"%s\n".as_ptr(), buffer.as_ptr());
        }

        FreeRtos::delay_ms(INTERVAL_MS);
    }
}

/// Starts periodic FreeRTOS runtime reporting.
pub(crate) fn start() -> Result<(), EspError> {
    // The standard Rust thread configuration rejects priority zero. Use the
    // raw FreeRTOS task wrapper to retain the existing idle-priority task.
    unsafe {
        task::create(
            runtime_stats_task,
            c"dbg_runtime_stats",
            STACK_SIZE,
            ptr::null_mut(),
            TASK_PRIORITY,
            None,
        )?;
    }

    Ok(())
}
