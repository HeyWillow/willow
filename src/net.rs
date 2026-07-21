pub(crate) mod ntp;
pub(crate) mod wifi;

use core::ptr::NonNull;
use std::{
    borrow::Cow,
    ffi::{CStr, CString},
};

use esp_idf_sys::{
    CONFIG_LWIP_LOCAL_HOSTNAME, EspError, esp_mac_type_t, esp_netif_set_hostname, esp_netif_t,
    esp_read_mac,
};
use log::error;

const LOG_TARGET: &str = "WILLOW/NETWORK";

/// Sets a network interface's hostname from its hardware MAC address.
///
/// The returned interface remains owned by ESP-IDF.
fn set_hostname(
    network_interface: NonNull<esp_netif_t>,
    mac_type: esp_mac_type_t,
) -> Option<NonNull<esp_netif_t>> {
    let mut address = [0; 6];
    if EspError::from(unsafe { esp_read_mac(address.as_mut_ptr(), mac_type) }).is_some() {
        let default_hostname = CStr::from_bytes_with_nul(CONFIG_LWIP_LOCAL_HOSTNAME)
            .map(CStr::to_string_lossy)
            .unwrap_or(Cow::Borrowed("<invalid>"));
        error!(
            target: LOG_TARGET,
            "failed to read MAC address, using default hostname ({default_hostname})"
        );
        return None;
    }

    let [a, b, c, d, e, f] = address;
    let hostname = CString::new(format!("willow-{a:02x}{b:02x}{c:02x}{d:02x}{e:02x}{f:02x}"))
        .expect("a formatted MAC address cannot contain NUL");

    if let Some(error) = EspError::from(unsafe {
        esp_netif_set_hostname(network_interface.as_ptr(), hostname.as_ptr())
    }) {
        error!(
            target: LOG_TARGET,
            "failed to set hostname ({}): {error}",
            hostname.to_string_lossy()
        );
    }

    Some(network_interface)
}
