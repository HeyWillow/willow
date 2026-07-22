//! I2C0 master bus ownership.
//!
//! Rust retains the native ESP-IDF bus for the firmware lifetime. The existing
//! Rust touch and the retained ADF codec paths borrow its handle.

use core::ptr;
use std::sync::OnceLock;

use esp_idf_sys::{
    ESP_ERR_INVALID_STATE, EspError, esp_err_t, gpio_num_t_GPIO_NUM_8, gpio_num_t_GPIO_NUM_18,
    i2c_del_master_bus, i2c_master_bus_config_t, i2c_master_bus_config_t__bindgen_ty_1,
    i2c_master_bus_config_t__bindgen_ty_2, i2c_master_bus_handle_t, i2c_master_probe,
    i2c_new_master_bus, i2c_port_num_t, i2c_port_t_I2C_NUM_0,
    soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT,
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

/// Initializes and retains the I2C0 master bus.
pub fn init() -> Result<(), EspError> {
    if I2C_BUS.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    debug!(target: LOG_TARGET, "initializing I2C0 master bus in Rust");

    let mut flags = i2c_master_bus_config_t__bindgen_ty_2::default();
    flags.set_enable_internal_pullup(1);
    let configuration = i2c_master_bus_config_t {
        i2c_port: i2c_port_t_I2C_NUM_0 as i2c_port_num_t,
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
        unsafe { i2c_new_master_bus(&configuration, &mut handle) },
        "failed to initialize I2C0 master bus",
    )?;

    I2C_BUS
        .set(I2cBus {
            handle: handle as usize,
        })
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

/// Returns a borrowed I2C0 master handle owned by Rust for the firmware lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn rust_i2c_master_handle() -> i2c_master_bus_handle_t {
    handle().unwrap_or_default()
}

/// Probes a seven-bit address on the Rust-owned I2C0 bus.
#[unsafe(no_mangle)]
pub extern "C" fn rust_i2c_probe(address: u16) -> esp_err_t {
    probe(address)
}
