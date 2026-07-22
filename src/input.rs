//! Mute input ownership and long-release detection.

use std::{
    sync::{Mutex, OnceLock, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{AnyInputPin, Input, PinDriver, Pull},
};
use esp_idf_sys::{
    ESP_ERR_INVALID_STATE, ESP_FAIL, ESP_OK, EspError, esp_err_t, gpio_num_t_GPIO_NUM_1,
};
use log::{debug, error, info};

const INPUT_MONITOR_STACK_SIZE: usize = 3_072;
const LONG_PRESS_DURATION: Duration = Duration::from_secs(2);
const LOG_TARGET: &str = "WILLOW/INPUT";
const POLL_INTERVAL_MS: u32 = 50;

static MUTE_INPUT: OnceLock<Mutex<PinDriver<'static, Input>>> = OnceLock::new();
static INPUT_MONITOR: OnceLock<JoinHandle<()>> = OnceLock::new();

/// Configures GPIO1 as the active-low mute input and retains its driver.
pub fn init() -> Result<(), EspError> {
    if MUTE_INPUT.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    debug!(target: LOG_TARGET, "initializing mute input on GPIO1 in Rust");

    // GPIO1 is not represented elsewhere in Rust and the old ADF button
    // peripheral is removed by this migration, so this becomes its sole owner.
    let pin = unsafe { AnyInputPin::steal(gpio_num_t_GPIO_NUM_1 as u8) };
    let input = PinDriver::input(pin, Pull::Up)?;

    MUTE_INPUT
        .set(Mutex::new(input))
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

fn is_muted() -> Option<bool> {
    let input = MUTE_INPUT.get()?;
    let input = input.lock().unwrap_or_else(PoisonError::into_inner);

    Some(input.is_low())
}

/// Reports whether the hardware mute input is currently active.
#[unsafe(no_mangle)]
pub extern "C" fn rust_input_is_muted() -> bool {
    is_muted().unwrap_or_else(|| {
        error!(target: LOG_TARGET, "mute input is not initialized");
        false
    })
}

fn wait_for_unmute() -> bool {
    let mut pressed_at = None;

    loop {
        let Some(muted) = is_muted() else {
            error!(target: LOG_TARGET, "mute input is not initialized");
            return false;
        };

        if muted {
            pressed_at.get_or_insert_with(Instant::now);
        } else if let Some(started) = pressed_at.take()
            && started.elapsed() > LONG_PRESS_DURATION
        {
            return true;
        }

        FreeRtos::delay_ms(POLL_INTERVAL_MS);
    }
}

fn monitor(unmute_event: i32) {
    while wait_for_unmute() {
        info!(target: LOG_TARGET, "unmute");
        if let Err(error) = crate::audio::send_recorder_event(unmute_event, u32::MAX) {
            error!(target: LOG_TARGET, "failed to send unmute event: {error}");
            return;
        }
    }

    error!(target: LOG_TARGET, "mute input monitor stopped");
}

fn start_monitor(unmute_event: i32) -> Result<(), EspError> {
    if INPUT_MONITOR.get().is_some() {
        return Ok(());
    }
    if MUTE_INPUT.get().is_none() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let monitor = thread::Builder::new()
        .name("mute_input".into())
        .stack_size(INPUT_MONITOR_STACK_SIZE)
        .spawn(move || self::monitor(unmute_event))
        .map_err(|error| {
            error!(target: LOG_TARGET, "failed to start mute input monitor: {error}");
            EspError::from_infallible::<ESP_FAIL>()
        })?;

    INPUT_MONITOR
        .set(monitor)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

/// Starts the Rust task that publishes long-release events to the audio queue.
#[unsafe(no_mangle)]
pub extern "C" fn rust_input_monitor_start(unmute_event: i32) -> esp_err_t {
    match start_monitor(unmute_event) {
        Ok(()) => ESP_OK,
        Err(error) => error.code(),
    }
}
