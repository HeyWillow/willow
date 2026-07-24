//! Mute input ownership and long-release detection.

use std::{
    io,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use esp_idf_svc::hal::{
    delay::FreeRtos,
    gpio::{AnyInputPin, Input, PinDriver, Pull},
};
use esp_idf_sys::{ESP_ERR_INVALID_STATE, EspError, gpio_num_t_GPIO_NUM_1};
use log::{debug, error, info};

const INPUT_MONITOR_STACK_SIZE: usize = 3_072;
const LONG_PRESS_DURATION: Duration = Duration::from_secs(2);
const LOG_TARGET: &str = "WILLOW/INPUT";
const POLL_INTERVAL_MS: u32 = 50;

static MUTE_INPUT: OnceLock<Mutex<PinDriver<'static, Input>>> = OnceLock::new();

/// Owned long-release monitor used after Rust takes over runtime audio.
pub(crate) struct UnmuteMonitor {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for UnmuteMonitor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            error!(target: LOG_TARGET, "mute input monitor panicked during shutdown");
        }
    }
}

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

/// Reads the active-low hardware mute input.
pub(crate) fn is_muted() -> Result<bool, EspError> {
    let input = MUTE_INPUT
        .get()
        .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
    let input = input.lock().unwrap_or_else(PoisonError::into_inner);

    Ok(input.is_low())
}

/// Waits at the legacy one-second cadence while the startup mute is active.
///
/// The return value reports whether startup actually had to wait.
pub(crate) fn wait_until_unmuted() -> Result<bool, EspError> {
    let waited = is_muted()?;
    while is_muted()? {
        FreeRtos::delay_ms(1_000);
    }
    Ok(waited)
}

fn wait_for_unmute(shutdown: &AtomicBool) -> bool {
    let mut pressed_at = None;

    while !shutdown.load(Ordering::Acquire) {
        let muted = match is_muted() {
            Ok(muted) => muted,
            Err(source) => {
                error!(target: LOG_TARGET, "cannot read mute input: {source:#?}");
                return false;
            }
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
    false
}

/// Starts an owned monitor which invokes `unmuted` after a long mute release.
pub(crate) fn start_unmute_monitor(
    mut unmuted: impl FnMut() + Send + 'static,
) -> Result<UnmuteMonitor, io::Error> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let monitor_shutdown = Arc::clone(&shutdown);
    let thread = thread::Builder::new()
        .name("mute_input".into())
        .stack_size(INPUT_MONITOR_STACK_SIZE)
        .spawn(move || {
            while wait_for_unmute(&monitor_shutdown) {
                info!(target: LOG_TARGET, "unmute");
                unmuted();
            }
        })?;

    Ok(UnmuteMonitor {
        shutdown,
        thread: Some(thread),
    })
}
