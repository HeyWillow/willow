//! SNTP setup backed by the typed Willow configuration.
//!
//! Transport initialization calls this module at the two points required by
//! ESP-NETIF: initialization happens before connecting so DHCP can supply an
//! NTP server, while startup happens after an address is acquired.

use core::{ffi::c_char, ptr};
use std::{ffi::CStr, ffi::CString, sync::OnceLock};

use esp_idf_sys::{
    CONFIG_LWIP_SNTP_MAX_SERVERS, ESP_ERR_INVALID_ARG, EspError, esp_netif_sntp_init,
    esp_netif_sntp_start, esp_sntp_config_t, esp_sntp_getserver, esp_sntp_getservername,
    esp_sntp_servermode_dhcp, esp_sntp_setservername, ip_event_t, ipaddr_ntoa_r, setenv, timeval,
    tzset,
};
use log::{error, info};
use willow_schema::config::v1::NtpConfig;

use crate::config::config;

const DEFAULT_NTP_HOST: &str = "pool.ntp.org";
const DEFAULT_TIMEZONE: &str = "CST6CDT,M3.2.0,M11.1.0";
const INET6_ADDRSTRLEN: usize = 48;
const LOG_TARGET: &str = "WILLOW/NETWORK";

static NTP_HOST: OnceLock<CString> = OnceLock::new();

unsafe extern "C" fn synchronization_callback(time: *mut timeval) {
    let Some(time) = (unsafe { time.as_ref() }) else {
        error!(target: LOG_TARGET, "SNTP synchronization callback received no time");
        return;
    };

    let Ok(server_count) = u8::try_from(CONFIG_LWIP_SNTP_MAX_SERVERS) else {
        error!(target: LOG_TARGET, "SNTP server count exceeds the ESP-IDF API limit");
        return;
    };
    let Ok(buffer_length) = i32::try_from(INET6_ADDRSTRLEN) else {
        error!(target: LOG_TARGET, "SNTP address buffer exceeds the ESP-IDF API limit");
        return;
    };

    for index in 0..server_count {
        let server_name = unsafe { esp_sntp_getservername(index) };
        if !server_name.is_null() {
            let server_name = unsafe { CStr::from_ptr(server_name) }.to_string_lossy();
            info!(
                target: LOG_TARGET,
                "SNTP client synchronized time to {} from server {server_name}",
                time.tv_sec
            );
            continue;
        }

        let server_address = unsafe { esp_sntp_getserver(index) };
        if server_address.is_null() {
            continue;
        }

        let mut buffer = [0 as c_char; INET6_ADDRSTRLEN];
        let formatted =
            unsafe { ipaddr_ntoa_r(server_address, buffer.as_mut_ptr(), buffer_length) };
        if !formatted.is_null() {
            let server_address = unsafe { CStr::from_ptr(formatted) }.to_string_lossy();
            info!(
                target: LOG_TARGET,
                "SNTP client synchronized time to {} from server {server_address}",
                time.tv_sec
            );
        }
    }
}

/// Configures the timezone and initializes the ESP-NETIF SNTP service.
pub(crate) fn initialize(ip_event_to_renew: ip_event_t) -> Result<(), EspError> {
    info!(target: LOG_TARGET, "initializing SNTP client");

    let timezone = config()
        .and_then(|config| config.timezone.as_deref())
        .unwrap_or(DEFAULT_TIMEZONE);
    let Ok(timezone) = CString::new(timezone) else {
        error!(target: LOG_TARGET, "configured timezone contains NUL");
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_ARG>());
    };

    unsafe {
        setenv(c"TZ".as_ptr(), timezone.as_ptr(), 1);
        tzset();
    }

    let sntp_config = esp_sntp_config_t {
        smooth_sync: false,
        server_from_dhcp: true,
        wait_for_sync: true,
        start: false,
        sync_cb: Some(synchronization_callback),
        renew_servers_after_new_IP: true,
        ip_event_to_renew,
        index_of_first_server: 0,
        num_of_servers: 0,
        servers: [ptr::null(); CONFIG_LWIP_SNTP_MAX_SERVERS as usize],
    };

    match EspError::from(unsafe { esp_netif_sntp_init(&raw const sntp_config) }) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Selects the configured NTP source and starts the ESP-NETIF SNTP service.
///
/// The retained [`CString`] is intentional: lwIP stores the configured host as
/// a borrowed pointer rather than copying it.
pub(crate) fn start() -> Result<(), EspError> {
    match config().and_then(|config| config.ntp_config) {
        Some(NtpConfig::Dhcp) => {
            info!(target: LOG_TARGET, "Using DHCP SNTP server");
            unsafe {
                esp_sntp_servermode_dhcp(true);
            }
        }
        Some(NtpConfig::Host) | None => {
            let ntp_host = config()
                .and_then(|config| config.ntp_host.as_deref())
                .unwrap_or(DEFAULT_NTP_HOST);
            let Ok(ntp_host) = CString::new(ntp_host) else {
                error!(target: LOG_TARGET, "configured NTP host contains NUL");
                return Err(EspError::from_infallible::<ESP_ERR_INVALID_ARG>());
            };
            let ntp_host = NTP_HOST.get_or_init(|| ntp_host);

            info!(
                target: LOG_TARGET,
                "Using configured SNTP server '{}'",
                ntp_host.to_string_lossy()
            );
            unsafe {
                esp_sntp_setservername(0, ntp_host.as_ptr());
            }
        }
    }

    match EspError::from(unsafe { esp_netif_sntp_start() }) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
