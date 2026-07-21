//! ESP-IDF Wi-Fi operations migrated from the remaining C initialization.
//!
//! C still creates the Wi-Fi event group and registers these callbacks. The IP
//! callback receives that event group as its opaque argument. Callback logic,
//! hostname setup, and MAC address retrieval stay entirely in Rust and call
//! only ESP-IDF through `esp-idf-sys`; they do not call back into Willow's C
//! implementation.

use core::{
    ffi::c_void,
    net::Ipv4Addr,
    ptr::{self, NonNull},
};
use std::borrow::Cow;

use esp_idf_sys::{
    EventGroupDef_t, esp_event_base_t, esp_mac_type_t, esp_netif_t, esp_wifi_connect,
    esp_wifi_get_mac, ip_event_got_ip_t, ip_event_t_IP_EVENT_STA_GOT_IP,
    wifi_event_sta_connected_t, wifi_event_sta_disconnected_t,
    wifi_event_t_WIFI_EVENT_STA_CONNECTED as WIFI_EVENT_STA_CONNECTED,
    wifi_event_t_WIFI_EVENT_STA_DISCONNECTED as WIFI_EVENT_STA_DISCONNECTED,
    wifi_event_t_WIFI_EVENT_STA_START as WIFI_EVENT_STA_START,
    wifi_interface_t_WIFI_IF_STA as WIFI_IF_STA, xEventGroupSetBits,
};
use log::{error, info};

use crate::state;

use super::{log_unhandled, set_hostname};

const LOG_TARGET: &str = "WILLOW/NETWORK";
const WIFI_BIT_CONNECTED: u32 = 1;

fn ssid(bytes: &[u8; 32], length: u8) -> Cow<'_, str> {
    String::from_utf8_lossy(&bytes[..usize::from(length).min(bytes.len())])
}

fn disconnect_reason_name(reason: u8) -> &'static str {
    const STANDARD: [&str; 39] = [
        "UNSPECIFIED",
        "AUTH_EXPIRE",
        "AUTH_LEAVE",
        "DISASSOC_DUE_TO_INACTIVITY",
        "ASSOC_TOOMANY",
        "CLASS2_FRAME_FROM_NONAUTH_STA",
        "CLASS3_FRAME_FROM_NONASSOC_STA",
        "ASSOC_LEAVE",
        "ASSOC_NOT_AUTHED",
        "DISASSOC_PWRCAP_BAD",
        "DISASSOC_SUPCHAN_BAD",
        "BSS_TRANSITION_DISASSOC",
        "IE_INVALID",
        "MIC_FAILURE",
        "4WAY_HANDSHAKE_TIMEOUT",
        "GROUP_KEY_UPDATE_TIMEOUT",
        "IE_IN_4WAY_DIFFERS",
        "GROUP_CIPHER_INVALID",
        "PAIRWISE_CIPHER_INVALID",
        "AKMP_INVALID",
        "UNSUPP_RSN_IE_VERSION",
        "INVALID_RSN_IE_CAP",
        "802_1X_AUTH_FAILED",
        "CIPHER_SUITE_REJECTED",
        "TDLS_PEER_UNREACHABLE",
        "TDLS_UNSPECIFIED",
        "SSP_REQUESTED_DISASSOC",
        "NO_SSP_ROAMING_AGREEMENT",
        "BAD_CIPHER_OR_AKM",
        "NOT_AUTHORIZED_THIS_LOCATION",
        "SERVICE_CHANGE_PERCLUDES_TS",
        "UNSPECIFIED_QOS",
        "NOT_ENOUGH_BANDWIDTH",
        "MISSING_ACKS",
        "EXCEEDED_TXOP",
        "STA_LEAVING",
        "END_BA",
        "UNKNOWN_BA",
        "TIMEOUT",
    ];
    const EXTENDED: [&str; 6] = [
        "PEER_INITIATED",
        "AP_INITIATED",
        "INVALID_FT_ACTION_FRAME_COUNT",
        "INVALID_PMKID",
        "INVALID_MDE",
        "INVALID_FTE",
    ];
    const LINK: [&str; 2] = [
        "TRANSMISSION_LINK_ESTABLISH_FAILED",
        "ALTERATIVE_CHANNEL_OCCUPIED",
    ];
    const ESPRESSIF: [&str; 13] = [
        "BEACON_TIMEOUT",
        "NO_AP_FOUND",
        "AUTH_FAIL",
        "ASSOC_FAIL",
        "HANDSHAKE_TIMEOUT",
        "CONNECTION_FAIL",
        "AP_TSF_RESET",
        "ROAMING",
        "ASSOC_COMEBACK_TIME_TOO_LONG",
        "SA_QUERY_TIMEOUT",
        "NO_AP_FOUND_W_COMPATIBLE_SECURITY",
        "NO_AP_FOUND_IN_AUTHMODE_THRESHOLD",
        "NO_AP_FOUND_IN_RSSI_THRESHOLD",
    ];

    let name = match reason {
        0 => return "RESERVED",
        1..=39 => STANDARD.get(usize::from(reason - 1)),
        46..=51 => EXTENDED.get(usize::from(reason - 46)),
        67..=68 => LINK.get(usize::from(reason - 67)),
        200..=212 => ESPRESSIF.get(usize::from(reason - 200)),
        _ => None,
    };
    name.copied().unwrap_or("UNKNOWN")
}

/// Reads and logs the station MAC address through ESP-IDF.
pub(crate) fn log_mac_address() {
    let mut address = [0; 6];
    let _ = unsafe { esp_wifi_get_mac(WIFI_IF_STA, address.as_mut_ptr()) };
    let [a, b, c, d, e, f] = address;
    info!(target: LOG_TARGET, "MAC address: {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}");
}

/// Compatibility entry point for the remaining C startup path.
#[unsafe(no_mangle)]
pub extern "C" fn rust_get_mac_address() {
    log_mac_address();
}

/// Compatibility entry point for the remaining C startup path.
///
/// # Safety
///
/// `network_interface` must point to a live ESP-IDF network interface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_set_hostname(
    network_interface: *mut esp_netif_t,
    mac_type: esp_mac_type_t,
) -> *mut esp_netif_t {
    let Some(network_interface) = NonNull::new(network_interface) else {
        error!(target: LOG_TARGET, "cannot set hostname on a null network interface");
        return ptr::null_mut();
    };

    set_hostname(network_interface, mac_type).map_or(ptr::null_mut(), NonNull::as_ptr)
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
            let reason_name = disconnect_reason_name(event.reason);
            info!(
                target: LOG_TARGET,
                "disconnected from AP (BSSID='{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}' SSID='{ssid}' reason='{reason_name} ({})' rssi='{}')",
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
