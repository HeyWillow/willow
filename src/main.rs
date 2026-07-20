mod config;
mod ffi;

fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!(target: "WILLOW/RUST", "entered Rust main()");

    ffi::init();

    loop {
        ffi::main_loop_iteration();
    }
}
