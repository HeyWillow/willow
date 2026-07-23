//! Raw bindings to the currently vendored standalone codec components.

use core::mem::{align_of, size_of};

pub(super) use esp_idf_sys::audio_codec as raw;

#[cfg(target_pointer_width = "32")]
const _: () = {
    const _: [(); 8] = [(); size_of::<raw::audio_codec_i2c_cfg_t>()];
    const _: [(); 12] = [(); size_of::<raw::audio_codec_i2s_cfg_t>()];
    const _: [(); 12] = [(); size_of::<raw::esp_codec_dev_cfg_t>()];
    const _: [(); 12] = [(); size_of::<raw::esp_codec_dev_sample_info_t>()];
    const _: [(); 12] = [(); size_of::<raw::esp_audio_dec_cfg_t>()];
    const _: [(); 12] = [(); size_of::<raw::esp_audio_dec_in_raw_t>()];
    const _: [(); 16] = [(); size_of::<raw::esp_audio_dec_out_frame_t>()];
    const _: [(); 12] = [(); size_of::<raw::esp_audio_enc_config_t>()];
    const _: [(); 20] = [(); size_of::<raw::esp_audio_enc_info_t>()];
    const _: [(); 12] = [(); size_of::<raw::esp_audio_simple_dec_cfg_t>()];
    const _: [(); 16] = [(); size_of::<raw::esp_audio_simple_dec_raw_t>()];
    const _: [(); 16] = [(); size_of::<raw::esp_audio_simple_dec_out_t>()];
    const _: [(); 4] = [(); align_of::<raw::audio_codec_i2c_cfg_t>()];
    const _: [(); 4] = [(); align_of::<raw::audio_codec_i2s_cfg_t>()];
    const _: [(); 4] = [(); align_of::<raw::esp_codec_dev_cfg_t>()];
    const _: [(); 4] = [(); align_of::<raw::esp_codec_dev_sample_info_t>()];
    const _: [(); 4] = [(); align_of::<raw::esp_audio_dec_cfg_t>()];
    const _: [(); 4] = [(); align_of::<raw::esp_audio_enc_config_t>()];
    const _: [(); 4] = [(); align_of::<raw::esp_audio_simple_dec_cfg_t>()];
};
