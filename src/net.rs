#[cfg(esp_idf_willow_ethernet)]
mod ethernet;
pub(crate) mod ntp;
#[cfg(not(esp_idf_willow_ethernet))]
pub(crate) mod wifi;

use core::{
    fmt, ptr,
    ptr::NonNull,
    sync::atomic::{AtomicPtr, Ordering},
};
use std::{
    borrow::Cow,
    ffi::{CStr, CString},
};

use esp_idf_sys::{
    CONFIG_LWIP_LOCAL_HOSTNAME, ESP_FAIL, EspError, esp_err_t, esp_event_base_t, esp_mac_type_t,
    esp_mac_type_t_ESP_MAC_ETH, esp_mac_type_t_ESP_MAC_WIFI_STA, esp_netif_get_hostname,
    esp_netif_init, esp_netif_set_hostname, esp_netif_t, esp_read_mac,
};
use log::{error, info};

#[cfg(not(esp_idf_willow_ethernet))]
use crate::{nvs, ui};

#[cfg(esp_idf_mbedtls_ssl_proto_tls1_3)]
use crate::crypto;
#[cfg(not(esp_idf_willow_ethernet))]
use esp_idf_sys::vTaskDelay;

const LOG_TARGET: &str = "WILLOW/NETWORK";
static NETWORK_INTERFACE: AtomicPtr<esp_netif_t> = AtomicPtr::new(ptr::null_mut());
const NETWORK_MAC_TYPE: esp_mac_type_t = if cfg!(esp_idf_willow_ethernet) {
    esp_mac_type_t_ESP_MAC_ETH
} else {
    esp_mac_type_t_ESP_MAC_WIFI_STA
};
#[cfg(not(esp_idf_willow_ethernet))]
const NVS_LOG_TARGET: &str = "WILLOW/MAIN";

fn check(result: esp_err_t) -> Result<(), EspError> {
    EspError::from(result).map_or(Ok(()), Err)
}

#[cfg(not(esp_idf_willow_ethernet))]
fn fatal_wifi_provisioning(error: &nvs::ReadError) -> ! {
    error!(target: NVS_LOG_TARGET, "failed to read Wi-Fi NVS configuration: {error}");
    ui::show_error("Fatal error!", Some("Failed to read NVS partition."));

    loop {
        unsafe {
            vTaskDelay(u32::MAX);
        }
    }
}

/// Initializes ESP-NETIF, the configured transport, and PSA Crypto.
///
/// Wi-Fi driver failures remain advisory, matching the C startup path. A
/// missing or invalid Wi-Fi provisioning record displays the fatal NVS screen
/// and waits indefinitely. ESP-NETIF and Ethernet failures remain fatal to the
/// caller.
pub(crate) fn initialize() -> Result<(), EspError> {
    check(unsafe { esp_netif_init() })?;

    #[cfg(esp_idf_willow_ethernet)]
    let network_interface = Some(ethernet::initialize()?);

    #[cfg(not(esp_idf_willow_ethernet))]
    let network_interface;
    #[cfg(not(esp_idf_willow_ethernet))]
    {
        let wifi = match nvs::read_wifi() {
            Ok(wifi) => wifi,
            Err(error) => fatal_wifi_provisioning(&error),
        };
        network_interface = wifi::initialize(wifi.psk.as_str(), wifi.ssid.as_str()).ok();
    }

    #[cfg(esp_idf_mbedtls_ssl_proto_tls1_3)]
    crypto::initialize();

    NETWORK_INTERFACE.store(
        network_interface.map_or(ptr::null_mut(), NonNull::as_ptr),
        Ordering::Release,
    );

    Ok(())
}

fn network_interface() -> Option<NonNull<esp_netif_t>> {
    NonNull::new(NETWORK_INTERFACE.load(Ordering::Acquire))
}

/// Returns a copy of the initialized network interface's hostname.
pub(crate) fn hostname() -> Result<String, EspError> {
    let network_interface =
        network_interface().ok_or_else(EspError::from_infallible::<ESP_FAIL>)?;
    let mut hostname = ptr::null();
    check(unsafe { esp_netif_get_hostname(network_interface.as_ptr(), &raw mut hostname) })?;

    if hostname.is_null() {
        return Err(EspError::from_infallible::<ESP_FAIL>());
    }

    Ok(unsafe { CStr::from_ptr(hostname) }
        .to_string_lossy()
        .into_owned())
}

/// Reads and logs the configured transport's ESP-derived MAC address.
pub(crate) fn log_mac_address() {
    let mut address = [0; 6];
    if let Some(error) =
        EspError::from(unsafe { esp_read_mac(address.as_mut_ptr(), NETWORK_MAC_TYPE) })
    {
        error!(target: LOG_TARGET, "failed to read MAC address: {error}");
        return;
    }

    info!(target: LOG_TARGET, "MAC address: {}", MacAddress(address));
}

struct MacAddress([u8; 6]);

impl fmt::Display for MacAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
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

    let hostname = match CString::new(format!(
        "willow-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        address[0], address[1], address[2], address[3], address[4], address[5]
    )) {
        Ok(hostname) => hostname,
        Err(error) => {
            error!(target: LOG_TARGET, "failed to format hostname: {error:?}");
            return None;
        }
    };

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
