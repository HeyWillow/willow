//! Typed mapping from Willow configuration to ESP-SR AFE policy.

use willow_schema::config::v1::{Config, VadMode, WakeMode};

use super::{InputFormat, SrError, WakeModel};

const DEFAULT_WAKE_WORD: &str = "hiesp";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AfeConfiguration {
    pub(super) model: WakeModel,
    pub(super) input: InputFormat,
    pub(super) acoustic_echo_cancellation: bool,
    pub(super) blind_source_separation: bool,
    pub(super) vad_mode: VadMode,
    pub(super) wake_mode: WakeMode,
}

impl AfeConfiguration {
    pub(super) fn from_active_config(input: InputFormat) -> Result<Self, SrError> {
        Self::from_config(crate::config::config(), input)
    }

    fn from_config(config: Option<&Config>, input: InputFormat) -> Result<Self, SrError> {
        let wake_word = config
            .and_then(|config| config.wake_word.as_deref())
            .unwrap_or(DEFAULT_WAKE_WORD);
        let model = match wake_word {
            "alexa" => WakeModel::Alexa,
            "hiesp" => WakeModel::HiEsp,
            "hilexin" => WakeModel::HiLexin,
            wake_word => return Err(SrError::UnsupportedWakeWord(wake_word.to_owned())),
        };

        Ok(Self {
            model,
            input,
            acoustic_echo_cancellation: input.reference_channels > 0
                && config.and_then(|config| config.aec).unwrap_or(true),
            blind_source_separation: config.and_then(|config| config.bss).unwrap_or(true),
            vad_mode: config
                .and_then(|config| config.vad_mode)
                .unwrap_or(VadMode::Mode3),
            wake_mode: config
                .and_then(|config| config.wake_mode)
                .unwrap_or(WakeMode::TwoChannel90),
        })
    }
}
