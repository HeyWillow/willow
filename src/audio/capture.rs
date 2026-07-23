//! Hardware-validated microphone framing for the Rust audio engine.

#![allow(
    dead_code,
    reason = "the framing remains inactive until Rust takes ownership of I2S0"
)]

use core::fmt;

pub(super) const SAMPLE_RATE_HZ: usize = 16_000;
pub(super) const PHYSICAL_SLOTS: usize = 2;
pub(super) const SLOT_WIDTH_BITS: usize = 32;
pub(super) const RAW_HALFWORDS: usize = PHYSICAL_SLOTS * 2;
pub(super) const RAW_FRAME_BYTES: usize = PHYSICAL_SLOTS * SLOT_WIDTH_BITS / 8;
pub(super) const MICROPHONE_CHANNELS: usize = 2;
pub(super) const MICROPHONE_HALFWORDS: [usize; MICROPHONE_CHANNELS] = [1, 3];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureFramingError {
    MisalignedInput {
        bytes: usize,
        frame_bytes: usize,
    },
    UnexpectedOutputLength {
        expected_samples: usize,
        actual_samples: usize,
    },
}

impl fmt::Display for CaptureFramingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MisalignedInput { bytes, frame_bytes } => write!(
                formatter,
                "I2S capture returned {bytes} bytes, which is not a whole number of {frame_bytes}-byte stereo frames"
            ),
            Self::UnexpectedOutputLength {
                expected_samples,
                actual_samples,
            } => write!(
                formatter,
                "microphone output contains {actual_samples} samples; {expected_samples} are required for the captured frames"
            ),
        }
    }
}

impl std::error::Error for CaptureFramingError {}

/// Extracts the two signed microphone samples from each raw I2S frame.
pub(super) fn extract_microphones(
    raw: &[u8],
    output: &mut [i16],
) -> Result<usize, CaptureFramingError> {
    if raw.len() % RAW_FRAME_BYTES != 0 {
        return Err(CaptureFramingError::MisalignedInput {
            bytes: raw.len(),
            frame_bytes: RAW_FRAME_BYTES,
        });
    }

    let frames = raw.len() / RAW_FRAME_BYTES;
    let expected_samples = frames * MICROPHONE_CHANNELS;
    if output.len() != expected_samples {
        return Err(CaptureFramingError::UnexpectedOutputLength {
            expected_samples,
            actual_samples: output.len(),
        });
    }

    for (raw_frame, microphone_frame) in raw
        .chunks_exact(RAW_FRAME_BYTES)
        .zip(output.chunks_exact_mut(MICROPHONE_CHANNELS))
    {
        // BOX-3 validation found left-aligned 24-bit samples in little-endian
        // 32-bit stereo slots. ESP-SR consumes the signed high halfword from
        // each slot; the low halfword is padding plus the least-significant
        // ADC bits.
        for (sample, halfword) in microphone_frame.iter_mut().zip(MICROPHONE_HALFWORDS) {
            let offset = halfword * core::mem::size_of::<i16>();
            *sample = i16::from_le_bytes([raw_frame[offset], raw_frame[offset + 1]]);
        }
    }

    Ok(frames)
}
