//! Proven ES7210 microphone ADC initialization and verification.

use core::fmt;

use esp_idf_sys::EspError;

use crate::i2c::I2cDevice;

const ADDRESS: u16 = 0x40;
const BUS_SPEED_HZ: u32 = 100_000;
const BUS_TIMEOUT_MS: i32 = 100;
const BOX3_GAIN_CODE: u8 = 0x0e;

const RESET: u8 = 0x00;
const CLOCK_OFF: u8 = 0x01;
const MAIN_CLOCK: u8 = 0x02;
const LRCLK_DIV_HIGH: u8 = 0x04;
const LRCLK_DIV_LOW: u8 = 0x05;
const OSR: u8 = 0x07;
const MODE_CONFIG: u8 = 0x08;
const TIME_CONTROL_0: u8 = 0x09;
const TIME_CONTROL_1: u8 = 0x0a;
const SERIAL_INTERFACE_1: u8 = 0x11;
const SERIAL_INTERFACE_2: u8 = 0x12;
const ADC34_HPF_2: u8 = 0x20;
const ADC34_HPF_1: u8 = 0x21;
const ADC12_HPF_1: u8 = 0x22;
const ADC12_HPF_2: u8 = 0x23;
const ANALOG: u8 = 0x40;
const MIC12_BIAS: u8 = 0x41;
const MIC34_BIAS: u8 = 0x42;
const MIC1_GAIN: u8 = 0x43;
const MIC2_GAIN: u8 = 0x44;
const MIC3_GAIN: u8 = 0x45;
const MIC4_GAIN: u8 = 0x46;
const MIC12_POWER: u8 = 0x4b;
const MIC34_POWER: u8 = 0x4c;

#[derive(Debug)]
pub(super) enum CodecError {
    Attach {
        address: u16,
        bus_speed_hz: u32,
        source: EspError,
    },
    Bus {
        operation: &'static str,
        register: u8,
        source: EspError,
    },
    Readback {
        register: u8,
        mask: u8,
        expected: u8,
        actual: u8,
    },
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attach {
                address,
                bus_speed_hz,
                source,
            } => write!(
                formatter,
                "failed to attach ES7210 at 0x{address:02x} to the {bus_speed_hz} Hz I2C0 bus: {source}"
            ),
            Self::Bus {
                operation,
                register,
                source,
            } => write!(
                formatter,
                "ES7210 {operation} failed at register 0x{register:02x}: {source}"
            ),
            Self::Readback {
                register,
                mask,
                expected,
                actual,
            } => write!(
                formatter,
                "ES7210 register 0x{register:02x} read 0x{actual:02x}; expected 0x{expected:02x} under mask 0x{mask:02x}"
            ),
        }
    }
}

impl std::error::Error for CodecError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Es7210Snapshot {
    pub(super) reset: u8,
    pub(super) clock_off: u8,
    pub(super) main_clock: u8,
    pub(super) lrclk_div_high: u8,
    pub(super) lrclk_div_low: u8,
    pub(super) osr: u8,
    pub(super) mode_config: u8,
    pub(super) serial_interface_1: u8,
    pub(super) serial_interface_2: u8,
    pub(super) analog: u8,
    pub(super) mic12_bias: u8,
    pub(super) mic34_bias: u8,
    pub(super) mic1_gain: u8,
    pub(super) mic2_gain: u8,
    pub(super) mic3_gain: u8,
    pub(super) mic4_gain: u8,
    pub(super) mic12_power: u8,
    pub(super) mic34_power: u8,
}

pub(super) struct Es7210 {
    bus: I2cDevice,
}

impl Es7210 {
    /// Attaches the ES7210 to Willow's shared I2C0 master bus.
    pub(super) fn attach() -> Result<Self, CodecError> {
        let bus = I2cDevice::new(ADDRESS, BUS_SPEED_HZ).map_err(|source| CodecError::Attach {
            address: ADDRESS,
            bus_speed_hz: BUS_SPEED_HZ,
            source,
        })?;
        Ok(Self { bus })
    }

    /// Applies and verifies Willow's proven BOX-3 microphone configuration.
    pub(super) fn initialize_box3(&mut self) -> Result<Es7210Snapshot, CodecError> {
        // This is Willow's known BOX-3 sequence, specialized directly to its
        // final 16 kHz, 32-bit, MIC1|MIC2|MIC3 configuration.
        self.write(RESET, 0xff)?;
        self.write(RESET, 0x41)?;
        self.write(CLOCK_OFF, 0x3f)?;
        self.write(TIME_CONTROL_0, 0x30)?;
        self.write(TIME_CONTROL_1, 0x30)?;
        self.write(ADC12_HPF_2, 0x2a)?;
        self.write(ADC12_HPF_1, 0x0a)?;
        self.write(ADC34_HPF_2, 0x0a)?;
        self.write(ADC34_HPF_1, 0x2a)?;

        // ESP32-S3 supplies MCLK/BCLK/LRCLK; ES7210 remains a clock target.
        self.update(MODE_CONFIG, 0x01, 0x00)?;
        self.write(ANALOG, 0x43)?;
        self.write(MIC12_BIAS, 0x70)?;
        self.write(MIC34_BIAS, 0x70)?;

        // MCLK=4.096 MHz and LRCLK=16 kHz coefficient from the proven driver.
        self.write(OSR, 0x20)?;
        self.write(MAIN_CLOCK, 0xc1)?;
        self.configure_16khz()?;

        // Preserve Willow's exact BOX-3 input selection and power-bank order.
        self.select_box3_inputs()?;
        self.set_selected_gain(0x08)?;
        self.configure_32bit_philips()?;
        self.configure_16khz()?;
        self.set_selected_gain(BOX3_GAIN_CODE)?;

        let snapshot = self.snapshot()?;
        Self::verify_snapshot(snapshot)?;
        Ok(snapshot)
    }

    /// Applies Willow's configured ES7210 gain code and verifies every
    /// selected microphone register.
    pub(super) fn set_gain(&mut self, gain: u8) -> Result<(), CodecError> {
        let gain = gain.min(BOX3_GAIN_CODE);
        self.set_selected_gain(gain)?;
        for register in [MIC1_GAIN, MIC2_GAIN, MIC3_GAIN] {
            let actual = self.read(register)?;
            if actual & 0x0f != gain {
                return Err(CodecError::Readback {
                    register,
                    mask: 0x0f,
                    expected: gain,
                    actual,
                });
            }
        }
        Ok(())
    }

    fn configure_16khz(&mut self) -> Result<(), CodecError> {
        self.write(MAIN_CLOCK, 0xc1)?;
        self.write(OSR, 0x20)?;
        self.write(LRCLK_DIV_HIGH, 0x01)?;
        self.write(LRCLK_DIV_LOW, 0x00)
    }

    fn configure_32bit_philips(&mut self) -> Result<(), CodecError> {
        let width = (self.read(SERIAL_INTERFACE_1)? & 0x1f) | 0x80;
        self.write(SERIAL_INTERFACE_1, width)?;
        let philips = self.read(SERIAL_INTERFACE_1)? & 0xfc;
        self.write(SERIAL_INTERFACE_1, philips)
    }

    fn select_box3_inputs(&mut self) -> Result<(), CodecError> {
        // MIC1/MIC2 are the two physical microphone capsules. MIC3 is the
        // schematic's ES8311 DAC loopback for future AEC, not another mic.
        // Keep it selected only to preserve Willow's known-working register
        // sequence and serialization mode during this migration. The validated
        // 32-bit capture exposes only the two microphone words; it must not
        // label either low halfword as a MIC3/reference sample.
        for register in MIC1_GAIN..=MIC4_GAIN {
            self.update(register, 0x10, 0x00)?;
        }
        self.write(MIC12_POWER, 0xff)?;
        self.write(MIC34_POWER, 0xff)?;

        self.update(CLOCK_OFF, 0x0b, 0x00)?;
        self.write(MIC12_POWER, 0x00)?;
        self.update(MIC1_GAIN, 0x10, 0x10)?;
        self.update(MIC1_GAIN, 0x0f, 0x00)?;

        self.update(CLOCK_OFF, 0x0b, 0x00)?;
        self.write(MIC12_POWER, 0x00)?;
        self.update(MIC2_GAIN, 0x10, 0x10)?;
        self.update(MIC2_GAIN, 0x0f, 0x00)?;

        self.update(CLOCK_OFF, 0x15, 0x00)?;
        self.write(MIC34_POWER, 0x00)?;
        self.update(MIC3_GAIN, 0x10, 0x10)?;
        self.update(MIC3_GAIN, 0x0f, 0x00)?;

        // This is ES7210 serialization inside two standard I2S slots. The S3
        // peripheral itself deliberately remains in standard, not TDM, mode.
        self.write(SERIAL_INTERFACE_2, 0x02)
    }

    fn set_selected_gain(&mut self, gain: u8) -> Result<(), CodecError> {
        for register in [MIC1_GAIN, MIC2_GAIN, MIC3_GAIN] {
            self.update(register, 0x0f, gain)?;
        }
        Ok(())
    }

    fn snapshot(&mut self) -> Result<Es7210Snapshot, CodecError> {
        Ok(Es7210Snapshot {
            reset: self.read(RESET)?,
            clock_off: self.read(CLOCK_OFF)?,
            main_clock: self.read(MAIN_CLOCK)?,
            lrclk_div_high: self.read(LRCLK_DIV_HIGH)?,
            lrclk_div_low: self.read(LRCLK_DIV_LOW)?,
            osr: self.read(OSR)?,
            mode_config: self.read(MODE_CONFIG)?,
            serial_interface_1: self.read(SERIAL_INTERFACE_1)?,
            serial_interface_2: self.read(SERIAL_INTERFACE_2)?,
            analog: self.read(ANALOG)?,
            mic12_bias: self.read(MIC12_BIAS)?,
            mic34_bias: self.read(MIC34_BIAS)?,
            mic1_gain: self.read(MIC1_GAIN)?,
            mic2_gain: self.read(MIC2_GAIN)?,
            mic3_gain: self.read(MIC3_GAIN)?,
            mic4_gain: self.read(MIC4_GAIN)?,
            mic12_power: self.read(MIC12_POWER)?,
            mic34_power: self.read(MIC34_POWER)?,
        })
    }

    fn verify_snapshot(snapshot: Es7210Snapshot) -> Result<(), CodecError> {
        for (register, actual, mask, expected) in [
            (RESET, snapshot.reset, 0xff, 0x41),
            (CLOCK_OFF, snapshot.clock_off, 0xff, 0x20),
            (MAIN_CLOCK, snapshot.main_clock, 0xff, 0xc1),
            (LRCLK_DIV_HIGH, snapshot.lrclk_div_high, 0xff, 0x01),
            (LRCLK_DIV_LOW, snapshot.lrclk_div_low, 0xff, 0x00),
            (OSR, snapshot.osr, 0xff, 0x20),
            (MODE_CONFIG, snapshot.mode_config, 0x01, 0x00),
            (SERIAL_INTERFACE_1, snapshot.serial_interface_1, 0xe3, 0x80),
            (SERIAL_INTERFACE_2, snapshot.serial_interface_2, 0xff, 0x02),
            (ANALOG, snapshot.analog, 0xff, 0x43),
            (MIC12_BIAS, snapshot.mic12_bias, 0xff, 0x70),
            (MIC34_BIAS, snapshot.mic34_bias, 0xff, 0x70),
            (MIC1_GAIN, snapshot.mic1_gain, 0x1f, 0x1e),
            (MIC2_GAIN, snapshot.mic2_gain, 0x1f, 0x1e),
            (MIC3_GAIN, snapshot.mic3_gain, 0x1f, 0x1e),
            (MIC4_GAIN, snapshot.mic4_gain, 0x10, 0x00),
            (MIC12_POWER, snapshot.mic12_power, 0xff, 0x00),
            (MIC34_POWER, snapshot.mic34_power, 0xff, 0x00),
        ] {
            if actual & mask != expected & mask {
                return Err(CodecError::Readback {
                    register,
                    mask,
                    expected,
                    actual,
                });
            }
        }
        Ok(())
    }

    fn update(&mut self, register: u8, mask: u8, value: u8) -> Result<(), CodecError> {
        let current = self.read(register)?;
        self.write(register, (current & !mask) | (value & mask))
    }

    fn read(&mut self, register: u8) -> Result<u8, CodecError> {
        let mut value = [0_u8];
        self.bus
            .write_read(&[register], &mut value, BUS_TIMEOUT_MS)
            .map_err(|source| CodecError::Bus {
                operation: "read",
                register,
                source,
            })?;
        Ok(value[0])
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), CodecError> {
        self.bus
            .write(&[register, value], BUS_TIMEOUT_MS)
            .map_err(|source| CodecError::Bus {
                operation: "write",
                register,
                source,
            })
    }
}
