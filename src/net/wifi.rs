//! Wi-Fi initialization and event handling through ESP-IDF.
//!
//! Rust reads the provisioned credentials and returns the resulting network
//! interface to the C-owned WAS code, which still needs its hostname. Driver
//! setup, event callbacks, hostname setup, SNTP, and connection
//! synchronization stay entirely in Rust.

use core::{
    ffi::c_void,
    net::Ipv4Addr,
    ptr::{self, NonNull},
};
use std::borrow::Cow;

use esp_idf_sys::{
    CONFIG_ESP_WIFI_DYNAMIC_RX_BUFFER_NUM, CONFIG_ESP_WIFI_DYNAMIC_RX_MGMT_BUF,
    CONFIG_ESP_WIFI_ESPNOW_MAX_ENCRYPT_NUM, CONFIG_ESP_WIFI_STATIC_RX_BUFFER_NUM,
    CONFIG_ESP_WIFI_TX_BUFFER_TYPE, ESP_ERR_NO_MEM, ESP_EVENT_ANY_ID, ESP_FAIL, EspError,
    EventGroupDef_t, IP_EVENT, WIFI_AMPDU_RX_ENABLED, WIFI_AMPDU_TX_ENABLED, WIFI_AMSDU_TX_ENABLED,
    WIFI_CACHE_TX_BUFFER_NUM, WIFI_CSI_ENABLED, WIFI_DEFAULT_RX_BA_WIN, WIFI_DUMP_HESIGB_ENABLED,
    WIFI_DYNAMIC_TX_BUFFER_NUM, WIFI_EVENT, WIFI_FEATURE_CAPS, WIFI_INIT_CONFIG_MAGIC,
    WIFI_MGMT_SBUF_NUM, WIFI_NANO_FORMAT_ENABLED, WIFI_NVS_ENABLED, WIFI_RX_MGMT_BUF_NUM_DEF,
    WIFI_SOFTAP_BEACON_MAX_LEN, WIFI_STA_DISCONNECTED_PM_ENABLED, WIFI_STATIC_TX_BUFFER_NUM,
    WIFI_TASK_CORE_ID, WIFI_TX_HETB_QUEUE_NUM, esp_err_t, esp_event_base_t,
    esp_event_handler_register, esp_mac_type_t_ESP_MAC_WIFI_STA, esp_netif_create_default_wifi_sta,
    esp_netif_t, esp_wifi_connect, esp_wifi_get_mac, esp_wifi_init, esp_wifi_set_config,
    esp_wifi_set_ps, esp_wifi_start, g_wifi_default_wpa_crypto_funcs, g_wifi_osi_funcs,
    ip_event_got_ip_t, ip_event_t_IP_EVENT_STA_GOT_IP, wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK,
    wifi_config_t, wifi_event_sta_connected_t, wifi_event_sta_disconnected_t,
    wifi_event_t_WIFI_EVENT_STA_CONNECTED as WIFI_EVENT_STA_CONNECTED,
    wifi_event_t_WIFI_EVENT_STA_DISCONNECTED as WIFI_EVENT_STA_DISCONNECTED,
    wifi_event_t_WIFI_EVENT_STA_START as WIFI_EVENT_STA_START, wifi_init_config_t,
    wifi_interface_t_WIFI_IF_STA as WIFI_IF_STA, wifi_ps_type_t_WIFI_PS_NONE,
    wifi_scan_method_t_WIFI_ALL_CHANNEL_SCAN, wifi_sort_method_t_WIFI_CONNECT_AP_BY_SIGNAL,
    wifi_sta_config_t, xEventGroupCreate, xEventGroupSetBits, xEventGroupWaitBits,
};
use log::{error, info};

use crate::{state, ui};

use super::{log_unhandled, ntp, set_hostname};

const LOG_TARGET: &str = "WILLOW/NETWORK";
const WIFI_BIT_CONNECTED: u32 = 1;

fn check(result: esp_err_t, operation: &str) -> Result<(), EspError> {
    if let Some(error) = EspError::from(result) {
        error!(target: LOG_TARGET, "{operation}: {error}");
        Err(error)
    } else {
        Ok(())
    }
}

fn copy_c_string<const LENGTH: usize>(destination: &mut [u8; LENGTH], source: &[u8]) {
    let copy_length = source.len().min(LENGTH.saturating_sub(1));
    destination[..copy_length].copy_from_slice(&source[..copy_length]);
}

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
unsafe extern "C" fn wifi_event_handler(
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

/// Initializes the station interface and waits until it has an IP address.
///
/// The returned network interface remains owned by ESP-IDF. The caller keeps
/// the borrowed pointer because the existing WAS hello message reads its
/// hostname.
pub(crate) fn initialize(psk: &str, ssid: &str) -> Result<NonNull<esp_netif_t>, EspError> {
    let event_group = unsafe { xEventGroupCreate() };
    if event_group.is_null() {
        error!(target: LOG_TARGET, "failed to create Wi-Fi event group");
        return Err(EspError::from_infallible::<ESP_ERR_NO_MEM>());
    }

    // Keep SNTP initialization before connecting so DHCP can supply a server.
    let _ = ntp::initialize(ip_event_t_IP_EVENT_STA_GOT_IP);

    let network_interface = NonNull::new(unsafe { esp_netif_create_default_wifi_sta() })
        .ok_or_else(|| {
            error!(target: LOG_TARGET, "failed to create Wi-Fi STA interface");
            EspError::from_infallible::<ESP_FAIL>()
        })?;

    check(
        unsafe {
            esp_event_handler_register(
                IP_EVENT,
                ip_event_t_IP_EVENT_STA_GOT_IP as i32,
                Some(ip_event_handler),
                event_group.cast(),
            )
        },
        "failed to register IP event handler",
    )?;
    check(
        unsafe {
            esp_event_handler_register(
                WIFI_EVENT,
                ESP_EVENT_ANY_ID,
                Some(wifi_event_handler),
                ptr::null_mut(),
            )
        },
        "failed to register Wi-Fi event handler",
    )?;

    ui::show_connecting("Connecting to Wi-Fi...");

    let initialization = wifi_init_configuration();
    check(
        unsafe { esp_wifi_init(&raw const initialization) },
        "failed to initialize Wi-Fi",
    )?;

    let mut configuration = station_configuration(psk.as_bytes(), ssid.as_bytes());
    let _ = set_hostname(network_interface, esp_mac_type_t_ESP_MAC_WIFI_STA);

    check(
        unsafe { esp_wifi_set_config(WIFI_IF_STA, &raw mut configuration) },
        "failed to set Wi-Fi config",
    )?;
    check(unsafe { esp_wifi_start() }, "failed to start Wi-Fi")?;
    check(unsafe { esp_wifi_connect() }, "failed to connect to Wi-Fi")?;

    // The event-group allocation intentionally remains live because the
    // registered IP callback keeps using its handle after this function exits.
    unsafe {
        xEventGroupWaitBits(event_group, WIFI_BIT_CONNECTED, 0, 0, u32::MAX);
    }

    let _ = ntp::start();

    check(
        unsafe { esp_wifi_set_ps(wifi_ps_type_t_WIFI_PS_NONE) },
        "failed to set Wi-Fi power save mode",
    )?;

    Ok(network_interface)
}
