mod audio;
mod backlight;
mod config;
mod display;
mod ffi;
mod i2c;
mod input;
mod logging;
mod nvs;
mod spiffs;
mod state;
mod system;
mod ui;

use esp_idf_svc::eventloop::EspSystemEventLoop;

fn main() {
    esp_idf_sys::link_patches();
    let log_filter = logging::initialize();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");

    state::mark_init();
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

    loop {
        ffi::main_loop_iteration();
    }
}
