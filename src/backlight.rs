//! LCD backlight PWM, strobe, and timeout ownership with a temporary C API.

use std::{
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use esp_idf_svc::hal::{delay::BLOCK, task::queue::Queue};
use esp_idf_sys::{
    ESP_ERR_INVALID_ARG, ESP_ERR_INVALID_STATE, ESP_FAIL, EspError, esp_err_t,
    gpio_num_t_GPIO_NUM_45, gpio_num_t_GPIO_NUM_47, ledc_channel_config, ledc_channel_config_t,
    ledc_channel_t_LEDC_CHANNEL_1, ledc_fade_func_install, ledc_intr_type_t_LEDC_INTR_DISABLE,
    ledc_mode_t_LEDC_LOW_SPEED_MODE, ledc_set_duty_and_update, ledc_timer_bit_t_LEDC_TIMER_10_BIT,
    ledc_timer_config, ledc_timer_config_t, ledc_timer_t_LEDC_TIMER_1,
    soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK,
};
use log::{debug, error, info};

const DEFAULT_BRIGHTNESS: u32 = 700;
const DEFAULT_DISPLAY_TIMEOUT_SECS: u32 = 10;
const DISPLAY_TIMER_STACK_SIZE: usize = 4_096;
const FREQUENCY_HZ: u32 = 5_000;
const LOG_TARGET: &str = "WILLOW/DISPLAY";
const MAX_DUTY: u32 = 1_023;
const MIN_STROBE_PERIOD_MS: u32 = 20;
const STROBE_STACK_SIZE: usize = 3_072;
const TIMER_LOG_TARGET: &str = "WILLOW/TIMER";

const BACKLIGHT_GPIO: i32 = if cfg!(esp_idf_esp32_s3_box_3_board) {
    gpio_num_t_GPIO_NUM_47
} else {
    gpio_num_t_GPIO_NUM_45
};
const ACTIVE_LOW: bool = cfg!(esp_idf_esp32_s3_box_lite_board);

struct Backlight {
    maximum: u32,
    off: u32,
    on: u32,
}

struct Strobe {
    stop: SyncSender<()>,
    thread: JoinHandle<()>,
}

enum DisplayTimerAction {
    Pause,
    Schedule(Duration),
}

struct DisplayTimer {
    command: Sender<DisplayTimerAction>,
    acknowledgement: Arc<Queue<u8>>,
    reset: Mutex<()>,
    _thread: JoinHandle<()>,
}

static BACKLIGHT: OnceLock<Backlight> = OnceLock::new();
static DISPLAY_TIMER: OnceLock<DisplayTimer> = OnceLock::new();
static STROBE: Mutex<Option<Strobe>> = Mutex::new(None);

fn check(result: esp_err_t, operation: &str) -> Result<(), EspError> {
    if let Some(error) = EspError::from(result) {
        error!(target: LOG_TARGET, "{operation}: {error}");
        Err(error)
    } else {
        Ok(())
    }
}

fn duties(brightness: u32) -> Result<Backlight, EspError> {
    if brightness > MAX_DUTY {
        error!(
            target: LOG_TARGET,
            "LCD brightness {brightness} exceeds maximum duty {MAX_DUTY}"
        );
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_ARG>());
    }

    Ok(if ACTIVE_LOW {
        Backlight {
            maximum: 0,
            off: MAX_DUTY,
            on: MAX_DUTY - brightness,
        }
    } else {
        Backlight {
            maximum: MAX_DUTY,
            off: 0,
            on: brightness,
        }
    })
}

pub(crate) fn initialize() -> Result<(), EspError> {
    if BACKLIGHT.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let brightness = crate::config::config()
        .and_then(|configuration| configuration.lcd_brightness)
        .map(u32::from)
        .unwrap_or(DEFAULT_BRIGHTNESS);
    let backlight = duties(brightness)?;

    debug!(
        target: LOG_TARGET,
        "backlight GPIO={BACKLIGHT_GPIO} on={} off={} maximum={}",
        backlight.on,
        backlight.off,
        backlight.maximum
    );

    let timer_configuration = ledc_timer_config_t {
        speed_mode: ledc_mode_t_LEDC_LOW_SPEED_MODE,
        duty_resolution: ledc_timer_bit_t_LEDC_TIMER_10_BIT,
        timer_num: ledc_timer_t_LEDC_TIMER_1,
        freq_hz: FREQUENCY_HZ,
        clk_cfg: soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK,
        deconfigure: false,
    };
    check(
        unsafe { ledc_timer_config(&timer_configuration) },
        "failed to configure LEDC timer for display backlight",
    )?;

    let channel_configuration = ledc_channel_config_t {
        gpio_num: BACKLIGHT_GPIO,
        speed_mode: ledc_mode_t_LEDC_LOW_SPEED_MODE,
        channel: ledc_channel_t_LEDC_CHANNEL_1,
        intr_type: ledc_intr_type_t_LEDC_INTR_DISABLE,
        timer_sel: ledc_timer_t_LEDC_TIMER_1,
        duty: backlight.on,
        hpoint: 0,
        ..Default::default()
    };
    check(
        unsafe { ledc_channel_config(&channel_configuration) },
        "failed to configure LEDC channel for display backlight",
    )?;
    check(
        unsafe { ledc_fade_func_install(0) },
        "failed to install LEDC fade function",
    )?;

    BACKLIGHT
        .set(backlight)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())?;

    Ok(())
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the worker owns the receiver for its full lifetime"
)]
fn display_timer(commands: Receiver<DisplayTimerAction>, acknowledge: Arc<Queue<u8>>) {
    let mut timeout = None;

    loop {
        let command = match timeout {
            Some(duration) => match commands.recv_timeout(duration) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => {
                    info!(target: TIMER_LOG_TARGET, "Wake LCD timeout, turning off LCD");
                    set(false, false);
                    timeout = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match commands.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };

        timeout = match command {
            DisplayTimerAction::Pause => None,
            DisplayTimerAction::Schedule(duration) => Some(duration),
        };
        if acknowledge.send_back(1, BLOCK).is_err() {
            error!(target: TIMER_LOG_TARGET, "failed to acknowledge display timer reset");
            return;
        }
    }
}

pub(crate) fn initialize_display_timer() -> Result<(), EspError> {
    if DISPLAY_TIMER.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let (command, commands) = channel();
    let acknowledgement = Arc::new(Queue::new(1));
    let worker_acknowledgement = Arc::clone(&acknowledgement);
    let timer_thread = thread::Builder::new()
        .name("display_timer".into())
        .stack_size(DISPLAY_TIMER_STACK_SIZE)
        .spawn(move || display_timer(commands, worker_acknowledgement))
        .map_err(|error| {
            error!(target: TIMER_LOG_TARGET, "failed to start display timer task: {error}");
            EspError::from_infallible::<ESP_FAIL>()
        })?;

    DISPLAY_TIMER
        .set(DisplayTimer {
            command,
            acknowledgement,
            reset: Mutex::new(()),
            _thread: timer_thread,
        })
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

pub(crate) fn reset_display_timer(pause: bool) -> Result<(), EspError> {
    let Some(display_timer) = DISPLAY_TIMER.get() else {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    };

    let action = if pause {
        DisplayTimerAction::Pause
    } else {
        let timeout_secs = crate::config::config()
            .and_then(|configuration| configuration.display_timeout)
            .unwrap_or(DEFAULT_DISPLAY_TIMEOUT_SECS);
        DisplayTimerAction::Schedule(Duration::from_secs(u64::from(timeout_secs)))
    };

    // Serialize the shared task-native acknowledgement queue so each caller
    // waits for the action it submitted.
    let _reset = display_timer
        .reset
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    display_timer.command.send(action).map_err(|error| {
        error!(target: TIMER_LOG_TARGET, "failed to reset display timer: {error}");
        EspError::from_infallible::<ESP_FAIL>()
    })?;

    // C may update the backlight immediately after this returns. Wait until
    // the worker has canceled the old deadline or installed the new one.
    if display_timer.acknowledgement.recv_front(BLOCK).is_some() {
        Ok(())
    } else {
        error!(target: TIMER_LOG_TARGET, "display timer reset was not acknowledged");
        Err(EspError::from_infallible::<ESP_FAIL>())
    }
}

pub(crate) fn set(on: bool, maximum: bool) {
    let Some(backlight) = BACKLIGHT.get() else {
        error!(target: LOG_TARGET, "backlight is not initialized");
        return;
    };
    let duty = if on {
        if maximum {
            backlight.maximum
        } else {
            backlight.on
        }
    } else {
        backlight.off
    };

    let _ = check(
        unsafe {
            ledc_set_duty_and_update(
                ledc_mode_t_LEDC_LOW_SPEED_MODE,
                ledc_channel_t_LEDC_CHANNEL_1,
                duty,
                0,
            )
        },
        "failed to set display backlight duty",
    );
}

fn strobe(period: Duration, stop: Receiver<()>) {
    info!(
        target: LOG_TARGET,
        "starting display backlight strobe effect with period '{}'",
        period.as_millis()
    );

    loop {
        set(true, true);
        if !matches!(stop.recv_timeout(period), Err(RecvTimeoutError::Timeout)) {
            break;
        }

        set(false, false);
        if !matches!(stop.recv_timeout(period), Err(RecvTimeoutError::Timeout)) {
            break;
        }
    }
}

pub(crate) fn start_strobe(period_ms: u32) -> Result<(), EspError> {
    let mut active = STROBE.lock().unwrap_or_else(PoisonError::into_inner);
    if active.is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let period = Duration::from_millis(u64::from(period_ms.max(MIN_STROBE_PERIOD_MS)));
    let (stop_sender, stop_receiver) = sync_channel(1);
    let strobe_thread = thread::Builder::new()
        .name("strobe_task".into())
        .stack_size(STROBE_STACK_SIZE)
        .spawn(move || strobe(period, stop_receiver))
        .map_err(|error| {
            error!(target: LOG_TARGET, "failed to start backlight strobe task: {error}");
            EspError::from_infallible::<ESP_FAIL>()
        })?;

    *active = Some(Strobe {
        stop: stop_sender,
        thread: strobe_thread,
    });

    Ok(())
}

pub(crate) fn stop_strobe() {
    let mut active = STROBE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(strobe) = active.take() else {
        return;
    };

    let _ = strobe.stop.send(());
    if strobe.thread.join().is_err() {
        error!(target: LOG_TARGET, "backlight strobe task panicked");
    }
    set(true, false);
}
