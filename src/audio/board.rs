//! Audio hardware descriptors for every supported ESP32-S3-BOX board.

use esp_idf_sys::{
    gpio_num_t_GPIO_NUM_1, gpio_num_t_GPIO_NUM_2, gpio_num_t_GPIO_NUM_15, gpio_num_t_GPIO_NUM_16,
    gpio_num_t_GPIO_NUM_17, gpio_num_t_GPIO_NUM_45, gpio_num_t_GPIO_NUM_46, gpio_num_t_GPIO_NUM_47,
};

use crate::system::{self, Hardware};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MicrophoneCodec {
    Es7210,
    Es7243e,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlaybackCodec {
    Es8156,
    Es8311,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct I2sPins {
    pub(super) master_clock: i32,
    pub(super) bit_clock: i32,
    pub(super) word_select: i32,
    pub(super) data_out: i32,
    pub(super) data_in: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BoardAudioConfiguration {
    pub(super) microphone_codec: MicrophoneCodec,
    pub(super) playback_codec: PlaybackCodec,
    pub(super) i2s: I2sPins,
    pub(super) amplifier_enable_gpio: i32,
    pub(super) amplifier_enable_active_high: bool,
    pub(super) mute_gpio: i32,
    pub(super) mute_active_low: bool,
    pub(super) hardware_aec: bool,
}

const COMMON_I2S_PINS: I2sPins = I2sPins {
    master_clock: gpio_num_t_GPIO_NUM_2,
    bit_clock: gpio_num_t_GPIO_NUM_17,
    word_select: gpio_num_t_GPIO_NUM_47,
    data_out: gpio_num_t_GPIO_NUM_15,
    data_in: gpio_num_t_GPIO_NUM_16,
};

const ESP32_S3_BOX: BoardAudioConfiguration = BoardAudioConfiguration {
    microphone_codec: MicrophoneCodec::Es7210,
    playback_codec: PlaybackCodec::Es8311,
    i2s: COMMON_I2S_PINS,
    amplifier_enable_gpio: gpio_num_t_GPIO_NUM_46,
    amplifier_enable_active_high: true,
    mute_gpio: gpio_num_t_GPIO_NUM_1,
    mute_active_low: true,
    hardware_aec: true,
};

const ESP32_S3_BOX_LITE: BoardAudioConfiguration = BoardAudioConfiguration {
    microphone_codec: MicrophoneCodec::Es7243e,
    playback_codec: PlaybackCodec::Es8156,
    i2s: COMMON_I2S_PINS,
    amplifier_enable_gpio: gpio_num_t_GPIO_NUM_46,
    amplifier_enable_active_high: true,
    mute_gpio: gpio_num_t_GPIO_NUM_1,
    mute_active_low: true,
    hardware_aec: false,
};

const ESP32_S3_BOX_3: BoardAudioConfiguration = BoardAudioConfiguration {
    microphone_codec: MicrophoneCodec::Es7210,
    playback_codec: PlaybackCodec::Es8311,
    i2s: I2sPins {
        word_select: gpio_num_t_GPIO_NUM_45,
        ..COMMON_I2S_PINS
    },
    amplifier_enable_gpio: gpio_num_t_GPIO_NUM_46,
    amplifier_enable_active_high: true,
    mute_gpio: gpio_num_t_GPIO_NUM_1,
    mute_active_low: true,
    hardware_aec: true,
};

/// Returns the audio descriptor selected by the existing board configuration.
pub(super) const fn selected() -> Option<&'static BoardAudioConfiguration> {
    match system::hardware() {
        Hardware::Esp32S3Box => Some(&ESP32_S3_BOX),
        Hardware::Esp32S3Box3 => Some(&ESP32_S3_BOX_3),
        Hardware::Esp32S3BoxLite => Some(&ESP32_S3_BOX_LITE),
        Hardware::Unsupported => None,
    }
}
