//! ESP-SR model and audio-front-end integration.

use core::mem::{align_of, size_of};

use esp_idf_sys::esp_sr;

#[cfg(target_pointer_width = "32")]
const _: () = {
    const _: [(); 16] = [(); size_of::<esp_sr::afe_pcm_config_t>()];
    const _: [(); 8] = [(); size_of::<esp_sr::afe_debug_hook_t>()];
    const _: [(); 104] = [(); size_of::<esp_sr::afe_config_t>()];
    const _: [(); 44] = [(); size_of::<esp_sr::afe_fetch_result_t>()];
    const _: [(); 68] = [(); size_of::<esp_sr::esp_afe_sr_iface_t>()];
    const _: [(); 16] = [(); size_of::<esp_sr::srmodel_data_t>()];
    const _: [(); 24] = [(); size_of::<esp_sr::srmodel_list_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_pcm_config_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_debug_hook_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_config_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_fetch_result_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::esp_afe_sr_iface_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::srmodel_data_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::srmodel_list_t>()];
};
