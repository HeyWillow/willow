//! Hardware-validated microphone framing for the Rust audio engine.

use core::fmt;

pub(super) const SAMPLE_RATE_HZ: u32 = super::pcm::OUTPUT_SAMPLE_RATE_HZ;
pub(super) const PHYSICAL_SLOTS: usize = 2;
pub(super) const SLOT_WIDTH_BITS: u32 = 32;
pub(super) const RAW_FRAME_BYTES: usize = PHYSICAL_SLOTS * core::mem::size_of::<i32>();
pub(super) const MICROPHONE_CHANNELS: usize = 2;
pub(super) const MAX_REFERENCE_CHANNELS: usize = 1;
const MICROPHONE_HALFWORDS: [usize; MICROPHONE_CHANNELS] = [1, 3];
const MICROPHONE_AND_REFERENCE_HALFWORDS: [usize; MICROPHONE_CHANNELS + MAX_REFERENCE_CHANNELS] =
    [MICROPHONE_HALFWORDS[0], MICROPHONE_HALFWORDS[1], 0];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureFramingError {
    MisalignedInput {
        bytes: usize,
        frame_bytes: usize,
    },
    UnexpectedAfeOutputLength {
        expected_samples: usize,
        actual_samples: usize,
    },
    UnsupportedReferenceChannels {
        channels: usize,
    },
}

impl fmt::Display for CaptureFramingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MisalignedInput { bytes, frame_bytes } => write!(
                formatter,
                "I2S capture returned {bytes} bytes, which is not a whole number of {frame_bytes}-byte stereo frames"
            ),
            Self::UnexpectedAfeOutputLength {
                expected_samples,
                actual_samples,
            } => write!(
                formatter,
                "AFE output contains {actual_samples} samples; {expected_samples} are required for the captured frames"
            ),
            Self::UnsupportedReferenceChannels { channels } => {
                write!(
                    formatter,
                    "AFE input requests {channels} reference channels"
                )
            }
        }
    }
}

/// Extracts the microphone samples and optional deployed reference lane.
///
/// With a reference the output remains `[mic0, mic1, slot0_low]` for parity
/// with the existing Box configurations. Without one it is `[mic0, mic1]`, so
/// CoreS3 does not present ADC low-bit residue to ESP-SR as an echo reference.
pub(super) fn extract_afe_inputs(
    raw: &[u8],
    output: &mut [i16],
    reference_channels: usize,
) -> Result<usize, CaptureFramingError> {
    if raw.len() % RAW_FRAME_BYTES != 0 {
        return Err(CaptureFramingError::MisalignedInput {
            bytes: raw.len(),
            frame_bytes: RAW_FRAME_BYTES,
        });
    }

    let halfwords: &[usize] = match reference_channels {
        0 => &MICROPHONE_HALFWORDS,
        1 => &MICROPHONE_AND_REFERENCE_HALFWORDS,
        channels => return Err(CaptureFramingError::UnsupportedReferenceChannels { channels }),
    };
    let channels = halfwords.len();
    let frames = raw.len() / RAW_FRAME_BYTES;
    let expected_samples = frames * channels;
    if output.len() != expected_samples {
        return Err(CaptureFramingError::UnexpectedAfeOutputLength {
            expected_samples,
            actual_samples: output.len(),
        });
    }

    for (raw_frame, afe_frame) in raw
        .chunks_exact(RAW_FRAME_BYTES)
        .zip(output.chunks_exact_mut(channels))
    {
        for (sample, halfword) in afe_frame.iter_mut().zip(halfwords.iter().copied()) {
            let offset = halfword * core::mem::size_of::<i16>();
            *sample = i16::from_le_bytes([raw_frame[offset], raw_frame[offset + 1]]);
        }
    }

    Ok(frames)
}

impl std::error::Error for CaptureFramingError {}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    #[test]
    fn reference_input_order_matches_the_deployed_s3_mapping() {
        let raw = [
            0x22, 0x11, 0x44, 0x33, 0x66, 0x55, 0x88, 0x77, 0xaa, 0x99, 0xcc, 0xbb, 0xee, 0xdd,
            0x00, 0xff,
        ];
        let mut output = [0_i16; 6];

        let frames = super::extract_afe_inputs(&raw, &mut output, 1)
            .expect("two complete raw frames should convert");

        assert_eq!(frames, 2);
        assert_eq!(output, [0x3344, 0x7788, 0x1122, -17_460, -256, -26_198]);
    }

    #[test]
    fn microphone_only_input_omits_the_reference_lane() {
        let raw = [
            0x22, 0x11, 0x44, 0x33, 0x66, 0x55, 0x88, 0x77, 0xaa, 0x99, 0xcc, 0xbb, 0xee, 0xdd,
            0x00, 0xff,
        ];
        let mut output = [0_i16; 4];

        let frames = super::extract_afe_inputs(&raw, &mut output, 0)
            .expect("two complete raw frames should convert");

        assert_eq!(frames, 2);
        assert_eq!(output, [0x3344, 0x7788, -17_460, -256]);
    }
}
