//! Network initialization and event handling through ESP-IDF.

#[cfg(esp_idf_willow_ethernet)]
mod ethernet;
mod wifi;

use core::ptr;
use std::{
    borrow::Cow,
    ffi::{CStr, CString},
};

use esp_idf_sys::{
    CONFIG_FREERTOS_HZ, CONFIG_LWIP_LOCAL_HOSTNAME, EspError, esp_err_t, esp_event_base_t,
    esp_mac_type_t, esp_netif_get_nr_of_ifs, esp_netif_next_unsafe, esp_netif_set_hostname,
    esp_netif_t, esp_read_mac, vTaskDelay,
};
use log::{error, info};

const LOG_TARGET: &str = "WILLOW/NETWORK";

fn check(result: esp_err_t, operation: &str) -> Result<(), esp_err_t> {
    if let Some(error) = EspError::from(result) {
        error!(target: LOG_TARGET, "{operation}: {error}");
        Err(result)
    } else {
        Ok(())
    }
}

fn log_unhandled(event_base: esp_event_base_t, event_id: i32) {
    let event_base = if event_base.is_null() {
        Cow::Borrowed("<null>")
    } else {
        unsafe { CStr::from_ptr(event_base) }.to_string_lossy()
    };
    info!(target: LOG_TARGET, "unhandled network event ev_base='{event_base}' ev_id='{event_id}'");
}

/// Sets the first network interface's hostname from its hardware MAC address.
///
/// The returned interface remains owned by ESP-IDF. C stores this borrowed
/// pointer in its existing `hdl_netif` global for the WAS code that still
/// reads the hostname there.
fn set_hostname(mac_type: esp_mac_type_t) -> *mut esp_netif_t {
    let mut address = [0; 6];
    if EspError::from(unsafe { esp_read_mac(address.as_mut_ptr(), mac_type) }).is_some() {
        let default_hostname = CStr::from_bytes_with_nul(CONFIG_LWIP_LOCAL_HOSTNAME)
            .map(CStr::to_string_lossy)
            .unwrap_or(Cow::Borrowed("<invalid>"));
        error!(
            target: LOG_TARGET,
            "failed to read MAC address, using default hostname ({default_hostname})"
        );
        return ptr::null_mut();
    }

    while unsafe { esp_netif_get_nr_of_ifs() } == 0 {
        unsafe {
            vTaskDelay(CONFIG_FREERTOS_HZ / 10);
        }
    }

    let [a, b, c, d, e, f] = address;
    let hostname = CString::new(format!("willow-{a:02x}{b:02x}{c:02x}{d:02x}{e:02x}{f:02x}"))
        .expect("a formatted MAC address cannot contain NUL");
    let network_interface = unsafe { esp_netif_next_unsafe(ptr::null_mut()) };

    if let Some(error) =
        EspError::from(unsafe { esp_netif_set_hostname(network_interface, hostname.as_ptr()) })
    {
        error!(
            target: LOG_TARGET,
            "failed to set hostname ({}): {error}",
            hostname.to_string_lossy()
        );
    }

    network_interface
}
