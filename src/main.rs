mod config;
mod ffi;
mod logging;
mod net;
mod nvs;
mod ota;
#[cfg(esp_idf_willow_debug_runtime_stats)]
mod runtime_stats;
mod sntp;
mod spiffs;
mod state;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::FreeRtos;

const MAIN_LOOP_INTERVAL_MS: u32 = 5_000;

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
