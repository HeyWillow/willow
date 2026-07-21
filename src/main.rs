mod audio;
mod backlight;
mod config;
mod display;
mod ffi;
mod i2c;
mod logging;
mod spiffs;
mod state;
mod ui;

fn main() {
    esp_idf_sys::link_patches();
    let log_filter = logging::initialize();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");

    state::mark_init();
    logging::apply_policy(log_filter).expect("failed to configure logging");
    log::info!(target: "WILLOW/MAIN", "Starting up! Please wait...");

    if let Err(error) = audio::initialize_recorder_queue() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize recorder queue: {error}");
        unsafe { esp_idf_sys::esp_system_abort(c"recorder queue initialization failed".as_ptr()) }
    }
    if let Err(error) = i2c::init() {
        log::error!(target: "WILLOW/MAIN", "failed to initialize I2C0 master bus: {error}");
        unsafe { esp_idf_sys::esp_system_abort(c"I2C0 initialization failed".as_ptr()) }
    }
    ffi::init();

    loop {
        ffi::main_loop_iteration();
    }
}
