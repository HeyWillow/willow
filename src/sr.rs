//! ESP-SR model and audio-front-end integration.

#![allow(
    dead_code,
    reason = "the imported owners remain inactive until the audio cut-over"
)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod ffi;
mod fixture;

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

impl Sha256Digest {
    const fn from_hex(hex: &[u8; 64]) -> Self {
        let mut bytes = [0_u8; 32];
        let mut index = 0;
        while index < bytes.len() {
            bytes[index] = (hex_nibble(hex[index * 2]) << 4) | hex_nibble(hex[index * 2 + 1]);
            index += 1;
        }
        Self(bytes)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeModel {
    Alexa,
    HiEsp,
    HiLexin,
}

impl WakeModel {
    const fn name(self) -> &'static str {
        match self {
            Self::Alexa => "wn9_alexa",
            Self::HiEsp => "wn9_hiesp",
            Self::HiLexin => "wn9_hilexin",
        }
    }
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FeedStatus {
    runtime_return: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VadState {
    Silence,
    Speech,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WakeDetection {
    wake_word_index_one_based: usize,
    wakenet_model_index_one_based: usize,
    wake_word_samples: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeState {
    None,
    Detected(WakeDetection),
    ChannelVerified { trigger_output_channel_id: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FetchedFrame<'a> {
    samples: &'a [i16],
    data_volume_db: f32,
    vad_state: VadState,
    wake_state: WakeState,
}

#[derive(Debug)]
enum AfeRuntimeError {
    InvalidFeedLength {
        expected: usize,
        actual: usize,
    },
    FeedFailed {
        code: i32,
    },
    NullFetchResult,
    FetchFailed {
        code: i32,
    },
    InvalidFetchData(&'static str),
    UnexpectedFetchSize {
        expected_bytes: usize,
        actual_bytes: usize,
    },
    InvalidWakeState(i32),
    InvalidVadState(u32),
    InvalidWakeMetadata(&'static str),
}

impl fmt::Display for AfeRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFeedLength { expected, actual } => write!(
                formatter,
                "ESP-SR feed requires exactly {expected} interleaved samples, got {actual}"
            ),
            Self::FeedFailed { code } => {
                write!(formatter, "ESP-SR feed failed with code {code}")
            }
            Self::NullFetchResult => write!(formatter, "ESP-SR fetch returned a null result"),
            Self::FetchFailed { code } => {
                write!(formatter, "ESP-SR fetch failed with code {code}")
            }
            Self::InvalidFetchData(reason) => write!(
                formatter,
                "ESP-SR fetch returned invalid audio metadata: {reason}"
            ),
            Self::UnexpectedFetchSize {
                expected_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "ESP-SR fetch returned {actual_bytes} audio bytes; expected {expected_bytes}"
            ),
            Self::InvalidWakeState(state) => {
                write!(
                    formatter,
                    "ESP-SR fetch returned invalid wake state {state}"
                )
            }
            Self::InvalidVadState(state) => {
                write!(formatter, "ESP-SR fetch returned invalid VAD state {state}")
            }
            Self::InvalidWakeMetadata(reason) => write!(
                formatter,
                "ESP-SR fetch returned invalid wake metadata: {reason}"
            ),
        }
    }
}

struct SpeechFrontend {
    inner: ffi::Frontend,
}

impl SpeechFrontend {
    fn open(model: WakeModel, input: InputFormat) -> Result<Self, SrError> {
        ffi::Frontend::open(model, input).map(|inner| Self { inner })
    }

    const fn frame_spec(&self) -> FrameSpec {
        self.inner.frame_spec()
    }

    const fn model_index(&self) -> usize {
        self.inner.model_index()
    }

    fn feed(&mut self, samples: &[i16]) -> Result<FeedStatus, SrError> {
        self.inner.feed(samples)
    }

    fn fetch(&mut self) -> Result<FetchedFrame<'_>, SrError> {
        self.inner.fetch()
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
    ErasedPack,
    PackedFileHashMismatch {
        model: &'static str,
        file: &'static str,
        actual: Sha256Digest,
    },
    InvalidPack(String),
    InsufficientMmapSpace {
        free_bytes: u64,
        required_bytes: u64,
    },
    AlreadyOpen,
    ExternalModelState,
    ModelLoadFailed,
    MissingWakeModel(String),
    InvalidModelList(&'static str),
    MissingAfeFunction(&'static str),
    AfeCreateFailed,
    UnsupportedInputFormat(InputFormat),
    InvalidAfeDimension(&'static str),
    UnexpectedAfeDimensions {
        sample_rate: usize,
        input_channels: usize,
        microphone_channels: usize,
    },
    AfeRuntime(AfeRuntimeError),
    InternalInvariant(&'static str),
}

impl From<AfeRuntimeError> for SrError {
    fn from(error: AfeRuntimeError) -> Self {
        Self::AfeRuntime(error)
    }
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
            Self::ErasedPack => write!(formatter, "model partition is erased"),
            Self::PackedFileHashMismatch {
                model,
                file,
                actual,
            } => write!(
                formatter,
                "model-pack file SHA-256 mismatch: model={model} file={file} got={actual}"
            ),
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
                write!(formatter, "ESP-SR rejected the reviewed model pack")
            }
            Self::MissingWakeModel(model) => {
                write!(formatter, "{model} is absent from the model pack")
            }
            Self::InvalidModelList(reason) => {
                write!(formatter, "ESP-SR returned an invalid model list: {reason}")
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
            Self::AfeRuntime(error) => error.fmt(formatter),
            Self::InternalInvariant(reason) => {
                write!(formatter, "internal wrapper invariant failed: {reason}")
            }
        }
    }
}

impl std::error::Error for SrError {}
