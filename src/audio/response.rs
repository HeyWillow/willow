//! Pure command-response audio policy.

use core::fmt;
use std::{borrow::Cow, collections::TryReserveError};

const ERROR_CHIME_URI: &str = "spiffs://spiffs/user/audio/error.wav";
const LEGACY_TTS_ARGUMENTS: &str = "?format=WAV&speaker=CLB&text=";
const SUCCESS_CHIME_URI: &str = "spiffs://spiffs/user/audio/success.wav";

/// Audible response behavior selected by the active configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResponseMode {
    Chimes,
    None,
    Tts,
}

/// Whether the completed command succeeded or failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CommandOutcome {
    Error,
    Success,
}

/// Playback selected for one command result.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum ResponseAudio<'source> {
    None,
    Play(Cow<'source, str>),
}

#[derive(Debug)]
pub(super) enum ResponseError {
    MissingTtsText,
    TtsUrlTooLong {
        base_bytes: usize,
        argument_bytes: usize,
        text_bytes: usize,
    },
    AllocateTtsUrl {
        bytes: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for ResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTtsText => {
                formatter.write_str("cannot construct a TTS response without text")
            }
            Self::TtsUrlTooLong {
                base_bytes,
                argument_bytes,
                text_bytes,
            } => write!(
                formatter,
                "TTS URL length overflow: base={base_bytes}, arguments={argument_bytes}, text={text_bytes} bytes"
            ),
            Self::AllocateTtsUrl { bytes, source } => {
                write!(
                    formatter,
                    "failed to allocate {bytes}-byte TTS URL: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ResponseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateTtsUrl { source, .. } => Some(source),
            Self::MissingTtsText | Self::TtsUrlTooLong { .. } => None,
        }
    }
}

/// Borrows the configured TTS endpoints and resolves response playback.
pub(super) struct ResponsePolicy<'config> {
    mode: ResponseMode,
    legacy_tts_url: &'config str,
    tts_url_v2: Option<&'config str>,
}

impl<'config> ResponsePolicy<'config> {
    pub(super) const fn new(
        mode: ResponseMode,
        legacy_tts_url: &'config str,
        tts_url_v2: Option<&'config str>,
    ) -> Self {
        Self {
            mode,
            legacy_tts_url,
            tts_url_v2,
        }
    }

    pub(super) fn select<'text>(
        &self,
        outcome: CommandOutcome,
        text: Option<&'text str>,
    ) -> Result<ResponseAudio<'text>, ResponseError> {
        match self.mode {
            ResponseMode::None => Ok(ResponseAudio::None),
            ResponseMode::Chimes => Ok(ResponseAudio::Play(Cow::Borrowed(chime_uri(outcome)))),
            ResponseMode::Tts => {
                let text = text.ok_or(ResponseError::MissingTtsText)?;
                let (base, arguments) = self
                    .tts_url_v2
                    .map_or((self.legacy_tts_url, LEGACY_TTS_ARGUMENTS), |url| (url, ""));
                build_tts_url(base, arguments, text).map(|url| ResponseAudio::Play(Cow::Owned(url)))
            }
        }
    }
}

/// Selects the fixed SPIFFS chime used for direct and configured feedback.
pub(super) const fn chime_uri(outcome: CommandOutcome) -> &'static str {
    match outcome {
        CommandOutcome::Error => ERROR_CHIME_URI,
        CommandOutcome::Success => SUCCESS_CHIME_URI,
    }
}

fn build_tts_url(base: &str, arguments: &str, text: &str) -> Result<String, ResponseError> {
    let encoded_text_bytes = text.bytes().try_fold(0_usize, |length, byte| {
        length.checked_add(if is_query_unreserved(byte) { 1 } else { 3 })
    });
    let bytes = base
        .len()
        .checked_add(arguments.len())
        .and_then(|length| encoded_text_bytes.and_then(|text| length.checked_add(text)))
        .ok_or(ResponseError::TtsUrlTooLong {
            base_bytes: base.len(),
            argument_bytes: arguments.len(),
            text_bytes: text.len(),
        })?;
    let mut url = String::new();
    url.try_reserve_exact(bytes)
        .map_err(|source| ResponseError::AllocateTtsUrl { bytes, source })?;
    url.push_str(base);
    url.push_str(arguments);
    push_encoded_query_value(&mut url, text);
    Ok(url)
}

const fn is_query_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn push_encoded_query_value(destination: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for byte in value.bytes() {
        if is_query_unreserved(byte) {
            destination.push(char::from(byte));
        } else {
            destination.push('%');
            destination.push(char::from(HEX[usize::from(byte >> 4)]));
            destination.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    const LEGACY_URL: &str = "https://legacy.example/tts";

    #[test]
    fn disabled_responses_remain_silent_without_text() {
        let policy = super::ResponsePolicy::new(super::ResponseMode::None, LEGACY_URL, None);

        assert_eq!(
            policy.select(super::CommandOutcome::Error, None).ok(),
            Some(super::ResponseAudio::None)
        );
    }

    #[test]
    fn chimes_ignore_text_and_distinguish_the_outcome() {
        let policy = super::ResponsePolicy::new(super::ResponseMode::Chimes, LEGACY_URL, None);

        assert_eq!(
            policy.select(super::CommandOutcome::Error, None).ok(),
            Some(super::ResponseAudio::Play(std::borrow::Cow::Borrowed(
                super::ERROR_CHIME_URI,
            )))
        );
        assert_eq!(
            policy.select(super::CommandOutcome::Success, None).ok(),
            Some(super::ResponseAudio::Play(std::borrow::Cow::Borrowed(
                super::SUCCESS_CHIME_URI,
            )))
        );
    }

    #[test]
    fn legacy_tts_retains_the_existing_arguments_and_encodes_text() {
        let policy = super::ResponsePolicy::new(super::ResponseMode::Tts, LEGACY_URL, None);

        assert_eq!(
            policy
                .select(super::CommandOutcome::Success, Some("lights & fan"))
                .ok(),
            Some(super::ResponseAudio::Play(std::borrow::Cow::Owned(
                format!(
                    "{LEGACY_URL}{}lights%20%26%20fan",
                    super::LEGACY_TTS_ARGUMENTS
                ),
            )))
        );
    }

    #[test]
    fn v2_tts_endpoint_takes_precedence_and_encodes_text() {
        let policy = super::ResponsePolicy::new(
            super::ResponseMode::Tts,
            LEGACY_URL,
            Some("https://v2.example/tts?text="),
        );

        assert_eq!(
            policy
                .select(super::CommandOutcome::Error, Some("not found"))
                .ok(),
            Some(super::ResponseAudio::Play(std::borrow::Cow::Owned(
                "https://v2.example/tts?text=not%20found".to_owned(),
            )))
        );
    }

    #[test]
    fn tts_encodes_utf8_and_reserved_query_bytes() {
        let policy = super::ResponsePolicy::new(
            super::ResponseMode::Tts,
            LEGACY_URL,
            Some("https://v2.example/tts?text="),
        );

        assert_eq!(
            policy
                .select(super::CommandOutcome::Success, Some("café?"))
                .ok(),
            Some(super::ResponseAudio::Play(std::borrow::Cow::Owned(
                "https://v2.example/tts?text=caf%C3%A9%3F".to_owned(),
            )))
        );
    }

    #[test]
    fn tts_rejects_a_missing_message() {
        let policy = super::ResponsePolicy::new(super::ResponseMode::Tts, LEGACY_URL, None);

        assert!(matches!(
            policy.select(super::CommandOutcome::Success, None),
            Err(super::ResponseError::MissingTtsText)
        ));
    }
}
