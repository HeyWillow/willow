//! Audio hardware descriptors for every supported Willow board.

use esp_idf_sys::{
    gpio_num_t_GPIO_NUM_0, gpio_num_t_GPIO_NUM_1, gpio_num_t_GPIO_NUM_2, gpio_num_t_GPIO_NUM_13,
    gpio_num_t_GPIO_NUM_14, gpio_num_t_GPIO_NUM_15, gpio_num_t_GPIO_NUM_16, gpio_num_t_GPIO_NUM_17,
    gpio_num_t_GPIO_NUM_33, gpio_num_t_GPIO_NUM_34, gpio_num_t_GPIO_NUM_45, gpio_num_t_GPIO_NUM_46,
    gpio_num_t_GPIO_NUM_47, gpio_num_t_GPIO_NUM_NC, i2s_port_t, i2s_port_t_I2S_NUM_0,
    i2s_port_t_I2S_NUM_1,
};

use crate::system::{self, Hardware};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MicrophoneCodec {
    Es7210(Es7210Profile),
    Es7243e,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Es7210Profile {
    Box3,
    CoreS3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlaybackCodec {
    Aw88298,
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
    pub(super) i2s_port: i2s_port_t,
}

const COMMON_I2S_PINS: I2sPins = I2sPins {
    master_clock: gpio_num_t_GPIO_NUM_2,
    bit_clock: gpio_num_t_GPIO_NUM_17,
    word_select: gpio_num_t_GPIO_NUM_47,
    data_out: gpio_num_t_GPIO_NUM_15,
    data_in: gpio_num_t_GPIO_NUM_16,
};

const ESP32_S3_BOX: BoardAudioConfiguration = BoardAudioConfiguration {
    microphone_codec: MicrophoneCodec::Es7210(Es7210Profile::Box3),
    playback_codec: PlaybackCodec::Es8311,
    i2s: COMMON_I2S_PINS,
    amplifier_enable_gpio: gpio_num_t_GPIO_NUM_46,
    amplifier_enable_active_high: true,
    mute_gpio: gpio_num_t_GPIO_NUM_1,
    mute_active_low: true,
    hardware_aec: true,
    i2s_port: i2s_port_t_I2S_NUM_0,
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
    i2s_port: i2s_port_t_I2S_NUM_0,
};

const ESP32_S3_BOX_3: BoardAudioConfiguration = BoardAudioConfiguration {
    microphone_codec: MicrophoneCodec::Es7210(Es7210Profile::Box3),
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
    i2s_port: i2s_port_t_I2S_NUM_0,
};

const M5STACK_CORE_S3: BoardAudioConfiguration = BoardAudioConfiguration {
    microphone_codec: MicrophoneCodec::Es7210(Es7210Profile::CoreS3),
    playback_codec: PlaybackCodec::Aw88298,
    i2s: I2sPins {
        master_clock: gpio_num_t_GPIO_NUM_0,
        bit_clock: gpio_num_t_GPIO_NUM_34,
        word_select: gpio_num_t_GPIO_NUM_33,
        data_out: gpio_num_t_GPIO_NUM_13,
        data_in: gpio_num_t_GPIO_NUM_14,
    },
    amplifier_enable_gpio: gpio_num_t_GPIO_NUM_NC,
    amplifier_enable_active_high: true,
    mute_gpio: gpio_num_t_GPIO_NUM_NC,
    mute_active_low: true,
    hardware_aec: false,
    i2s_port: i2s_port_t_I2S_NUM_1,
};

/// Returns the audio descriptor selected by the existing board configuration.
pub(super) const fn selected() -> Option<&'static BoardAudioConfiguration> {
    match system::hardware() {
        Hardware::Esp32S3Box => Some(&ESP32_S3_BOX),
        Hardware::Esp32S3Box3 => Some(&ESP32_S3_BOX_3),
        Hardware::Esp32S3BoxLite => Some(&ESP32_S3_BOX_LITE),
        Hardware::M5StackCoreS3 => Some(&M5STACK_CORE_S3),
        Hardware::Unsupported => None,
    }
}
