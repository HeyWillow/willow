mod config;
mod ffi;
mod network;
mod nvs;
mod state;

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");
    nvs::init().expect("failed to initialize NVS");

    state::mark_init();
    ffi::init();

    loop {
        ffi::main_loop_iteration();
    }
}
