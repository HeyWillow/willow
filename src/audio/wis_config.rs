//! Active configuration adapter for WIS audio encoding.

#![allow(
    dead_code,
    reason = "WIS streaming remains inactive until Rust owns runtime audio"
)]

use willow_schema::config::v1::AudioCodec;

use super::wis_framing::WisFormat;

const DEFAULT_WIS_URL: &str = "https://infer.tovera.io/api/willow";

pub(super) fn active_format() -> WisFormat {
    match crate::config::config().and_then(|config| config.audio_codec) {
        Some(AudioCodec::AmrWb) => WisFormat::AmrWb,
        Some(AudioCodec::Wav) => WisFormat::Wav,
        Some(AudioCodec::Pcm) | None => WisFormat::Pcm,
    }
}

pub(super) fn active_url() -> &'static str {
    crate::config::config()
        .and_then(|config| config.wis_url.as_deref())
        .unwrap_or(DEFAULT_WIS_URL)
}
