//! Pure WIS audio-format metadata and PCM frame buffering.

use core::{fmt, mem::size_of};
use std::{collections::TryReserveError, io};

const PCM_WRITE_SAMPLES: usize = 512;

/// Audio encodings accepted by the Willow Inference Server endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WisFormat {
    AmrWb,
    Pcm,
    Wav,
}

impl WisFormat {
    pub(super) const fn header_value(self) -> &'static str {
        match self {
            Self::AmrWb => "amrwb",
            Self::Pcm => "pcm",
            Self::Wav => "wav",
        }
    }

    pub(super) const fn is_uncompressed(self) -> bool {
        matches!(self, Self::Pcm | Self::Wav)
    }
}

#[derive(Debug)]
pub(super) enum UncompressedWriteError {
    CompressedFormat { format: WisFormat },
    Write { source: io::Error },
}

impl fmt::Display for UncompressedWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CompressedFormat { format } => {
                write!(formatter, "cannot write compressed {format:?} as PCM")
            }
            Self::Write { source } => write!(formatter, "failed to write PCM audio: {source}"),
        }
    }
}

impl std::error::Error for UncompressedWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write { source } => Some(source),
            Self::CompressedFormat { .. } => None,
        }
    }
}

/// Writes PCM and legacy streamed-WAV samples as signed little-endian PCM.
///
/// Willow's streamed-WAV mode sends the same PCM bytes as `Pcm`; the WIS
/// request header distinguishes the two formats.
pub(super) fn write_uncompressed(
    format: WisFormat,
    samples: &[i16],
    destination: &mut impl io::Write,
) -> Result<(), UncompressedWriteError> {
    if !format.is_uncompressed() {
        return Err(UncompressedWriteError::CompressedFormat { format });
    }

    let mut bytes = [0_u8; PCM_WRITE_SAMPLES * size_of::<i16>()];
    for chunk in samples.chunks(PCM_WRITE_SAMPLES) {
        for (output, sample) in bytes.chunks_exact_mut(2).zip(chunk) {
            output.copy_from_slice(&sample.to_le_bytes());
        }
        destination
            .write_all(&bytes[..core::mem::size_of_val(chunk)])
            .map_err(|source| UncompressedWriteError::Write { source })?;
    }
    Ok(())
}

#[derive(Debug)]
pub(super) enum PcmFrameBufferError {
    InvalidFrameSize {
        bytes: usize,
    },
    Allocate {
        bytes: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for PcmFrameBufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFrameSize { bytes } => write!(
                formatter,
                "PCM encoder frame is {bytes} bytes; a positive, even size is required"
            ),
            Self::Allocate { bytes, source } => {
                write!(
                    formatter,
                    "failed to allocate {bytes}-byte PCM frame: {source}"
                )
            }
        }
    }
}

impl std::error::Error for PcmFrameBufferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocate { source, .. } => Some(source),
            Self::InvalidFrameSize { .. } => None,
        }
    }
}

/// Accumulates arbitrary PCM chunks into fixed native-encoder frames.
pub(super) struct PcmFrameBuffer {
    bytes: Vec<u8>,
    filled: usize,
}

impl PcmFrameBuffer {
    pub(super) fn new(frame_bytes: usize) -> Result<Self, PcmFrameBufferError> {
        if frame_bytes == 0 || frame_bytes % size_of::<i16>() != 0 {
            return Err(PcmFrameBufferError::InvalidFrameSize { bytes: frame_bytes });
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(frame_bytes)
            .map_err(|source| PcmFrameBufferError::Allocate {
                bytes: frame_bytes,
                source,
            })?;
        bytes.resize(frame_bytes, 0);
        Ok(Self { bytes, filled: 0 })
    }

    pub(super) fn push<E>(
        &mut self,
        mut samples: &[i16],
        mut encode: impl FnMut(&mut [u8]) -> Result<(), E>,
    ) -> Result<usize, E> {
        let mut frames = 0;
        while !samples.is_empty() {
            let available_samples = (self.bytes.len() - self.filled) / size_of::<i16>();
            let copied_samples = available_samples.min(samples.len());
            let (copied, remaining) = samples.split_at(copied_samples);
            for sample in copied {
                let end = self.filled + size_of::<i16>();
                self.bytes[self.filled..end].copy_from_slice(&sample.to_le_bytes());
                self.filled = end;
            }
            samples = remaining;

            if self.filled == self.bytes.len() {
                encode(&mut self.bytes)?;
                self.filled = 0;
                frames += 1;
            }
        }
        Ok(frames)
    }

    pub(super) const fn pending_samples(&self) -> usize {
        self.filled / size_of::<i16>()
    }

    pub(super) fn discard_pending(&mut self) -> usize {
        let samples = self.pending_samples();
        self.filled = 0;
        samples
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    #[test]
    fn format_header_values_remain_compatible_with_wis() {
        assert_eq!(super::WisFormat::AmrWb.header_value(), "amrwb");
        assert_eq!(super::WisFormat::Pcm.header_value(), "pcm");
        assert_eq!(super::WisFormat::Wav.header_value(), "wav");
    }

    #[test]
    fn pcm_and_streamed_wav_emit_identical_little_endian_samples() {
        let samples = [i16::MIN, -1, 0, 1, i16::MAX];
        let expected = [0x00, 0x80, 0xff, 0xff, 0x00, 0x00, 0x01, 0x00, 0xff, 0x7f];

        for format in [super::WisFormat::Pcm, super::WisFormat::Wav] {
            let mut output = Vec::new();
            assert!(super::write_uncompressed(format, &samples, &mut output).is_ok());
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn compressed_format_is_rejected_by_the_pcm_writer() {
        let mut output = Vec::new();

        assert!(matches!(
            super::write_uncompressed(super::WisFormat::AmrWb, &[0], &mut output),
            Err(super::UncompressedWriteError::CompressedFormat {
                format: super::WisFormat::AmrWb
            })
        ));
    }

    #[test]
    fn frame_buffer_preserves_samples_across_arbitrary_chunks() {
        let mut buffer = match super::PcmFrameBuffer::new(8) {
            Ok(buffer) => buffer,
            Err(error) => panic!("failed to allocate test frame: {error}"),
        };
        let mut frames = Vec::new();

        assert!(
            buffer
                .push(&[1, 2, 3], |frame| {
                    frames.push(frame.to_vec());
                    Ok::<(), ()>(())
                })
                .is_ok()
        );
        assert_eq!(buffer.pending_samples(), 3);
        assert!(
            buffer
                .push(&[4, 5, 6, 7, 8, 9], |frame| {
                    frames.push(frame.to_vec());
                    Ok::<(), ()>(())
                })
                .is_ok()
        );

        assert_eq!(
            frames,
            [vec![1, 0, 2, 0, 3, 0, 4, 0], vec![5, 0, 6, 0, 7, 0, 8, 0]]
        );
        assert_eq!(buffer.pending_samples(), 1);
        assert_eq!(buffer.discard_pending(), 1);
        assert_eq!(buffer.pending_samples(), 0);
    }

    #[test]
    fn frame_buffer_rejects_zero_and_partial_sample_sizes() {
        assert!(matches!(
            super::PcmFrameBuffer::new(0),
            Err(super::PcmFrameBufferError::InvalidFrameSize { bytes: 0 })
        ));
        assert!(matches!(
            super::PcmFrameBuffer::new(3),
            Err(super::PcmFrameBufferError::InvalidFrameSize { bytes: 3 })
        ));
    }
}
