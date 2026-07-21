mod config;
mod ffi;
mod nvs;

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");
    nvs::init().expect("failed to initialize NVS");

    ffi::init();

    loop {
        ffi::main_loop_iteration();
    }
}
