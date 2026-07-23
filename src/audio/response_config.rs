//! Active configuration adapter for the pure audio response policy.

#![allow(
    dead_code,
    reason = "the response policy remains inactive until Rust owns runtime audio"
)]

use log::error;
use willow_schema::config::v1::AudioResponseType;

use super::response::{ResponseMode, ResponsePolicy};

const DEFAULT_WIS_TTS_URL: &str = "https://infer.tovera.io/api/tts";
const LOG_TARGET: &str = "WILLOW/AUDIO";

pub(super) fn prepare_playback() {
    if let Err(source) = crate::backlight::reset_display_timer(false) {
        error!(target: LOG_TARGET, "failed to schedule display timeout for response audio: {source:#?}");
    }
    crate::backlight::set(true, false);
}

pub(super) fn active_policy() -> ResponsePolicy<'static> {
    let configuration = crate::config::config();
    let mode = match configuration.and_then(|config| config.audio_response_type) {
        Some(AudioResponseType::Chimes) => ResponseMode::Chimes,
        Some(AudioResponseType::Tts) => ResponseMode::Tts,
        Some(AudioResponseType::None) | None => ResponseMode::None,
    };
    let legacy_tts_url = configuration
        .and_then(|config| config.wis_tts_url.as_deref())
        .unwrap_or(DEFAULT_WIS_TTS_URL);
    let tts_url_v2 = configuration.and_then(|config| config.wis_tts_url_v2.as_deref());

    ResponsePolicy::new(mode, legacy_tts_url, tts_url_v2)
}
