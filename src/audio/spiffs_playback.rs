//! SPIFFS audio URI validation and streaming playback.

#![allow(
    dead_code,
    reason = "SPIFFS playback remains inactive until Rust owns runtime audio"
)]

use core::fmt;
use std::{fs::File, io, path::PathBuf};

use super::{
    i2s::TransmitChannel,
    playback::{self, PlaybackError, PlaybackWorkspace},
    spiffs_uri::{self, AudioFileFormat, SpiffsUriError},
    stream_codec::{CodecLibrary, StreamFormat},
};
use crate::spiffs;

#[derive(Debug)]
pub(super) enum SpiffsPlaybackError {
    Uri {
        source: SpiffsUriError,
    },
    Open {
        uri: String,
        path: PathBuf,
        source: io::Error,
    },
    Playback {
        uri: String,
        source: PlaybackError,
    },
}

impl fmt::Display for SpiffsPlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uri { source } => write!(formatter, "invalid SPIFFS playback source: {source}"),
            Self::Open { uri, path, source } => write!(
                formatter,
                "failed to open SPIFFS audio URI {uri:?} at {}: {source}",
                path.display()
            ),
            Self::Playback { uri, source } => {
                write!(
                    formatter,
                    "failed to play SPIFFS audio URI {uri:?}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for SpiffsPlaybackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Uri { source } => Some(source),
            Self::Open { source, .. } => Some(source),
            Self::Playback { source, .. } => Some(source),
        }
    }
}

impl SpiffsPlaybackError {
    pub(super) const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Playback { source, .. } if source.is_cancelled())
    }
}

pub(super) fn play(
    uri: &str,
    codecs: &CodecLibrary,
    transmit: &mut TransmitChannel,
    workspace: &mut PlaybackWorkspace<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), SpiffsPlaybackError> {
    let path = spiffs_uri::resolve(uri, spiffs::MOUNT_PATH.as_ref())
        .map_err(|source| SpiffsPlaybackError::Uri { source })?;
    let format = spiffs_uri::format(uri, &path)
        .map(StreamFormat::from)
        .map_err(|source| SpiffsPlaybackError::Uri { source })?;
    let mut file = File::open(&path).map_err(|source| SpiffsPlaybackError::Open {
        uri: uri.to_owned(),
        path: path.clone(),
        source,
    })?;

    playback::play_reader(&mut file, format, codecs, transmit, workspace, cancelled).map_err(
        |source| SpiffsPlaybackError::Playback {
            uri: uri.to_owned(),
            source,
        },
    )
}

impl From<AudioFileFormat> for StreamFormat {
    fn from(format: AudioFileFormat) -> Self {
        match format {
            AudioFileFormat::Aac => Self::Aac,
            AudioFileFormat::AmrNb => Self::AmrNb,
            AudioFileFormat::AmrWb => Self::AmrWb,
            AudioFileFormat::Flac => Self::Flac,
            AudioFileFormat::M4a => Self::M4a,
            AudioFileFormat::Mp3 => Self::Mp3,
            AudioFileFormat::Ogg => Self::Ogg,
            AudioFileFormat::Opus => Self::Opus,
            AudioFileFormat::Pcm => Self::Pcm,
            AudioFileFormat::TransportStream => Self::TransportStream,
            AudioFileFormat::Wav => Self::Wav,
        }
    }
}
