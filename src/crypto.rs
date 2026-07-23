//! PSA Crypto initialization for TLS 1.3 support.

use esp_idf_sys::{psa_crypto_init, psa_status_t};

const LOG_TARGET: &str = "WILLOW/MAIN";
// The PSA header defines this macro as zero, but bindgen does not emit it.
const PSA_SUCCESS: psa_status_t = 0;

/// Initializes PSA Crypto after the network has supplied entropy.
pub(crate) fn initialize() {
    let status = unsafe { psa_crypto_init() };
    if status != PSA_SUCCESS {
        log::error!(
            target: LOG_TARGET,
            "failed to initialize Mbed TLS PSA library, TLS will not work"
        );
    }
}

/// Compatibility entry point for C-owned startup ordering.
#[unsafe(no_mangle)]
pub extern "C" fn rust_crypto_init() {
    initialize();
}
