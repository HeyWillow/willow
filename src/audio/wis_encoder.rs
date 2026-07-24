//! Bounded per-session WIS audio encoder.

use core::fmt;
use std::{collections::TryReserveError, io};

use super::{
    stream_codec::{AmrWbEncoder, StreamCodecError},
    wis_framing::{
        PcmFrameBuffer, PcmFrameBufferError, UncompressedWriteError, WisFormat, write_uncompressed,
    },
};

const AMR_WB_FILE_HEADER_BYTES: usize = b"#!AMR-WB\n".len();

#[derive(Debug)]
pub(super) enum WisEncodingError {
    Uncompressed {
        source: UncompressedWriteError,
    },
    PcmFrame {
        source: PcmFrameBufferError,
    },
    Codec {
        source: StreamCodecError,
    },
    AmrOutputSizeOverflow {
        frame_bytes: usize,
        header_bytes: usize,
    },
    AllocateAmrOutput {
        bytes: usize,
        source: TryReserveError,
    },
    Write {
        format: WisFormat,
        bytes: usize,
        source: io::Error,
    },
}

impl fmt::Display for WisEncodingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uncompressed { source } => {
                write!(formatter, "uncompressed WIS audio failed: {source}")
            }
            Self::PcmFrame { source } => write!(formatter, "AMR-WB framing failed: {source}"),
            Self::Codec { source } => write!(formatter, "AMR-WB encoding failed: {source}"),
            Self::AmrOutputSizeOverflow {
                frame_bytes,
                header_bytes,
            } => write!(
                formatter,
                "AMR-WB output size overflow: frame={frame_bytes}, file header={header_bytes} bytes"
            ),
            Self::AllocateAmrOutput { bytes, source } => write!(
                formatter,
                "failed to allocate {bytes}-byte AMR-WB output frame: {source}"
            ),
            Self::Write {
                format,
                bytes,
                source,
            } => write!(
                formatter,
                "failed to write {bytes} bytes of {format:?} audio: {source}"
            ),
        }
    }
}

impl std::error::Error for WisEncodingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Uncompressed { source } => Some(source),
            Self::PcmFrame { source } => Some(source),
            Self::Codec { source } => Some(source),
            Self::AllocateAmrOutput { source, .. } => Some(source),
            Self::Write { source, .. } => Some(source),
            Self::AmrOutputSizeOverflow { .. } => None,
        }
    }
}

/// Result of closing one encoding session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EncodingFinish {
    /// PCM samples which could not fill a complete compressed frame.
    pub(super) dropped_samples: usize,
}

/// Encodes exactly one WIS request using its configured wire format.
pub(super) struct WisEncoder {
    format: WisFormat,
    inner: EncoderInner,
}

impl WisEncoder {
    pub(super) fn new(format: WisFormat) -> Result<Self, WisEncodingError> {
        let inner = match format {
            WisFormat::AmrWb => EncoderInner::AmrWb(AmrWbStreamEncoder::new()?),
            WisFormat::Pcm | WisFormat::Wav => EncoderInner::Uncompressed,
        };
        Ok(Self { format, inner })
    }

    pub(super) fn write_samples(
        &mut self,
        samples: &[i16],
        destination: &mut impl io::Write,
    ) -> Result<(), WisEncodingError> {
        match &mut self.inner {
            EncoderInner::Uncompressed => write_uncompressed(self.format, samples, destination)
                .map_err(|source| WisEncodingError::Uncompressed { source }),
            EncoderInner::AmrWb(encoder) => encoder.write_samples(samples, destination),
        }
    }

    pub(super) fn finish(&mut self) -> EncodingFinish {
        let dropped_samples = match &mut self.inner {
            EncoderInner::Uncompressed => 0,
            EncoderInner::AmrWb(encoder) => encoder.input.discard_pending(),
        };
        EncodingFinish { dropped_samples }
    }
}

enum EncoderInner {
    Uncompressed,
    AmrWb(AmrWbStreamEncoder),
}

struct AmrWbStreamEncoder {
    encoder: AmrWbEncoder,
    input: PcmFrameBuffer,
    output: Vec<u8>,
}

impl AmrWbStreamEncoder {
    fn new() -> Result<Self, WisEncodingError> {
        let encoder = AmrWbEncoder::new().map_err(|source| WisEncodingError::Codec { source })?;
        let frame_sizes = encoder.frame_sizes();
        let input = PcmFrameBuffer::new(frame_sizes.input_bytes)
            .map_err(|source| WisEncodingError::PcmFrame { source })?;
        let output_bytes = frame_sizes
            .output_bytes
            .checked_add(AMR_WB_FILE_HEADER_BYTES)
            .ok_or(WisEncodingError::AmrOutputSizeOverflow {
                frame_bytes: frame_sizes.output_bytes,
                header_bytes: AMR_WB_FILE_HEADER_BYTES,
            })?;
        let output = allocate_output(output_bytes)?;

        Ok(Self {
            encoder,
            input,
            output,
        })
    }

    fn write_samples(
        &mut self,
        samples: &[i16],
        destination: &mut impl io::Write,
    ) -> Result<(), WisEncodingError> {
        let Self {
            encoder,
            input,
            output,
        } = self;
        input.push(samples, |pcm| {
            let progress = encoder
                .encode_frame(pcm, output)
                .map_err(|source| WisEncodingError::Codec { source })?;
            destination
                .write_all(&output[..progress.output_bytes])
                .map_err(|source| WisEncodingError::Write {
                    format: WisFormat::AmrWb,
                    bytes: progress.output_bytes,
                    source,
                })
        })?;
        Ok(())
    }
}

fn allocate_output(bytes: usize) -> Result<Vec<u8>, WisEncodingError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes)
        .map_err(|source| WisEncodingError::AllocateAmrOutput { bytes, source })?;
    output.resize(bytes, 0);
    Ok(output)
}
