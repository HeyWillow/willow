//! `M5Stack CoreS3` power-management and I/O-expander ownership.

use core::fmt;
use std::sync::{Mutex, OnceLock, PoisonError};

use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_sys::{ESP_ERR_INVALID_STATE, EspError};
use log::debug;

use crate::{
    i2c::I2cDevice,
    system::{self, Hardware},
};

const AXP2101_ADDRESS: u16 = 0x34;
const AXP2101_LDO_ENABLE: u8 = 0x90;
const AXP2101_DLDO1_VOLTAGE: u8 = 0x99;
const AW9523_ADDRESS: u16 = 0x58;
const AW9523_DIRECTION: u8 = 0x04;
const AW9523_GLOBAL_CONTROL: u8 = 0x11;
const AW9523_OUTPUT: u8 = 0x02;
const AW9523_RESET: u8 = 0x7f;
const BUS_SPEED_HZ: u32 = 400_000;
const BUS_TIMEOUT_MS: i32 = 1_000;
const LCD_RESET: u16 = 1 << 9;
const LOG_TARGET: &str = "WILLOW/CORES3";
const PERIPHERAL_RESET_DELAY_MS: u32 = 20;
const SPEAKER_AND_MIC_RESET: u16 = 1 << 2;
const SY7088_BOOST_ENABLE: u16 = 1 << 15;
const TOUCH_RESET: u16 = 1;

static BOARD: OnceLock<Mutex<CoreS3>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct CoreS3Error {
    operation: &'static str,
    source: EspError,
}

impl fmt::Display for CoreS3Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.source)
    }
}

impl std::error::Error for CoreS3Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

struct CoreS3 {
    _expander: I2cDevice,
    pmic: I2cDevice,
}

impl CoreS3 {
    fn new() -> Result<Self, CoreS3Error> {
        let mut expander = attach(AW9523_ADDRESS, "attach AW9523 I/O expander")?;
        let mut pmic = attach(AXP2101_ADDRESS, "attach AXP2101 PMIC")?;

        // Hold every expander-controlled peripheral in reset while its supply
        // rails are configured. P0 push-pull mode is required for reliable
        // AW88298 reset control.
        write(&mut expander, AW9523_RESET, &[0], "reset AW9523")?;
        write(
            &mut expander,
            AW9523_GLOBAL_CONTROL,
            &[0x10],
            "configure AW9523 push-pull outputs",
        )?;
        write(
            &mut expander,
            AW9523_DIRECTION,
            &[0, 0],
            "configure AW9523 output directions",
        )?;
        write(
            &mut expander,
            AW9523_OUTPUT,
            &[0, 0],
            "assert CoreS3 peripheral resets",
        )?;

        // This is M5Stack's board bring-up sequence. Willow uses ALDO1 for
        // AW88298 (1.8 V), ALDO2 for ES7210 (3.3 V), and DLDO1 for the LCD
        // backlight. Keeping the remaining factory rails configured avoids
        // surprising the camera and SD hardware attached to the same PMIC.
        for (register, value, operation) in [
            (0x92, 18 - 5, "set AW88298 supply to 1.8 V"),
            (0x93, 33 - 5, "set ES7210 supply to 3.3 V"),
            (0x94, 33 - 5, "set camera supply to 3.3 V"),
            (0x95, 33 - 5, "set SD supply to 3.3 V"),
            (0x27, 0x00, "configure CoreS3 power-key timing"),
            (0x69, 0x11, "configure CoreS3 charge LED"),
            (0x10, 0x30, "configure AXP2101 common controls"),
            (0x30, 0x0f, "enable AXP2101 ADC channels"),
            (
                AXP2101_DLDO1_VOLTAGE,
                0x1c,
                "set LCD backlight supply to 3.3 V",
            ),
            (AXP2101_LDO_ENABLE, 0xbf, "enable CoreS3 LDO rails"),
        ] {
            write(&mut pmic, register, &[value], operation)?;
        }

        let enabled = TOUCH_RESET | SPEAKER_AND_MIC_RESET | LCD_RESET | SY7088_BOOST_ENABLE;
        write(
            &mut expander,
            AW9523_OUTPUT,
            &enabled.to_le_bytes(),
            "release CoreS3 peripheral resets",
        )?;
        FreeRtos::delay_ms(PERIPHERAL_RESET_DELAY_MS);

        Ok(Self {
            _expander: expander,
            pmic,
        })
    }

    fn set_backlight(&mut self, brightness: u32) -> Result<(), CoreS3Error> {
        let millivolts = if brightness == 0 {
            0
        } else {
            2_500 + brightness * 800 / 1_023
        };
        let voltage_code = if millivolts == 0 {
            0
        } else {
            u8::try_from((millivolts - 500) / 100).unwrap_or(0x1c)
        };
        write(
            &mut self.pmic,
            AXP2101_DLDO1_VOLTAGE,
            &[voltage_code],
            "set CoreS3 LCD backlight voltage",
        )?;

        let mut enabled = read(&mut self.pmic, AXP2101_LDO_ENABLE, "read CoreS3 LDO state")?;
        if brightness == 0 {
            enabled &= !0x80;
        } else {
            enabled |= 0x80;
        }
        write(
            &mut self.pmic,
            AXP2101_LDO_ENABLE,
            &[enabled],
            "set CoreS3 LCD backlight state",
        )
    }
}

fn attach(address: u16, operation: &'static str) -> Result<I2cDevice, CoreS3Error> {
    I2cDevice::new(address, BUS_SPEED_HZ).map_err(|source| CoreS3Error { operation, source })
}

fn read(device: &mut I2cDevice, register: u8, operation: &'static str) -> Result<u8, CoreS3Error> {
    let mut value = [0];
    device
        .write_read(&[register], &mut value, BUS_TIMEOUT_MS)
        .map_err(|source| CoreS3Error { operation, source })?;
    Ok(value[0])
}

fn write(
    device: &mut I2cDevice,
    register: u8,
    value: &[u8],
    operation: &'static str,
) -> Result<(), CoreS3Error> {
    let mut command = [0; 3];
    command[0] = register;
    command[1..=value.len()].copy_from_slice(value);
    device
        .write(&command[..=value.len()], BUS_TIMEOUT_MS)
        .map_err(|source| CoreS3Error { operation, source })
}

/// Initializes CoreS3-specific supplies and reset lines when selected.
pub(crate) fn initialize() -> Result<(), CoreS3Error> {
    if system::hardware() != Hardware::M5StackCoreS3 {
        return Ok(());
    }
    if BOARD.get().is_some() {
        return Err(CoreS3Error {
            operation: "initialize CoreS3 board twice",
            source: EspError::from_infallible::<ESP_ERR_INVALID_STATE>(),
        });
    }

    debug!(target: LOG_TARGET, "initializing CoreS3 PMIC and I/O expander");
    BOARD
        .set(Mutex::new(CoreS3::new()?))
        .map_err(|_| CoreS3Error {
            operation: "retain CoreS3 board owner",
            source: EspError::from_infallible::<ESP_ERR_INVALID_STATE>(),
        })
}

/// Applies a 10-bit Willow brightness value through the AXP2101 DLDO1 rail.
pub(crate) fn set_backlight(brightness: u32) -> Result<(), CoreS3Error> {
    let board = BOARD.get().ok_or_else(|| CoreS3Error {
        operation: "set CoreS3 backlight before board initialization",
        source: EspError::from_infallible::<ESP_ERR_INVALID_STATE>(),
    })?;
    board
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_backlight(brightness)
}
