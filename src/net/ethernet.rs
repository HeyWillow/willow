//! W5500 Ethernet initialization and event handling through ESP-IDF.
//!
//! The network interface, driver, MAC, PHY, and SPI bus remain live for the
//! firmware lifetime, matching the previous C implementation. Rust registers
//! callbacks directly with the system event loop retained by Rust's entry
//! point; no callback crosses back into Willow's C implementation.

use core::{ffi::c_void, net::Ipv4Addr, ptr};

use esp_idf_sys::{
    _g_esp_netif_inherent_eth_config, _g_esp_netif_netstack_default_eth, CONFIG_FREERTOS_HZ,
    ESP_ERR_NO_MEM, ESP_EVENT_ANY_ID, ESP_OK, ETH_EVENT, IP_EVENT, esp_err_t, esp_eth_config_t,
    esp_eth_driver_install, esp_eth_handle_t, esp_eth_io_cmd_t_ETH_CMD_G_MAC_ADDR,
    esp_eth_io_cmd_t_ETH_CMD_S_MAC_ADDR, esp_eth_ioctl, esp_eth_mac_new_w5500,
    esp_eth_new_netif_glue, esp_eth_phy_new_w5500, esp_eth_start, esp_event_base_t,
    esp_event_handler_register, esp_netif_attach, esp_netif_config_t, esp_netif_is_netif_up,
    esp_netif_new, eth_event_t_ETHERNET_EVENT_CONNECTED as ETHERNET_EVENT_CONNECTED,
    eth_event_t_ETHERNET_EVENT_DISCONNECTED as ETHERNET_EVENT_DISCONNECTED,
    eth_event_t_ETHERNET_EVENT_START as ETHERNET_EVENT_START,
    eth_event_t_ETHERNET_EVENT_STOP as ETHERNET_EVENT_STOP, eth_mac_config_t, eth_phy_config_t,
    eth_w5500_config_t, ip_event_got_ip_t, ip_event_t_IP_EVENT_ETH_GOT_IP, spi_bus_config_t,
    spi_bus_initialize, spi_common_dma_t_SPI_DMA_CH_AUTO, spi_device_interface_config_t,
    vTaskDelay,
};
use log::{error, info};

use super::{check, log_unhandled};

const ETHERNET_MAC_ADDRESS: [u8; 6] = [0x02, 0x00, 0x00, 0x12, 0x34, 0x56];
const LOG_TARGET: &str = "WILLOW/ETHERNET";
const WILLOW_ETHERNET_CS: i32 = 10;
const WILLOW_ETHERNET_INT: i32 = 14;
const WILLOW_ETHERNET_MISO: i32 = 13;
const WILLOW_ETHERNET_MOSI: i32 = 11;
const WILLOW_ETHERNET_PHY: i32 = 1;
const WILLOW_ETHERNET_RST: i32 = 9;
const WILLOW_ETHERNET_SCLK: i32 = 12;
const WILLOW_ETHERNET_SPI_BUS: u32 = 2;
const WILLOW_ETHERNET_SPI_SPEED: i32 = 36 * 1_000 * 1_000;

/// Builds the Rust equivalent of ESP-IDF's `ETH_DEFAULT_CONFIG()`.
///
/// Bindgen cannot expose function-like C macros. These values mirror the
/// ESP-IDF v5.3.4 macro used by the Cargo build.
fn driver_configuration(
    mac: *mut esp_idf_sys::esp_eth_mac_t,
    phy: *mut esp_idf_sys::esp_eth_phy_t,
) -> esp_eth_config_t {
    esp_eth_config_t {
        check_link_period_ms: 2_000,
        mac,
        on_lowlevel_deinit_done: None,
        on_lowlevel_init_done: None,
        phy,
        read_phy_reg: None,
        stack_input: None,
        write_phy_reg: None,
    }
}

/// Builds the Rust equivalent of ESP-IDF's `ETH_MAC_DEFAULT_CONFIG()`.
fn mac_configuration() -> eth_mac_config_t {
    eth_mac_config_t {
        flags: 0,
        rx_task_prio: 15,
        rx_task_stack_size: 4_096,
        sw_reset_timeout_ms: 100,
    }
}

/// Builds the Rust equivalent of ESP-IDF's `ETH_PHY_DEFAULT_CONFIG()` with
/// Willow's W5500 address and reset pin.
fn phy_configuration() -> eth_phy_config_t {
    eth_phy_config_t {
        autonego_timeout_ms: 4_000,
        phy_addr: WILLOW_ETHERNET_PHY,
        reset_gpio_num: WILLOW_ETHERNET_RST,
        reset_timeout_ms: 100,
    }
}

fn spi_bus_configuration() -> spi_bus_config_t {
    let mut configuration = spi_bus_config_t::default();
    configuration.__bindgen_anon_1.mosi_io_num = WILLOW_ETHERNET_MOSI;
    configuration.__bindgen_anon_2.miso_io_num = WILLOW_ETHERNET_MISO;
    configuration.__bindgen_anon_3.quadwp_io_num = -1;
    configuration.__bindgen_anon_4.quadhd_io_num = -1;
    configuration.sclk_io_num = WILLOW_ETHERNET_SCLK;
    configuration
}

fn spi_device_configuration() -> spi_device_interface_config_t {
    let mut configuration = spi_device_interface_config_t::default();
    configuration.address_bits = 8;
    configuration.clock_speed_hz = WILLOW_ETHERNET_SPI_SPEED;
    configuration.command_bits = 16;
    configuration.mode = 0;
    configuration.queue_size = 20;
    configuration.spics_io_num = WILLOW_ETHERNET_CS;
    configuration
}

/// Builds the Rust equivalent of ESP-IDF's `ETH_W5500_DEFAULT_CONFIG()` with
/// Willow's interrupt pin.
///
/// ESP-IDF v5.3 lets the W5500 driver add its own SPI device from this bus and
/// device configuration. The old C code used the pre-v5.3 handle-based form of
/// the macro, which no longer compiles against the pinned ESP-IDF release.
fn w5500_configuration(spi_device: *mut spi_device_interface_config_t) -> eth_w5500_config_t {
    eth_w5500_config_t {
        custom_spi_driver: Default::default(),
        int_gpio_num: WILLOW_ETHERNET_INT,
        poll_period_ms: 0,
        spi_devcfg: spi_device,
        spi_host_id: WILLOW_ETHERNET_SPI_BUS as _,
    }
}

/// Handles ESP-IDF Ethernet lifecycle events.
///
/// # Safety
///
/// `event_data` must point to a live [`esp_eth_handle_t`] for the duration of
/// known Ethernet events. `event_base` must be null or point to a live
/// NUL-terminated event-base name.
unsafe extern "C" fn ethernet_event_handler(
    _event_handler_arg: *mut c_void,
    event_base: esp_event_base_t,
    event_id: i32,
    event_data: *mut c_void,
) {
    match event_id as u32 {
        ETHERNET_EVENT_CONNECTED => {
            let Some(ethernet_handle) = (unsafe { event_data.cast::<esp_eth_handle_t>().as_ref() })
            else {
                error!(target: LOG_TARGET, "Ethernet event data is null");
                return;
            };

            let mut address = [0_u8; 6];
            let _ = check(
                unsafe {
                    esp_eth_ioctl(
                        *ethernet_handle,
                        esp_eth_io_cmd_t_ETH_CMD_G_MAC_ADDR,
                        address.as_mut_ptr().cast(),
                    )
                },
                "failed to read Ethernet MAC address",
            );
            let [a, b, c, d, e, f] = address;
            info!(target: LOG_TARGET, "Ethernet Link Up");
            info!(target: LOG_TARGET, "Ethernet HW Addr {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}");
        }
        ETHERNET_EVENT_DISCONNECTED => info!(target: LOG_TARGET, "Ethernet Link Down"),
        ETHERNET_EVENT_START => info!(target: LOG_TARGET, "Ethernet Started"),
        ETHERNET_EVENT_STOP => info!(target: LOG_TARGET, "Ethernet Stopped"),
        _ => log_unhandled(event_base, event_id),
    }
}

/// Logs the address assigned to the Ethernet interface.
///
/// # Safety
///
/// For `IP_EVENT_ETH_GOT_IP`, `event_data` must point to a live
/// [`ip_event_got_ip_t`] for the duration of this call. `event_base` must be
/// null or point to a live NUL-terminated event-base name.
unsafe extern "C" fn ip_event_handler(
    _event_handler_arg: *mut c_void,
    event_base: esp_event_base_t,
    event_id: i32,
    event_data: *mut c_void,
) {
    if event_id == ip_event_t_IP_EVENT_ETH_GOT_IP as i32 {
        let Some(event) = (unsafe { event_data.cast::<ip_event_got_ip_t>().as_ref() }) else {
            error!(target: LOG_TARGET, "IP event data is null");
            return;
        };

        let address = Ipv4Addr::from(event.ip_info.ip.addr.to_ne_bytes());
        let gateway = Ipv4Addr::from(event.ip_info.gw.addr.to_ne_bytes());
        let netmask = Ipv4Addr::from(event.ip_info.netmask.addr.to_ne_bytes());
        info!(target: LOG_TARGET, "Ethernet Got IP Address");
        info!(target: LOG_TARGET, "ETHIP: {address}");
        info!(target: LOG_TARGET, "ETHMASK: {netmask}");
        info!(target: LOG_TARGET, "ETHGW: {gateway}");
        return;
    }

    log_unhandled(event_base, event_id);
}

/// Initializes Willow's fixed W5500 Ethernet hardware.
///
/// The Rust entry point initializes the default event loop and the existing C
/// startup path initializes the TCP/IP stack before this function is called.
/// All calls from this point are directly to ESP-IDF; Rust does not call back
/// into Willow C.
#[unsafe(no_mangle)]
pub extern "C" fn rust_ethernet_init() -> esp_err_t {
    let mut network_configuration = unsafe { _g_esp_netif_inherent_eth_config };
    network_configuration.if_desc = c"eth0".as_ptr();
    network_configuration.if_key = c"ETH_SPI_0".as_ptr();
    network_configuration.route_prio = 30;
    let network_interface_configuration = esp_netif_config_t {
        base: &raw const network_configuration,
        driver: ptr::null(),
        stack: unsafe { _g_esp_netif_netstack_default_eth },
    };
    let network_interface = unsafe { esp_netif_new(&raw const network_interface_configuration) };
    if network_interface.is_null() {
        error!(target: LOG_TARGET, "failed to create Ethernet network interface");
        return ESP_ERR_NO_MEM;
    }

    let spi_bus = spi_bus_configuration();
    if let Err(error) = check(
        unsafe {
            spi_bus_initialize(
                WILLOW_ETHERNET_SPI_BUS as _,
                &raw const spi_bus,
                spi_common_dma_t_SPI_DMA_CH_AUTO,
            )
        },
        "failed to initialize Ethernet SPI bus",
    ) {
        return error;
    }

    let mut spi_device = spi_device_configuration();
    let w5500_configuration = w5500_configuration(&raw mut spi_device);
    let mac_configuration = mac_configuration();
    let phy_configuration = phy_configuration();
    let mac = unsafe {
        esp_eth_mac_new_w5500(&raw const w5500_configuration, &raw const mac_configuration)
    };
    let phy = unsafe { esp_eth_phy_new_w5500(&raw const phy_configuration) };
    if mac.is_null() || phy.is_null() {
        error!(target: LOG_TARGET, "failed to create W5500 MAC or PHY");
        return ESP_ERR_NO_MEM;
    }

    let driver_configuration = driver_configuration(mac, phy);
    let mut ethernet_handle = ptr::null_mut();
    if let Err(error) = check(
        unsafe {
            esp_eth_driver_install(&raw const driver_configuration, &raw mut ethernet_handle)
        },
        "failed to install Ethernet driver",
    ) {
        return error;
    }

    let mut address = ETHERNET_MAC_ADDRESS;
    if let Err(error) = check(
        unsafe {
            esp_eth_ioctl(
                ethernet_handle,
                esp_eth_io_cmd_t_ETH_CMD_S_MAC_ADDR,
                address.as_mut_ptr().cast(),
            )
        },
        "failed to set Ethernet MAC address",
    ) {
        return error;
    }

    let glue = unsafe { esp_eth_new_netif_glue(ethernet_handle) };
    if glue.is_null() {
        error!(target: LOG_TARGET, "failed to create Ethernet network glue");
        return ESP_ERR_NO_MEM;
    }
    if let Err(error) = check(
        unsafe { esp_netif_attach(network_interface, glue.cast()) },
        "failed to attach Ethernet network interface",
    ) {
        return error;
    }

    if let Err(error) = check(
        unsafe {
            esp_event_handler_register(
                ETH_EVENT,
                ESP_EVENT_ANY_ID,
                Some(ethernet_event_handler),
                ptr::null_mut(),
            )
        },
        "failed to register Ethernet event handler",
    ) {
        return error;
    }
    if let Err(error) = check(
        unsafe {
            esp_event_handler_register(
                IP_EVENT,
                ip_event_t_IP_EVENT_ETH_GOT_IP as i32,
                Some(ip_event_handler),
                ptr::null_mut(),
            )
        },
        "failed to register Ethernet IP event handler",
    ) {
        return error;
    }

    if let Err(error) = check(
        unsafe { esp_eth_start(ethernet_handle) },
        "failed to start Ethernet",
    ) {
        return error;
    }

    while !unsafe { esp_netif_is_netif_up(network_interface) } {
        info!(target: LOG_TARGET, "Waiting on Ethernet...");
        unsafe {
            vTaskDelay(CONFIG_FREERTOS_HZ / 2);
        }
    }

    // Preserve the original settling delay until runtime testing proves it is
    // unnecessary.
    unsafe {
        vTaskDelay(CONFIG_FREERTOS_HZ * 5);
    }

    ESP_OK
}
