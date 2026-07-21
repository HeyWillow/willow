//! ESP-IDF network operations migrated from the remaining C initialization.
//!
//! C still creates the Wi-Fi event group and registers these callbacks. The IP
//! callback receives that event group as its opaque argument. Callback logic,
//! hostname setup, and MAC address retrieval stay entirely in Rust and call only
//! ESP-IDF through `esp-idf-sys`; they do not call back into Willow's C
//! implementation.

use core::{ffi::c_void, net::Ipv4Addr, ptr};
use std::{
    borrow::Cow,
    ffi::{CStr, CString},
};

use esp_idf_sys::{
    esp_event_base_t, esp_mac_type_t, esp_netif_get_nr_of_ifs, esp_netif_next_unsafe,
    esp_netif_set_hostname, esp_netif_t, esp_read_mac, esp_wifi_connect, esp_wifi_get_mac,
    ip_event_got_ip_t, ip_event_t_IP_EVENT_STA_GOT_IP, vTaskDelay, wifi_event_sta_connected_t,
    wifi_event_sta_disconnected_t,
    wifi_event_t_WIFI_EVENT_STA_CONNECTED as WIFI_EVENT_STA_CONNECTED,
    wifi_event_t_WIFI_EVENT_STA_DISCONNECTED as WIFI_EVENT_STA_DISCONNECTED,
    wifi_event_t_WIFI_EVENT_STA_START as WIFI_EVENT_STA_START,
    wifi_interface_t_WIFI_IF_STA as WIFI_IF_STA, xEventGroupSetBits, EspError, EventGroupDef_t,
    CONFIG_FREERTOS_HZ, CONFIG_LWIP_LOCAL_HOSTNAME,
};
use log::{error, info};

use crate::state;

const LOG_TARGET: &str = "WILLOW/NETWORK";
const WIFI_BIT_CONNECTED: u32 = 1;

fn log_unhandled(event_base: esp_event_base_t, event_id: i32) {
    let event_base = if event_base.is_null() {
        Cow::Borrowed("<null>")
    } else {
        unsafe { CStr::from_ptr(event_base) }.to_string_lossy()
    };
    info!(target: LOG_TARGET, "unhandled network event ev_base='{event_base}' ev_id='{event_id}'");
}

fn ssid(bytes: &[u8; 32], length: u8) -> Cow<'_, str> {
    String::from_utf8_lossy(&bytes[..usize::from(length).min(bytes.len())])
}

/// Reads and logs the station MAC address through ESP-IDF.
///
/// The C startup path still calls this symbol after Wi-Fi initialization. It
/// remains exported until that caller moves to Rust as well.
#[unsafe(no_mangle)]
pub extern "C" fn rust_get_mac_address() {
    let mut address = [0; 6];
    let _ = unsafe { esp_wifi_get_mac(WIFI_IF_STA, address.as_mut_ptr()) };
    let [a, b, c, d, e, f] = address;
    info!(target: LOG_TARGET, "MAC address: {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}");
}

/// Sets the first network interface's hostname from its hardware MAC address.
///
/// The returned interface remains owned by ESP-IDF. C stores this borrowed
/// pointer in its existing `hdl_netif` global for the WAS code that still
/// reads the hostname there.
#[unsafe(no_mangle)]
pub extern "C" fn rust_set_hostname(mac_type: esp_mac_type_t) -> *mut esp_netif_t {
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

/// Handles the ESP-IDF station-address event and releases C's connection
/// wait.
///
/// # Safety
///
/// `event_group` must be the live `EventGroupHandle_t` supplied during
/// registration. For `IP_EVENT_STA_GOT_IP`, `event_data` must point to a live
/// [`ip_event_got_ip_t`] for the duration of this call. `event_base` must be
/// null or point to a live NUL-terminated event-base name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ip_event_handler(
    event_group: *mut c_void,
    event_base: esp_event_base_t,
    event_id: i32,
    event_data: *mut c_void,
) {
    if event_id == ip_event_t_IP_EVENT_STA_GOT_IP as i32 {
        let Some(event) = (unsafe { event_data.cast::<ip_event_got_ip_t>().as_ref() }) else {
            error!(target: LOG_TARGET, "IP event data is null");
            return;
        };

        let address = Ipv4Addr::from(event.ip_info.ip.addr.to_ne_bytes());
        info!(target: LOG_TARGET, "received IP: {address}");

        if event_group.is_null() {
            error!(target: LOG_TARGET, "Wi-Fi event group is null");
            return;
        }

        unsafe {
            xEventGroupSetBits(event_group.cast::<EventGroupDef_t>(), WIFI_BIT_CONNECTED);
        }
        return;
    }

    log_unhandled(event_base, event_id);
}

/// Handles ESP-IDF station lifecycle events and reconnects unless firmware
/// restart shutdown has begun.
///
/// # Safety
///
/// For connected and disconnected station events, `event_data` must point to
/// the corresponding ESP-IDF event structure for the duration of this call.
/// `event_base` must be null or point to a live NUL-terminated event-base name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_wifi_event_handler(
    _event_handler_arg: *mut c_void,
    event_base: esp_event_base_t,
    event_id: i32,
    event_data: *mut c_void,
) {
    match event_id as u32 {
        WIFI_EVENT_STA_CONNECTED => {
            let Some(event) = (unsafe { event_data.cast::<wifi_event_sta_connected_t>().as_ref() })
            else {
                error!(target: LOG_TARGET, "connected event data is null");
                return;
            };

            let [a, b, c, d, e, f] = event.bssid;
            let ssid = ssid(&event.ssid, event.ssid_len);
            info!(
                target: LOG_TARGET,
                "connected to AP (BSSID='{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}' SSID='{ssid}' channel='{}')",
                event.channel
            );
        }
        WIFI_EVENT_STA_DISCONNECTED => {
            let Some(event) =
                (unsafe { event_data.cast::<wifi_event_sta_disconnected_t>().as_ref() })
            else {
                error!(target: LOG_TARGET, "disconnected event data is null");
                return;
            };

            let [a, b, c, d, e, f] = event.bssid;
            let ssid = ssid(&event.ssid, event.ssid_len);
            info!(
                target: LOG_TARGET,
                "disconnected from AP (BSSID='{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}' SSID='{ssid}' reason='{}' rssi='{}')",
                event.reason,
                event.rssi
            );

            if !state::is_restarting() {
                info!(target: LOG_TARGET, "reconnecting");
                unsafe {
                    esp_wifi_connect();
                }
            }
        }
        WIFI_EVENT_STA_START => {
            info!(target: LOG_TARGET, "WIFI_EVENT_STA_START");
        }
        _ => {
            log_unhandled(event_base, event_id);
        }
    }
}
