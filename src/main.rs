mod config;
mod ffi;
mod logging;
mod net;
mod nvs;
mod sntp;
mod state;

fn main() {
    esp_idf_sys::link_patches();
    let log_filter = logging::initialize();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");
    nvs::init().expect("failed to initialize NVS");

    state::mark_init();
    logging::apply_policy(log_filter).expect("failed to configure logging");
    log::info!(target: "WILLOW/MAIN", "Starting up! Please wait...");
    ffi::init();

    loop {
        ffi::main_loop_iteration();
    }
}
