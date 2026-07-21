//! Wi-Fi initialization and event handling through ESP-IDF.
//!
//! C supplies the provisioned credentials and retains the resulting network
//! interface for the WAS code that still needs its hostname. Driver setup,
//! event callbacks, hostname setup, and connection synchronization stay
//! entirely in Rust and do not call back into Willow's C implementation.

use core::{
    ffi::{c_char, c_void},
    net::Ipv4Addr,
    ptr,
};
use std::{borrow::Cow, ffi::CStr};

use esp_idf_sys::{
    CONFIG_ESP_WIFI_DYNAMIC_RX_BUFFER_NUM, CONFIG_ESP_WIFI_DYNAMIC_RX_MGMT_BUF,
    CONFIG_ESP_WIFI_ESPNOW_MAX_ENCRYPT_NUM, CONFIG_ESP_WIFI_STATIC_RX_BUFFER_NUM,
    CONFIG_ESP_WIFI_TX_BUFFER_TYPE, ESP_ERR_INVALID_ARG, ESP_ERR_NO_MEM, ESP_EVENT_ANY_ID,
    ESP_FAIL, EventGroupDef_t, IP_EVENT, WIFI_AMPDU_RX_ENABLED, WIFI_AMPDU_TX_ENABLED,
    WIFI_AMSDU_TX_ENABLED, WIFI_CACHE_TX_BUFFER_NUM, WIFI_CSI_ENABLED, WIFI_DEFAULT_RX_BA_WIN,
    WIFI_DUMP_HESIGB_ENABLED, WIFI_DYNAMIC_TX_BUFFER_NUM, WIFI_EVENT, WIFI_FEATURE_CAPS,
    WIFI_INIT_CONFIG_MAGIC, WIFI_MGMT_SBUF_NUM, WIFI_NANO_FORMAT_ENABLED, WIFI_NVS_ENABLED,
    WIFI_RX_MGMT_BUF_NUM_DEF, WIFI_SOFTAP_BEACON_MAX_LEN, WIFI_STA_DISCONNECTED_PM_ENABLED,
    WIFI_STATIC_TX_BUFFER_NUM, WIFI_TASK_CORE_ID, WIFI_TX_HETB_QUEUE_NUM, esp_err_t,
    esp_event_base_t, esp_event_handler_register, esp_netif_create_default_wifi_sta, esp_netif_t,
    esp_wifi_connect, esp_wifi_get_mac, esp_wifi_init, esp_wifi_set_config, esp_wifi_set_ps,
    esp_wifi_start, g_wifi_default_wpa_crypto_funcs, g_wifi_osi_funcs, ip_event_got_ip_t,
    ip_event_t_IP_EVENT_STA_GOT_IP, wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK, wifi_config_t,
    wifi_event_sta_connected_t, wifi_event_sta_disconnected_t,
    wifi_event_t_WIFI_EVENT_STA_CONNECTED as WIFI_EVENT_STA_CONNECTED,
    wifi_event_t_WIFI_EVENT_STA_DISCONNECTED as WIFI_EVENT_STA_DISCONNECTED,
    wifi_event_t_WIFI_EVENT_STA_START as WIFI_EVENT_STA_START, wifi_init_config_t,
    wifi_interface_t_WIFI_IF_STA as WIFI_IF_STA, wifi_ps_type_t_WIFI_PS_NONE,
    wifi_scan_method_t_WIFI_ALL_CHANNEL_SCAN, wifi_sort_method_t_WIFI_CONNECT_AP_BY_SIGNAL,
    wifi_sta_config_t, xEventGroupCreate, xEventGroupSetBits, xEventGroupWaitBits,
};
use log::{error, info};

use crate::{sntp, state};

use super::{LOG_TARGET, check, log_unhandled, set_hostname};

const CONNECTED_BIT: u32 = 1;

fn copy_c_string<const LENGTH: usize>(destination: &mut [u8; LENGTH], source: &[u8]) {
    let copy_length = source.len().min(LENGTH.saturating_sub(1));
    destination[..copy_length].copy_from_slice(&source[..copy_length]);
}

fn ssid(bytes: &[u8; 32], length: u8) -> Cow<'_, str> {
    String::from_utf8_lossy(&bytes[..usize::from(length).min(bytes.len())])
}

fn station_configuration(psk: &[u8], ssid: &[u8]) -> wifi_config_t {
    let mut station = wifi_sta_config_t::default();
    copy_c_string(&mut station.password, psk);
    copy_c_string(&mut station.ssid, ssid);
    station.failure_retry_cnt = 3;
    station.scan_method = wifi_scan_method_t_WIFI_ALL_CHANNEL_SCAN;
    station.sort_method = wifi_sort_method_t_WIFI_CONNECT_AP_BY_SIGNAL;
    station.threshold.authmode = wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK;
    station.set_btm_enabled(1);
    station.set_mbo_enabled(1);
    station.set_rm_enabled(1);

    wifi_config_t { sta: station }
}

/// Builds the Rust equivalent of ESP-IDF's `WIFI_INIT_CONFIG_DEFAULT()`.
///
/// Bindgen cannot expose function-like C macros. Keep this field order aligned
/// with the upstream macro so additions in a future ESP-IDF update are visible
/// during review.
fn wifi_init_configuration() -> wifi_init_config_t {
    unsafe {
        wifi_init_config_t {
            osi_funcs: &raw mut g_wifi_osi_funcs,
            wpa_crypto_funcs: g_wifi_default_wpa_crypto_funcs,
            static_rx_buf_num: CONFIG_ESP_WIFI_STATIC_RX_BUFFER_NUM as i32,
            dynamic_rx_buf_num: CONFIG_ESP_WIFI_DYNAMIC_RX_BUFFER_NUM as i32,
            tx_buf_type: CONFIG_ESP_WIFI_TX_BUFFER_TYPE as i32,
            static_tx_buf_num: WIFI_STATIC_TX_BUFFER_NUM as i32,
            dynamic_tx_buf_num: WIFI_DYNAMIC_TX_BUFFER_NUM as i32,
            rx_mgmt_buf_type: CONFIG_ESP_WIFI_DYNAMIC_RX_MGMT_BUF as i32,
            rx_mgmt_buf_num: WIFI_RX_MGMT_BUF_NUM_DEF as i32,
            cache_tx_buf_num: WIFI_CACHE_TX_BUFFER_NUM as i32,
            csi_enable: WIFI_CSI_ENABLED as i32,
            ampdu_rx_enable: WIFI_AMPDU_RX_ENABLED as i32,
            ampdu_tx_enable: WIFI_AMPDU_TX_ENABLED as i32,
            amsdu_tx_enable: WIFI_AMSDU_TX_ENABLED as i32,
            nvs_enable: WIFI_NVS_ENABLED as i32,
            nano_enable: WIFI_NANO_FORMAT_ENABLED as i32,
            rx_ba_win: WIFI_DEFAULT_RX_BA_WIN as i32,
            wifi_task_core_id: WIFI_TASK_CORE_ID as i32,
            beacon_max_len: WIFI_SOFTAP_BEACON_MAX_LEN as i32,
            mgmt_sbuf_num: WIFI_MGMT_SBUF_NUM as i32,
            feature_caps: u64::from(WIFI_FEATURE_CAPS),
            sta_disconnected_pm: WIFI_STA_DISCONNECTED_PM_ENABLED != 0,
            espnow_max_encrypt_num: CONFIG_ESP_WIFI_ESPNOW_MAX_ENCRYPT_NUM as i32,
            tx_hetb_queue_num: WIFI_TX_HETB_QUEUE_NUM as i32,
            dump_hesigb_enable: WIFI_DUMP_HESIGB_ENABLED != 0,
            magic: WIFI_INIT_CONFIG_MAGIC as i32,
        }
    }
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

/// Handles the ESP-IDF station-address event and releases the connection wait.
///
/// # Safety
///
/// `event_group` must be the live `EventGroupHandle_t` supplied during
/// registration. For `IP_EVENT_STA_GOT_IP`, `event_data` must point to a live
/// [`ip_event_got_ip_t`] for the duration of this call. `event_base` must be
/// null or point to a live NUL-terminated event-base name.
unsafe extern "C" fn ip_event_handler(
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
            xEventGroupSetBits(event_group.cast::<EventGroupDef_t>(), CONNECTED_BIT);
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
unsafe extern "C" fn event_handler(
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

/// Initializes the station interface and waits until it has an IP address.
///
/// The returned network interface remains owned by ESP-IDF. C retains the
/// borrowed pointer because the existing WAS hello message reads its hostname.
///
/// # Safety
///
/// `psk` and `ssid` must point to live NUL-terminated strings for the duration
/// of this call. `network_interface` must point to writable storage for an
/// [`esp_netif_t`] pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_wifi_init(
    psk: *const c_char,
    ssid: *const c_char,
    network_interface: *mut *mut esp_netif_t,
) -> esp_err_t {
    let Some(network_interface) = (unsafe { network_interface.as_mut() }) else {
        error!(target: LOG_TARGET, "network interface output is null");
        return ESP_ERR_INVALID_ARG;
    };
    *network_interface = ptr::null_mut();

    if psk.is_null() || ssid.is_null() {
        error!(target: LOG_TARGET, "Wi-Fi credentials are null");
        return ESP_ERR_INVALID_ARG;
    }
    let psk = unsafe { CStr::from_ptr(psk) }.to_bytes();
    let ssid = unsafe { CStr::from_ptr(ssid) }.to_bytes();

    let event_group = unsafe { xEventGroupCreate() };
    if event_group.is_null() {
        error!(target: LOG_TARGET, "failed to create Wi-Fi event group");
        return ESP_ERR_NO_MEM;
    }

    let _ = sntp::init(ip_event_t_IP_EVENT_STA_GOT_IP);

    if unsafe { esp_netif_create_default_wifi_sta() }.is_null() {
        error!(target: LOG_TARGET, "failed to create Wi-Fi STA interface");
        return ESP_FAIL;
    }

    let result = unsafe {
        esp_event_handler_register(
            IP_EVENT,
            ip_event_t_IP_EVENT_STA_GOT_IP as i32,
            Some(ip_event_handler),
            event_group.cast(),
        )
    };
    if let Err(error) = check(result, "failed to register IP event handler") {
        return error;
    }

    let result = unsafe {
        esp_event_handler_register(
            WIFI_EVENT,
            ESP_EVENT_ANY_ID,
            Some(event_handler),
            ptr::null_mut(),
        )
    };
    if let Err(error) = check(result, "failed to register Wi-Fi event handler") {
        return error;
    }

    let initialization = wifi_init_configuration();
    let result = unsafe { esp_wifi_init(&raw const initialization) };
    if let Err(error) = check(result, "failed to initialize Wi-Fi") {
        return error;
    }

    let mut configuration = station_configuration(psk, ssid);
    *network_interface = set_hostname(esp_idf_sys::esp_mac_type_t_ESP_MAC_WIFI_STA);

    let result = unsafe { esp_wifi_set_config(WIFI_IF_STA, &raw mut configuration) };
    if let Err(error) = check(result, "failed to set Wi-Fi config") {
        return error;
    }

    let result = unsafe { esp_wifi_start() };
    if let Err(error) = check(result, "failed to start Wi-Fi") {
        return error;
    }

    let result = unsafe { esp_wifi_connect() };
    if let Err(error) = check(result, "failed to connect to Wi-Fi") {
        return error;
    }

    // The event-group allocation intentionally remains live because the
    // registered IP callback keeps using its handle after this function exits.
    unsafe {
        xEventGroupWaitBits(event_group, CONNECTED_BIT, 0, 0, u32::MAX);
    }

    let _ = sntp::start();

    let result = unsafe { esp_wifi_set_ps(wifi_ps_type_t_WIFI_PS_NONE) };
    if let Err(error) = check(result, "failed to set Wi-Fi power save mode") {
        return error;
    }

    result
}
