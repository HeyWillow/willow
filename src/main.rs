mod audio;
mod backlight;
mod config;
#[cfg(esp_idf_mbedtls_ssl_proto_tls1_3)]
mod crypto;
mod display;
mod ffi;
mod i2c;
mod input;
mod logging;
mod net;
mod nvs;
mod ota;
#[cfg(esp_idf_willow_debug_runtime_stats)]
mod runtime_stats;
mod spiffs;
mod state;
mod system;
mod ui;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;

const MAIN_LOOP_INTERVAL_MS: u32 = 5_000;

fn main() {
    esp_idf_sys::link_patches();
    let log_filter = logging::initialize();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");

    logging::apply_policy(log_filter).expect("failed to configure logging");
    log::info!(target: "WILLOW/MAIN", "Starting up! Please wait...");

    // Dropping this handle deletes the default event loop. Keep it in Rust's
    // non-returning main function so it remains available to every subsystem.
    let _system_event_loop =
        EspSystemEventLoop::take().expect("failed to initialize default event loop");
    system::log_hardware();

    match spiffs::mount() {
        Ok(()) => log::info!(target: "WILLOW/MAIN", "SPIFFS mounted"),
        Err(error) => {
            log::error!(target: "WILLOW/MAIN", "failed to mount SPIFFS user partition: {error}");
            // Preserve the old wait for the filesystem to become mounted.
            unsafe { esp_idf_sys::vTaskDelay(u32::MAX) }
        }
    }
    config::load();
    if let Err(error) = display::initialize() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize display: {error}");
    }
    if let Err(error) = ui::initialize() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize UI: {error}");
    }
    if let Err(error) = nvs::initialize() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize NVS: {error}");
        ui::show_error("Fatal error!", Some("Failed to read NVS partition."));
        unsafe { esp_idf_sys::vTaskDelay(u32::MAX) }
    }

    if let Err(error) = audio::initialize_recorder_queue() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize recorder queue: {error}");
        unsafe { esp_idf_sys::esp_system_abort(c"recorder queue initialization failed".as_ptr()) }
    }
    if let Err(error) = i2c::init() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize I2C0 master bus: {error}");
        unsafe { esp_idf_sys::esp_system_abort(c"I2C0 initialization failed".as_ptr()) }
    }
    if let Err(error) = input::init() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize mute input: {error}");
    }
    ffi::init();

    log::info!(
        target: "WILLOW/MAIN",
        "Startup complete! Hardware: {}. Version: {}. Waiting for wake word.",
        system::hardware().name(),
        ota::running_version()
    );

    // Reaching this point is enough to mark the current partition valid, so
    // it remains the boot partition after restart. Wake handling and the main
    // loop can still fail after this point.
    if let Err(error) = ota::mark_running_slot_valid() {
        log::error!(target: "WILLOW/MAIN", "failed to mark running firmware valid: {error}");
    }
    if let Err(error) = backlight::reset_display_timer(false) {
        log::error!(target: "WILLOW/MAIN", "failed to reset display timer: {error}");
    }

    #[cfg(esp_idf_willow_debug_runtime_stats)]
    let _ = runtime_stats::start();

    loop {
        #[cfg(esp_idf_willow_debug_mem)]
        print_memory();

        #[cfg(esp_idf_willow_debug_tasks)]
        print_tasks();

        #[cfg(esp_idf_willow_debug_timers)]
        print_timers();

        FreeRtos::delay_ms(MAIN_LOOP_INTERVAL_MS);
    }
}

#[cfg(esp_idf_willow_debug_mem)]
fn print_memory() {
    // These ESP-IDF diagnostics synchronously inspect the allocator and write
    // their report to the console. Static format strings make the variadic
    // calls equivalent to the previous C implementation.
    unsafe {
        esp_idf_sys::printf(c"MALLOC_CAP_INTERNAL:\n".as_ptr());
        esp_idf_sys::heap_caps_print_heap_info(esp_idf_sys::MALLOC_CAP_INTERNAL);
        esp_idf_sys::printf(c"MALLOC_CAP_SPIRAM:\n".as_ptr());
        esp_idf_sys::heap_caps_print_heap_info(esp_idf_sys::MALLOC_CAP_SPIRAM);
    }
}

#[cfg(esp_idf_willow_debug_tasks)]
fn print_tasks() {
    let mut buffer = [0; 128];

    // FreeRTOS provides no buffer length to this formatting function. The
    // fixed buffer deliberately matches the previous C main loop.
    unsafe {
        esp_idf_sys::vTaskList(buffer.as_mut_ptr());
        esp_idf_sys::printf(c"%s\n".as_ptr(), buffer.as_ptr());
    }
}

#[cfg(esp_idf_willow_debug_timers)]
fn print_timers() {
    // C's `stdout` macro resolves through the current task's newlib reentrancy
    // state. Access the same stream before handing it to ESP-IDF.
    unsafe {
        let stdout = (*esp_idf_sys::__getreent())._stdout;
        let _ = esp_idf_sys::esp_timer_dump(stdout);
    }
}
