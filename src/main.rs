mod config;
mod ffi;
mod logging;
mod net;
mod nvs;
mod sntp;
mod spiffs;
mod state;

use esp_idf_svc::eventloop::EspSystemEventLoop;

fn main() {
    esp_idf_sys::link_patches();
    let log_filter = logging::initialize();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");
    nvs::init().expect("failed to initialize NVS");

    state::mark_init();
    logging::apply_policy(log_filter).expect("failed to configure logging");
    log::info!(target: "WILLOW/MAIN", "Starting up! Please wait...");

    // Dropping this handle deletes the default event loop. Keep it in Rust's
    // non-returning main function so it remains available to every subsystem.
    let _system_event_loop =
        EspSystemEventLoop::take().expect("failed to initialize default event loop");

    ffi::init();

    loop {
        ffi::main_loop_iteration();
    }
}
