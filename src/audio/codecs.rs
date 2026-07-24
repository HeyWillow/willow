//! Owned codec interfaces for every supported Willow audio board.

use core::{
    ffi::c_void,
    fmt,
    mem::size_of,
    ptr::{self, NonNull},
};

use log::error;

use crate::i2c;

use super::{
    board::{self, BoardAudioConfiguration, Es7210Profile, MicrophoneCodec, PlaybackCodec},
    codec_ffi::raw,
    es7210::{CodecError as Es7210Error, Es7210},
};

const I2C_PORT: u8 = 0;
const LOG_TARGET: &str = "WILLOW/AUDIO";

#[derive(Debug)]
pub(super) enum CodecError {
    CodecOperation {
        codec: &'static str,
        operation: &'static str,
        status: i32,
    },
    InvalidAmplifierPin {
        codec: &'static str,
        pin: i32,
    },
    InvalidI2cAddress {
        codec: &'static str,
        address: u32,
    },
    InvalidConfigurationSize {
        codec: &'static str,
        bytes: usize,
    },
    MissingCodecOperation {
        codec: &'static str,
        operation: &'static str,
    },
    MissingI2cBus {
        codec: &'static str,
        address: u8,
    },
    NewControlInterface {
        codec: &'static str,
        address: u8,
    },
    NewGpioInterface {
        codec: &'static str,
    },
    NewCodecInterface {
        codec: &'static str,
    },
    Es7210 {
        source: Es7210Error,
    },
    UnsupportedHardware,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CodecOperation {
                codec,
                operation,
                status,
            } => write!(
                formatter,
                "{codec} failed to {operation}: codec status {status}"
            ),
            Self::InvalidAmplifierPin { codec, pin } => {
                write!(
                    formatter,
                    "{codec} amplifier GPIO {pin} does not fit in int16_t"
                )
            }
            Self::InvalidI2cAddress { codec, address } => write!(
                formatter,
                "{codec} control address 0x{address:x} does not fit in uint8_t"
            ),
            Self::InvalidConfigurationSize { codec, bytes } => write!(
                formatter,
                "{codec} configuration size {bytes} does not fit in int"
            ),
            Self::MissingCodecOperation { codec, operation } => {
                write!(
                    formatter,
                    "{codec} does not provide its {operation} operation"
                )
            }
            Self::MissingI2cBus { codec, address } => write!(
                formatter,
                "cannot attach {codec} at I2C address 0x{address:02x}: the shared I2C0 bus is not initialized"
            ),
            Self::NewControlInterface { codec, address } => write!(
                formatter,
                "failed to create the {codec} I2C control interface at address 0x{address:02x}"
            ),
            Self::NewGpioInterface { codec } => {
                write!(formatter, "failed to create the {codec} GPIO interface")
            }
            Self::NewCodecInterface { codec } => {
                write!(formatter, "failed to create the {codec} codec interface")
            }
            Self::Es7210 { source } => write!(formatter, "failed to initialize ES7210: {source}"),
            Self::UnsupportedHardware => {
                formatter.write_str("the selected hardware has no Rust audio configuration")
            }
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Es7210 { source } => Some(source),
            _ => None,
        }
    }
}

struct ControlInterface {
    codec: &'static str,
    address: u8,
    raw: NonNull<raw::audio_codec_ctrl_if_t>,
}

impl ControlInterface {
    fn new(codec: &'static str, address: u8) -> Result<Self, CodecError> {
        let bus = i2c::handle().ok_or(CodecError::MissingI2cBus { codec, address })?;
        let mut configuration = raw::audio_codec_i2c_cfg_t {
            port: I2C_PORT,
            addr: address,
            bus_handle: bus.cast::<c_void>(),
        };
        // SAFETY: the retained I2C0 handle remains live for the firmware
        // lifetime, and the component copies this synchronous configuration.
        let interface = unsafe { raw::audio_codec_new_i2c_ctrl(&raw mut configuration) };
        let raw = NonNull::new(interface.cast_mut())
            .ok_or(CodecError::NewControlInterface { codec, address })?;

        Ok(Self {
            codec,
            address,
            raw,
        })
    }

    const fn as_ptr(&self) -> *const raw::audio_codec_ctrl_if_t {
        self.raw.as_ptr()
    }
}

impl Drop for ControlInterface {
    fn drop(&mut self) {
        // SAFETY: this owner is the only code which deletes the interface,
        // after the dependent codec interface has already been deleted.
        let status = unsafe { raw::audio_codec_delete_ctrl_if(self.raw.as_ptr()) };
        if status != raw::ESP_CODEC_DEV_OK.cast_signed() {
            error!(
                target: LOG_TARGET,
                "failed to delete {} I2C control interface at address 0x{:02x}: codec status {status}",
                self.codec,
                self.address
            );
        }
    }
}

struct GpioInterface {
    codec: &'static str,
    raw: NonNull<raw::audio_codec_gpio_if_t>,
}

impl GpioInterface {
    fn new(codec: &'static str) -> Result<Self, CodecError> {
        // SAFETY: the constructor takes no borrowed data and returns a new
        // allocation, which this owner releases exactly once.
        let interface = unsafe { raw::audio_codec_new_gpio() };
        let raw =
            NonNull::new(interface.cast_mut()).ok_or(CodecError::NewGpioInterface { codec })?;
        Ok(Self { codec, raw })
    }

    const fn as_ptr(&self) -> *const raw::audio_codec_gpio_if_t {
        self.raw.as_ptr()
    }
}

impl Drop for GpioInterface {
    fn drop(&mut self) {
        // SAFETY: this owner is the only code which deletes the interface,
        // after the dependent codec interface has already been deleted.
        let status = unsafe { raw::audio_codec_delete_gpio_if(self.raw.as_ptr()) };
        if status != raw::ESP_CODEC_DEV_OK.cast_signed() {
            error!(
                target: LOG_TARGET,
                "failed to delete {} GPIO interface: codec status {status}",
                self.codec
            );
        }
    }
}

pub(super) struct CodecInterface {
    codec: &'static str,
    raw: NonNull<raw::audio_codec_if_t>,
    control: ControlInterface,
    _gpio: Option<GpioInterface>,
}

impl CodecInterface {
    fn new(
        codec: &'static str,
        interface: *const raw::audio_codec_if_t,
        control: ControlInterface,
        gpio: Option<GpioInterface>,
    ) -> Result<Self, CodecError> {
        let raw =
            NonNull::new(interface.cast_mut()).ok_or(CodecError::NewCodecInterface { codec })?;
        Ok(Self {
            codec,
            raw,
            control,
            _gpio: gpio,
        })
    }

    pub(super) const fn as_ptr(&self) -> *const raw::audio_codec_if_t {
        self.raw.as_ptr()
    }

    pub(super) fn enable(&mut self, enabled: bool) -> Result<(), CodecError> {
        let interface = unsafe { self.raw.as_ref() };
        let Some(enable) = interface.enable else {
            return Ok(());
        };
        let status = unsafe { enable(self.as_ptr(), enabled) };
        self.check("change enabled state", status)
    }

    fn reopen_es7243e(&mut self) -> Result<(), CodecError> {
        let interface = unsafe { self.raw.as_ref() };
        let close = interface.close.ok_or(CodecError::MissingCodecOperation {
            codec: self.codec,
            operation: "close",
        })?;
        let open = interface.open.ok_or(CodecError::MissingCodecOperation {
            codec: self.codec,
            operation: "open",
        })?;
        let status = unsafe { close(self.as_ptr()) };
        self.check("close before reinitialization", status)?;

        let mut configuration = raw::es7243e_codec_cfg_t {
            ctrl_if: self.control.as_ptr(),
        };
        let configuration_bytes = size_of::<raw::es7243e_codec_cfg_t>();
        let configuration_bytes = i32::try_from(configuration_bytes).map_err(|_| {
            CodecError::InvalidConfigurationSize {
                codec: self.codec,
                bytes: configuration_bytes,
            }
        })?;
        let status = unsafe {
            open(
                self.as_ptr(),
                ptr::from_mut(&mut configuration).cast::<c_void>(),
                configuration_bytes,
            )
        };
        self.check("reinitialize", status)
    }

    pub(super) fn configure_playback(&mut self) -> Result<(), CodecError> {
        let interface = unsafe { self.raw.as_ref() };
        if let Some(set_fs) = interface.set_fs {
            let mut sample_information = raw::esp_codec_dev_sample_info_t {
                bits_per_sample: 32,
                channel: 2,
                channel_mask: 0,
                sample_rate: super::capture::SAMPLE_RATE_HZ,
                mclk_multiple: 256,
            };
            let status = unsafe { set_fs(self.as_ptr(), &raw mut sample_information) };
            self.check("configure playback format", status)?;
        }

        // Both playback drivers power their amplifier while opening but keep
        // their internal enabled flag clear. Pass through enabled once so the
        // following disable actually turns the amplifier off.
        self.enable(true)?;
        self.enable(false)
    }

    pub(super) fn set_volume(&mut self, volume: u8) -> Result<(), CodecError> {
        let interface = unsafe { self.raw.as_ref() };
        let Some(set_volume) = interface.set_vol else {
            return Ok(());
        };
        let decibels = if volume == 0 {
            // The codec device subtracts its negative hardware gain before
            // clamping. Stay below that compensation so zero still reaches
            // the old driver's -95.5 dB minimum register.
            -128.0
        } else if volume >= 100 {
            0.0
        } else {
            -50.0 + f32::from(volume) * 0.5
        };
        let status = unsafe { set_volume(self.as_ptr(), decibels) };
        self.check("set playback volume", status)
    }

    fn check(&self, operation: &'static str, status: i32) -> Result<(), CodecError> {
        if status == raw::ESP_CODEC_DEV_OK.cast_signed() {
            Ok(())
        } else {
            Err(CodecError::CodecOperation {
                codec: self.codec,
                operation,
                status,
            })
        }
    }
}

// SAFETY: each codec interface is uniquely owned. Its callbacks are invoked
// only by the worker to which ownership is transferred, and destruction runs
// only after that worker has stopped.
unsafe impl Send for CodecInterface {}

impl Drop for CodecInterface {
    fn drop(&mut self) {
        // SAFETY: this owner is the only code which deletes the interface.
        // Its control and GPIO dependencies are fields and drop afterwards.
        let status = unsafe { raw::audio_codec_delete_codec_if(self.raw.as_ptr()) };
        if status != raw::ESP_CODEC_DEV_OK.cast_signed() {
            error!(
                target: LOG_TARGET,
                "failed to delete {} codec interface: codec status {status}",
                self.codec
            );
        }
    }
}

pub(super) enum MicrophoneDevice {
    Es7210 {
        codec: Es7210,
        profile: Es7210Profile,
    },
    Es7243e(CodecInterface),
}

impl MicrophoneDevice {
    pub(super) fn apply_gain(&mut self, gain: u8) -> Result<(), CodecError> {
        match self {
            Self::Es7210 { codec, profile } => codec
                .set_gain(*profile, gain)
                .map_err(|source| CodecError::Es7210 { source }),
            // Preserve the old ES7243E adapter, whose volume callback was a
            // no-op and therefore left the codec's +30 dB initialization.
            Self::Es7243e(_) => Ok(()),
        }
    }

    pub(super) fn reinitialize(&mut self, gain: u8) -> Result<(), CodecError> {
        match self {
            Self::Es7210 { codec, profile } => {
                codec
                    .initialize(*profile)
                    .map_err(|source| CodecError::Es7210 { source })?;
                codec
                    .set_gain(*profile, gain)
                    .map_err(|source| CodecError::Es7210 { source })
            }
            Self::Es7243e(codec) => codec.reopen_es7243e(),
        }
    }
}

pub(super) struct BoardCodecDevices {
    pub(super) microphone: MicrophoneDevice,
    pub(super) playback: CodecInterface,
}

impl BoardCodecDevices {
    /// Creates the microphone and playback interfaces selected for this board.
    pub(super) fn new() -> Result<Self, CodecError> {
        let configuration = board::selected().ok_or(CodecError::UnsupportedHardware)?;

        // Preserve the existing initialization order: playback DAC first,
        // followed by the microphone ADC.
        let playback = new_playback(configuration)?;
        let microphone = new_microphone(configuration)?;
        Ok(Self {
            microphone,
            playback,
        })
    }
}

fn new_microphone(configuration: &BoardAudioConfiguration) -> Result<MicrophoneDevice, CodecError> {
    match configuration.microphone_codec {
        MicrophoneCodec::Es7210(profile) => {
            let mut codec = Es7210::attach().map_err(|source| CodecError::Es7210 { source })?;
            codec
                .initialize(profile)
                .map_err(|source| CodecError::Es7210 { source })?;
            Ok(MicrophoneDevice::Es7210 { codec, profile })
        }
        MicrophoneCodec::Es7243e => {
            let address = codec_address("ES7243E", raw::ES7243E_CODEC_DEFAULT_ADDR)?;
            let control = ControlInterface::new("ES7243E", address)?;
            let mut codec_configuration = raw::es7243e_codec_cfg_t {
                ctrl_if: control.as_ptr(),
            };
            // SAFETY: the control interface remains live inside the resulting
            // owner and the component copies the synchronous configuration.
            let interface = unsafe { raw::es7243e_codec_new(&raw mut codec_configuration) };
            CodecInterface::new("ES7243E", interface, control, None).map(MicrophoneDevice::Es7243e)
        }
    }
}

fn new_playback(configuration: &BoardAudioConfiguration) -> Result<CodecInterface, CodecError> {
    match configuration.playback_codec {
        PlaybackCodec::Aw88298 => new_aw88298(),
        PlaybackCodec::Es8156 => new_es8156(configuration, amplifier_pin(configuration)?),
        PlaybackCodec::Es8311 => new_es8311(configuration, amplifier_pin(configuration)?),
    }
}

fn amplifier_pin(configuration: &BoardAudioConfiguration) -> Result<i16, CodecError> {
    i16::try_from(configuration.amplifier_enable_gpio).map_err(|_| {
        CodecError::InvalidAmplifierPin {
            codec: playback_name(configuration.playback_codec),
            pin: configuration.amplifier_enable_gpio,
        }
    })
}

fn new_aw88298() -> Result<CodecInterface, CodecError> {
    let address = codec_address("AW88298", raw::AW88298_CODEC_DEFAULT_ADDR)?;
    let control = ControlInterface::new("AW88298", address)?;
    let gpio = GpioInterface::new("AW88298")?;
    let mut codec_configuration = raw::aw88298_codec_cfg_t {
        ctrl_if: control.as_ptr(),
        gpio_if: gpio.as_ptr(),
        // CoreS3 reset is already released through the AW9523 expander.
        reset_pin: -1,
        hw_gain: raw::esp_codec_dev_hw_gain_t {
            pa_gain: 15.0,
            ..Default::default()
        },
    };
    // SAFETY: both dependent interfaces remain live inside the resulting
    // owner and the component copies the synchronous configuration.
    let interface = unsafe { raw::aw88298_codec_new(&raw mut codec_configuration) };
    CodecInterface::new("AW88298", interface, control, Some(gpio))
}

fn new_es8156(
    configuration: &BoardAudioConfiguration,
    amplifier_pin: i16,
) -> Result<CodecInterface, CodecError> {
    let address = codec_address("ES8156", raw::ES8156_CODEC_DEFAULT_ADDR)?;
    let control = ControlInterface::new("ES8156", address)?;
    let gpio = GpioInterface::new("ES8156")?;
    let mut codec_configuration = raw::es8156_codec_cfg_t {
        ctrl_if: control.as_ptr(),
        gpio_if: gpio.as_ptr(),
        pa_pin: amplifier_pin,
        pa_reverted: !configuration.amplifier_enable_active_high,
        hw_gain: raw::esp_codec_dev_hw_gain_t::default(),
    };
    // SAFETY: both dependent interfaces remain live inside the resulting
    // owner and the component copies the synchronous configuration.
    let interface = unsafe { raw::es8156_codec_new(&raw mut codec_configuration) };
    CodecInterface::new("ES8156", interface, control, Some(gpio))
}

fn new_es8311(
    configuration: &BoardAudioConfiguration,
    amplifier_pin: i16,
) -> Result<CodecInterface, CodecError> {
    let address = codec_address("ES8311", raw::ES8311_CODEC_DEFAULT_ADDR)?;
    let control = ControlInterface::new("ES8311", address)?;
    let gpio = GpioInterface::new("ES8311")?;
    let mut codec_configuration = raw::es8311_codec_cfg_t {
        ctrl_if: control.as_ptr(),
        gpio_if: gpio.as_ptr(),
        codec_mode: raw::esp_codec_dec_work_mode_t_ESP_CODEC_DEV_WORK_MODE_DAC,
        pa_pin: amplifier_pin,
        pa_reverted: !configuration.amplifier_enable_active_high,
        master_mode: false,
        use_mclk: true,
        digital_mic: false,
        invert_mclk: false,
        invert_sclk: false,
        hw_gain: raw::esp_codec_dev_hw_gain_t::default(),
        no_dac_ref: false,
        mclk_div: 0,
    };
    // SAFETY: both dependent interfaces remain live inside the resulting
    // owner and the component copies the synchronous configuration.
    let interface = unsafe { raw::es8311_codec_new(&raw mut codec_configuration) };
    CodecInterface::new("ES8311", interface, control, Some(gpio))
}

fn codec_address(codec: &'static str, address: u32) -> Result<u8, CodecError> {
    u8::try_from(address).map_err(|_| CodecError::InvalidI2cAddress { codec, address })
}

const fn playback_name(codec: PlaybackCodec) -> &'static str {
    match codec {
        PlaybackCodec::Aw88298 => "AW88298",
        PlaybackCodec::Es8156 => "ES8156",
        PlaybackCodec::Es8311 => "ES8311",
    }
}
