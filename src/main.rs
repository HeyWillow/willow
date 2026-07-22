mod audio;
mod backlight;
mod config;
mod display;
mod ffi;
mod i2c;
mod ui;

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");

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
