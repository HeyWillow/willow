//! ESP-SR model and audio-front-end integration.

#![allow(
    dead_code,
    reason = "the imported owners remain inactive until the audio cut-over"
)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod ffi;
mod fixture;

use core::ffi::CStr;
use core::fmt;
use core::mem::{align_of, size_of};

use esp_idf_sys::esp_sr;

#[cfg(target_pointer_width = "32")]
const _: () = {
    const _: [(); 16] = [(); size_of::<esp_sr::afe_pcm_config_t>()];
    const _: [(); 8] = [(); size_of::<esp_sr::afe_debug_hook_t>()];
    const _: [(); 104] = [(); size_of::<esp_sr::afe_config_t>()];
    const _: [(); 44] = [(); size_of::<esp_sr::afe_fetch_result_t>()];
    const _: [(); 68] = [(); size_of::<esp_sr::esp_afe_sr_iface_t>()];
    const _: [(); 16] = [(); size_of::<esp_sr::srmodel_data_t>()];
    const _: [(); 24] = [(); size_of::<esp_sr::srmodel_list_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_pcm_config_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_debug_hook_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_config_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::afe_fetch_result_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::esp_afe_sr_iface_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::srmodel_data_t>()];
    const _: [(); 4] = [(); align_of::<esp_sr::srmodel_list_t>()];
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Sha256Digest([u8; 32]);

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

struct ModelFixture {
    partition_label: &'static CStr,
    model_name: &'static CStr,
    image_len: u32,
    sha256: Sha256Digest,
}

impl ModelFixture {
    const fn reviewed(
        partition_label: &'static CStr,
        model_name: &'static CStr,
        image_len: u32,
        sha256: [u8; 32],
    ) -> Self {
        Self {
            partition_label,
            model_name,
            image_len,
            sha256: Sha256Digest(sha256),
        }
    }

    const fn partition_label(&self) -> &'static CStr {
        self.partition_label
    }

    const fn model_name(&self) -> &'static CStr {
        self.model_name
    }

    const fn image_len(&self) -> u32 {
        self.image_len
    }

    const fn expected_sha256(&self) -> Sha256Digest {
        self.sha256
    }
}

static HEY_WILLOW_FIXTURE: ModelFixture = ModelFixture::reviewed(
    c"model",
    c"wn9_heywillow_tts",
    291_040,
    [
        0x66, 0x19, 0x32, 0x4d, 0xd6, 0x8d, 0x66, 0xd0, 0xee, 0xb2, 0x11, 0xda, 0x9d, 0x23, 0xa9,
        0xe2, 0x07, 0xfc, 0x77, 0x93, 0xf5, 0x7e, 0x4a, 0xed, 0x12, 0x76, 0xc2, 0x69, 0xd0, 0x41,
        0x80, 0xaf,
    ],
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputFormat {
    sample_rate: u32,
    microphone_channels: usize,
    reference_channels: usize,
}

impl InputFormat {
    const BOX3_WAKE_ONLY: Self = Self {
        sample_rate: 16_000,
        microphone_channels: 2,
        reference_channels: 0,
    };

    fn total_channels(self) -> Result<usize, SrError> {
        self.microphone_channels
            .checked_add(self.reference_channels)
            .ok_or(SrError::InvalidAfeDimension(
                "input channel count overflows",
            ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameSpec {
    sample_rate: u32,
    input_channels: usize,
    microphone_channels: usize,
    reference_channels: usize,
    feed_samples_per_channel: usize,
    fetch_samples: usize,
}

struct SpeechFrontend {
    inner: ffi::Frontend,
}

impl SpeechFrontend {
    fn open(fixture: &ModelFixture, input: InputFormat) -> Result<Self, SrError> {
        ffi::Frontend::open(fixture, input).map(|inner| Self { inner })
    }

    const fn frame_spec(&self) -> FrameSpec {
        self.inner.frame_spec()
    }

    const fn model_index(&self) -> usize {
        self.inner.model_index()
    }
}

#[derive(Debug)]
enum SrError {
    MissingPartition,
    WrongPartitionGeometry {
        address: u32,
        size: u32,
        partition_type: u32,
        subtype: u32,
        encrypted: bool,
    },
    PartitionRead {
        offset: u32,
        code: i32,
    },
    ErasedFixture,
    TruncatedFixture {
        erased_tail_bytes: usize,
    },
    FixtureHashMismatch {
        actual: Sha256Digest,
    },
    InvalidPack(&'static str),
    InsufficientMmapSpace {
        free_bytes: u64,
        required_bytes: u64,
    },
    AlreadyOpen,
    ExternalModelState,
    ModelLoadFailed,
    MissingHeyWillowModel,
    MissingAfeFunction(&'static str),
    AfeCreateFailed,
    UnsupportedInputFormat(InputFormat),
    InvalidAfeDimension(&'static str),
    UnexpectedAfeDimensions {
        sample_rate: usize,
        input_channels: usize,
        microphone_channels: usize,
    },
    InternalInvariant(&'static str),
}

impl fmt::Display for SrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPartition => write!(formatter, "model partition is missing"),
            Self::WrongPartitionGeometry {
                address,
                size,
                partition_type,
                subtype,
                encrypted,
            } => write!(
                formatter,
                "wrong model partition geometry: address={address:#x} size={size:#x} type={partition_type:#x} subtype={subtype:#x} encrypted={encrypted}"
            ),
            Self::PartitionRead { offset, code } => {
                write!(
                    formatter,
                    "partition read failed at {offset:#x}: ESP error {code:#x}"
                )
            }
            Self::ErasedFixture => write!(formatter, "model partition is erased"),
            Self::TruncatedFixture { erased_tail_bytes } => write!(
                formatter,
                "model fixture appears truncated ({erased_tail_bytes} erased trailing bytes)"
            ),
            Self::FixtureHashMismatch { actual } => {
                write!(formatter, "model fixture SHA-256 mismatch: got {actual}")
            }
            Self::InvalidPack(reason) => write!(formatter, "invalid model pack: {reason}"),
            Self::InsufficientMmapSpace {
                free_bytes,
                required_bytes,
            } => write!(
                formatter,
                "insufficient flash mmap space: {free_bytes} bytes free, {required_bytes} required"
            ),
            Self::AlreadyOpen => write!(formatter, "an ESP-SR frontend is already open"),
            Self::ExternalModelState => write!(
                formatter,
                "ESP-SR's global model singleton was initialized outside Willow"
            ),
            Self::ModelLoadFailed => {
                write!(formatter, "ESP-SR rejected the reviewed model fixture")
            }
            Self::MissingHeyWillowModel => {
                write!(formatter, "wn9_heywillow_tts is absent from the model pack")
            }
            Self::MissingAfeFunction(name) => {
                write!(formatter, "ESP-SR AFE function pointer is null: {name}")
            }
            Self::AfeCreateFailed => write!(
                formatter,
                "ESP-SR could not create the 16 kHz, two-microphone, zero-reference AFE"
            ),
            Self::UnsupportedInputFormat(input) => write!(
                formatter,
                "unsupported first-slice input: {} Hz, {} microphones, {} references",
                input.sample_rate, input.microphone_channels, input.reference_channels
            ),
            Self::InvalidAfeDimension(name) => {
                write!(formatter, "invalid ESP-SR runtime dimension: {name}")
            }
            Self::UnexpectedAfeDimensions {
                sample_rate,
                input_channels,
                microphone_channels,
            } => write!(
                formatter,
                "ESP-SR returned unexpected dimensions: rate={sample_rate}, inputs={input_channels}, microphones={microphone_channels}"
            ),
            Self::InternalInvariant(reason) => {
                write!(formatter, "internal wrapper invariant failed: {reason}")
            }
        }
    }
}

impl std::error::Error for SrError {}
