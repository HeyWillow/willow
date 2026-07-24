//! I2C0 master bus ownership.
//!
//! Rust retains the native ESP-IDF bus for the firmware lifetime. Touch,
//! display, and audio codecs borrow its handle.

use core::ptr;
use std::sync::OnceLock;

use esp_idf_sys::{
    ESP_ERR_INVALID_ARG, ESP_ERR_INVALID_STATE, EspError, esp_err_t, gpio_num_t_GPIO_NUM_8,
    gpio_num_t_GPIO_NUM_18, i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7, i2c_del_master_bus,
    i2c_device_config_t, i2c_master_bus_add_device, i2c_master_bus_config_t,
    i2c_master_bus_config_t__bindgen_ty_1, i2c_master_bus_config_t__bindgen_ty_2,
    i2c_master_bus_handle_t, i2c_master_bus_rm_device, i2c_master_dev_handle_t, i2c_master_probe,
    i2c_master_transmit, i2c_master_transmit_receive, i2c_new_master_bus, i2c_port_num_t,
    i2c_port_t_I2C_NUM_0, soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT,
};
use log::{debug, error};

const GLITCH_IGNORE_COUNT: u8 = 7;
const LOG_TARGET: &str = "WILLOW/I2C";
const PROBE_TIMEOUT_MS: i32 = 200;

struct I2cBus {
    handle: usize,
}

impl I2cBus {
    fn handle(&self) -> i2c_master_bus_handle_t {
        self.handle as i2c_master_bus_handle_t
    }
}

impl Drop for I2cBus {
    fn drop(&mut self) {
        if !self.handle().is_null() {
            let result = unsafe { i2c_del_master_bus(self.handle()) };
            if let Some(error) = EspError::from(result) {
                error!(target: LOG_TARGET, "failed to delete I2C master bus: {error}");
            }
        }
    }
}

static I2C_BUS: OnceLock<I2cBus> = OnceLock::new();

/// An addressed device attached to the shared Rust-owned I2C0 master bus.
pub(crate) struct I2cDevice {
    address: u16,
    handle: usize,
}

impl I2cDevice {
    /// Attaches a seven-bit-addressed device to the initialized shared bus.
    pub(crate) fn new(address: u16, scl_speed_hz: u32) -> Result<Self, EspError> {
        let bus = handle().ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
        let configuration = i2c_device_config_t {
            dev_addr_length: i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7,
            device_address: address,
            scl_speed_hz,
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        // SAFETY: the retained bus handle is live, and both configuration and
        // output storage remain valid for this synchronous call.
        esp_result(unsafe {
            i2c_master_bus_add_device(bus, &raw const configuration, &raw mut handle)
        })?;
        if handle.is_null() {
            return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
        }

        Ok(Self {
            address,
            handle: handle as usize,
        })
    }

    /// Writes one complete transaction to this device.
    pub(crate) fn write(&mut self, bytes: &[u8], timeout_ms: i32) -> Result<(), EspError> {
        // SAFETY: this owner contains a live device handle. No asynchronous
        // callback is installed, so the input slice need only outlive the call.
        esp_result(unsafe {
            i2c_master_transmit(self.handle(), bytes.as_ptr(), bytes.len(), timeout_ms)
        })
    }

    /// Writes and then reads in one transaction with a repeated start.
    pub(crate) fn write_read(
        &mut self,
        bytes: &[u8],
        output: &mut [u8],
        timeout_ms: i32,
    ) -> Result<(), EspError> {
        // SAFETY: this owner contains a live device handle. The input and
        // output slices remain valid for the complete synchronous transaction.
        esp_result(unsafe {
            i2c_master_transmit_receive(
                self.handle(),
                bytes.as_ptr(),
                bytes.len(),
                output.as_mut_ptr(),
                output.len(),
                timeout_ms,
            )
        })
    }

    fn handle(&self) -> i2c_master_dev_handle_t {
        self.handle as i2c_master_dev_handle_t
    }
}

impl Drop for I2cDevice {
    fn drop(&mut self) {
        // SAFETY: only this owner can remove its live device handle, and all
        // operations are synchronous mutable borrows which have ended.
        if let Err(error) = esp_result(unsafe { i2c_master_bus_rm_device(self.handle()) }) {
            error!(
                target: LOG_TARGET,
                "failed to remove I2C device at 0x{:02x}: {error}",
                self.address
            );
        }
    }
}

pub(crate) fn handle() -> Option<i2c_master_bus_handle_t> {
    I2C_BUS.get().map(I2cBus::handle)
}

pub(crate) fn probe(address: u16) -> esp_err_t {
    let Some(bus) = I2C_BUS.get() else {
        return ESP_ERR_INVALID_STATE;
    };

    unsafe { i2c_master_probe(bus.handle(), address, PROBE_TIMEOUT_MS) }
}

fn check(result: esp_err_t, operation: &str) -> Result<(), EspError> {
    if let Some(error) = EspError::from(result) {
        error!(target: LOG_TARGET, "{operation}: {error}");
        Err(error)
    } else {
        Ok(())
    }
}

fn esp_result(result: esp_err_t) -> Result<(), EspError> {
    EspError::from(result).map_or(Ok(()), Err)
}

/// Initializes and retains the I2C0 master bus.
pub fn init() -> Result<(), EspError> {
    if I2C_BUS.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    debug!(target: LOG_TARGET, "initializing I2C0 master bus in Rust");

    let mut flags = i2c_master_bus_config_t__bindgen_ty_2::default();
    flags.set_enable_internal_pullup(1);
    let i2c_port: i2c_port_num_t = i32::try_from(i2c_port_t_I2C_NUM_0)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_ARG>())?;
    let configuration = i2c_master_bus_config_t {
        i2c_port,
        sda_io_num: gpio_num_t_GPIO_NUM_8,
        scl_io_num: gpio_num_t_GPIO_NUM_18,
        __bindgen_anon_1: i2c_master_bus_config_t__bindgen_ty_1 {
            clk_source: soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT,
        },
        glitch_ignore_cnt: GLITCH_IGNORE_COUNT,
        intr_priority: 0,
        trans_queue_depth: 0,
        flags,
    };
    let mut handle = ptr::null_mut();
    check(
        unsafe { i2c_new_master_bus(&raw const configuration, &raw mut handle) },
        "failed to initialize I2C0 master bus",
    )?;

    I2C_BUS
        .set(I2cBus {
            handle: handle as usize,
        })
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}
