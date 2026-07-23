//! Allocation-free PCM channel conversion, resampling, and I2S packing.

#![allow(
    dead_code,
    reason = "the PCM converter remains inactive until the Rust player is connected"
)]

use core::fmt;

const MAXIMUM_SAMPLE_RATE_HZ: u32 = 192_000;
const MINIMUM_SAMPLE_RATE_HZ: u32 = 8_000;
const OUTPUT_CHANNELS: usize = 2;
pub(crate) const OUTPUT_SAMPLE_RATE_HZ: u32 = 16_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChannelCount {
    Mono,
    Stereo,
}

impl ChannelCount {
    const fn new(channels: usize) -> Result<Self, PcmError> {
        match channels {
            1 => Ok(Self::Mono),
            2 => Ok(Self::Stereo),
            _ => Err(PcmError::UnsupportedChannelCount { channels }),
        }
    }

    const fn get(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StereoFrame {
    left: i16,
    right: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConversionProgress {
    pub(crate) input_frames: usize,
    pub(crate) output_frames: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PcmError {
    UnsupportedSampleRate { sample_rate_hz: u32 },
    UnsupportedChannelCount { channels: usize },
    MisalignedInput { samples: usize, channels: usize },
    MisalignedOutput { samples: usize, channels: usize },
}

impl fmt::Display for PcmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSampleRate { sample_rate_hz } => write!(
                formatter,
                "PCM input rate {sample_rate_hz} Hz is outside the supported {MINIMUM_SAMPLE_RATE_HZ}..={MAXIMUM_SAMPLE_RATE_HZ} Hz range"
            ),
            Self::UnsupportedChannelCount { channels } => write!(
                formatter,
                "PCM input has {channels} channels; only mono and stereo are supported"
            ),
            Self::MisalignedInput { samples, channels } => write!(
                formatter,
                "PCM input has {samples} samples, which is not a whole number of {channels}-channel frames"
            ),
            Self::MisalignedOutput { samples, channels } => write!(
                formatter,
                "PCM output has {samples} samples, which is not a whole number of {channels}-channel frames"
            ),
        }
    }
}

impl core::error::Error for PcmError {}

/// Converts interleaved signed 16-bit PCM into Willow's fixed I2S format.
///
/// The converter is streaming and allocation-free. It consumes only as many
/// input frames as can be represented by the caller's output slice, retaining
/// at most one stereo frame when an interpolation interval spans calls.
pub(crate) struct PcmConverter {
    input_rate_hz: u32,
    input_channels: ChannelCount,
    previous: Option<StereoFrame>,
    current: Option<StereoFrame>,
    next_output_offset: u32,
    emit_initial: bool,
}

impl PcmConverter {
    /// Creates a converter targeting 16 kHz, stereo, 32-bit I2S slots.
    pub(crate) fn new(input_rate_hz: u32, input_channels: usize) -> Result<Self, PcmError> {
        if !(MINIMUM_SAMPLE_RATE_HZ..=MAXIMUM_SAMPLE_RATE_HZ).contains(&input_rate_hz) {
            return Err(PcmError::UnsupportedSampleRate {
                sample_rate_hz: input_rate_hz,
            });
        }

        Ok(Self {
            input_rate_hz,
            input_channels: ChannelCount::new(input_channels)?,
            previous: None,
            current: None,
            next_output_offset: input_rate_hz,
            emit_initial: false,
        })
    }

    /// Clears interpolation history before beginning a different PCM stream.
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.current = None;
        self.next_output_offset = self.input_rate_hz;
        self.emit_initial = false;
    }

    /// Converts as much input as the supplied output slice can hold.
    pub(crate) fn process(
        &mut self,
        input: &[i16],
        output: &mut [i32],
    ) -> Result<ConversionProgress, PcmError> {
        let input_channels = self.input_channels.get();
        if input.len() % input_channels != 0 {
            return Err(PcmError::MisalignedInput {
                samples: input.len(),
                channels: input_channels,
            });
        }
        if output.len() % OUTPUT_CHANNELS != 0 {
            return Err(PcmError::MisalignedOutput {
                samples: output.len(),
                channels: OUTPUT_CHANNELS,
            });
        }

        let input_frames = input.len() / input_channels;
        let output_capacity = output.len() / OUTPUT_CHANNELS;
        let mut progress = ConversionProgress {
            input_frames: 0,
            output_frames: 0,
        };

        if self.previous.is_none() && input_frames > 0 {
            self.previous = Some(self.input_frame(input, 0));
            self.emit_initial = true;
            progress.input_frames = 1;
        }

        if self.emit_initial {
            if progress.output_frames == output_capacity {
                return Ok(progress);
            }
            if let Some(frame) = self.previous {
                write_i2s_frame(output, progress.output_frames, frame);
                progress.output_frames += 1;
                self.emit_initial = false;
            }
        }

        loop {
            if self.current.is_none() {
                if progress.input_frames == input_frames {
                    return Ok(progress);
                }
                self.current = Some(self.input_frame(input, progress.input_frames));
                progress.input_frames += 1;
            }

            while self.next_output_offset <= OUTPUT_SAMPLE_RATE_HZ {
                if progress.output_frames == output_capacity {
                    return Ok(progress);
                }
                if let (Some(previous), Some(current)) = (self.previous, self.current) {
                    let frame = interpolate_frame(
                        previous,
                        current,
                        self.next_output_offset,
                        OUTPUT_SAMPLE_RATE_HZ,
                    );
                    write_i2s_frame(output, progress.output_frames, frame);
                    progress.output_frames += 1;
                    self.next_output_offset += self.input_rate_hz;
                }
            }

            self.next_output_offset -= OUTPUT_SAMPLE_RATE_HZ;
            self.previous = self.current.take();
        }
    }

    fn input_frame(&self, input: &[i16], frame: usize) -> StereoFrame {
        match self.input_channels {
            ChannelCount::Mono => {
                let sample = input[frame];
                StereoFrame {
                    left: sample,
                    right: sample,
                }
            }
            ChannelCount::Stereo => {
                let offset = frame * 2;
                StereoFrame {
                    left: input[offset],
                    right: input[offset + 1],
                }
            }
        }
    }
}

fn interpolate_frame(
    previous: StereoFrame,
    current: StereoFrame,
    numerator: u32,
    denominator: u32,
) -> StereoFrame {
    StereoFrame {
        left: interpolate_sample(previous.left, current.left, numerator, denominator),
        right: interpolate_sample(previous.right, current.right, numerator, denominator),
    }
}

fn interpolate_sample(previous: i16, current: i16, numerator: u32, denominator: u32) -> i16 {
    let delta = i64::from(current) - i64::from(previous);
    let weighted = delta * i64::from(numerator);
    let denominator = i64::from(denominator);
    let rounded = if weighted >= 0 {
        (weighted + denominator / 2) / denominator
    } else {
        (weighted - denominator / 2) / denominator
    };
    let sample = i64::from(previous) + rounded;
    match i16::try_from(sample) {
        Ok(sample) => sample,
        Err(_) if sample < 0 => i16::MIN,
        Err(_) => i16::MAX,
    }
}

fn write_i2s_frame(output: &mut [i32], frame: usize, samples: StereoFrame) {
    let offset = frame * OUTPUT_CHANNELS;
    output[offset] = i32::from(samples.left) << 16;
    output[offset + 1] = i32::from(samples.right) << 16;
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_invalid_layouts() {
        assert_eq!(
            super::PcmConverter::new(7_999, 1).err(),
            Some(super::PcmError::UnsupportedSampleRate {
                sample_rate_hz: 7_999
            })
        );
        assert_eq!(
            super::PcmConverter::new(16_000, 3).err(),
            Some(super::PcmError::UnsupportedChannelCount { channels: 3 })
        );

        let mut converter = super::PcmConverter::new(16_000, 2).unwrap();
        assert_eq!(
            converter.process(&[1], &mut [0; 2]),
            Err(super::PcmError::MisalignedInput {
                samples: 1,
                channels: 2
            })
        );
        assert_eq!(
            converter.process(&[1, 2], &mut [0; 1]),
            Err(super::PcmError::MisalignedOutput {
                samples: 1,
                channels: 2
            })
        );
    }

    #[test]
    fn duplicates_and_left_aligns_mono_samples() {
        let mut converter = super::PcmConverter::new(16_000, 1).unwrap();
        let mut output = [0; 8];
        let progress = converter
            .process(&[i16::MIN, -1, 0, i16::MAX], &mut output)
            .unwrap();

        assert_eq!(
            progress,
            super::ConversionProgress {
                input_frames: 4,
                output_frames: 4
            }
        );
        assert_eq!(
            output,
            [
                i32::MIN,
                i32::MIN,
                -65_536,
                -65_536,
                0,
                0,
                2_147_418_112,
                2_147_418_112,
            ]
        );
    }

    #[test]
    fn preserves_stereo_at_the_target_rate() {
        let mut converter = super::PcmConverter::new(16_000, 2).unwrap();
        let mut output = [0; 6];
        let progress = converter
            .process(&[1, -1, 2, -2, 3, -3], &mut output)
            .unwrap();

        assert_eq!(progress.output_frames, 3);
        assert_eq!(
            output,
            [65_536, -65_536, 131_072, -131_072, 196_608, -196_608]
        );
    }

    #[test]
    fn linearly_upsamples_across_bounded_calls() {
        let mut converter = super::PcmConverter::new(8_000, 1).unwrap();
        let mut first = [0; 2];
        let progress = converter.process(&[0, 1_000], &mut first).unwrap();
        assert_eq!(progress.input_frames, 2);
        assert_eq!(progress.output_frames, 1);
        assert_eq!(first, [0, 0]);

        let mut middle = [0; 2];
        let progress = converter.process(&[], &mut middle).unwrap();
        assert_eq!(progress.input_frames, 0);
        assert_eq!(progress.output_frames, 1);
        assert_eq!(middle, [500 << 16, 500 << 16]);

        let mut last = [0; 2];
        let progress = converter.process(&[], &mut last).unwrap();
        assert_eq!(progress.output_frames, 1);
        assert_eq!(last, [1_000 << 16, 1_000 << 16]);
    }

    #[test]
    fn downsamples_on_the_global_frame_grid() {
        let mut converter = super::PcmConverter::new(48_000, 1).unwrap();
        let mut output = [0; 6];
        let progress = converter
            .process(&[0, 1, 2, 3, 4, 5, 6], &mut output)
            .unwrap();

        assert_eq!(progress.input_frames, 7);
        assert_eq!(progress.output_frames, 3);
        assert_eq!(output, [0, 0, 3 << 16, 3 << 16, 6 << 16, 6 << 16]);
    }

    #[test]
    fn reset_starts_the_next_stream_at_its_first_sample() {
        let mut converter = super::PcmConverter::new(16_000, 1).unwrap();
        let mut output = [0; 4];
        converter.process(&[10, 20], &mut output).unwrap();
        converter.reset();
        converter.process(&[-10, -20], &mut output).unwrap();

        assert_eq!(output, [-10 << 16, -10 << 16, -20 << 16, -20 << 16]);
    }
}
