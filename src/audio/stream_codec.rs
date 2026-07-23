//! Owned streaming decoder and frame encoder handles.

#![allow(
    dead_code,
    reason = "the codec handles remain inactive until the Rust player is connected"
)]

use core::{
    ffi::{c_int, c_void},
    fmt,
    marker::PhantomData,
    mem::size_of,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, Ordering},
};
use std::rc::Rc;

use super::{codec_ffi::raw, pcm};

type RawStatus = raw::esp_audio_err_t;

const AUDIO_BITS_PER_SAMPLE: c_int = 16;
const AUDIO_CHANNELS: c_int = 1;
const OK: RawStatus = raw::esp_audio_err_t_ESP_AUDIO_ERR_OK;
const OUTPUT_BUFFER_TOO_SMALL: RawStatus = raw::esp_audio_err_t_ESP_AUDIO_ERR_BUFF_NOT_ENOUGH;
const INFORMATION_NOT_READY: RawStatus = raw::esp_audio_err_t_ESP_AUDIO_ERR_NOT_FOUND;
const REQUIRED_DECODERS: [(&str, raw::esp_audio_type_t); 8] = [
    ("AAC", raw::esp_audio_type_t_ESP_AUDIO_TYPE_AAC),
    ("ALAC", raw::esp_audio_type_t_ESP_AUDIO_TYPE_ALAC),
    ("AMR-NB", raw::esp_audio_type_t_ESP_AUDIO_TYPE_AMRNB),
    ("AMR-WB", raw::esp_audio_type_t_ESP_AUDIO_TYPE_AMRWB),
    ("FLAC", raw::esp_audio_type_t_ESP_AUDIO_TYPE_FLAC),
    ("MP3", raw::esp_audio_type_t_ESP_AUDIO_TYPE_MP3),
    ("Opus", raw::esp_audio_type_t_ESP_AUDIO_TYPE_OPUS),
    ("Vorbis", raw::esp_audio_type_t_ESP_AUDIO_TYPE_VORBIS),
];

static LIBRARY_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Encoded stream formats accepted by the playback pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StreamFormat {
    Aac,
    AmrNb,
    AmrWb,
    Flac,
    M4a,
    Mp3,
    Ogg,
    Opus,
    Pcm,
    TransportStream,
    Wav,
}

impl StreamFormat {
    const fn simple_raw(self) -> Option<raw::esp_audio_simple_dec_type_t> {
        match self {
            Self::Aac => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_AAC),
            Self::AmrNb => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_AMRNB),
            Self::AmrWb => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_AMRWB),
            Self::Flac => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_FLAC),
            Self::M4a => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_M4A),
            Self::Mp3 => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_MP3),
            Self::TransportStream => {
                Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_TS)
            }
            Self::Wav => Some(raw::esp_audio_simple_dec_type_t_ESP_AUDIO_SIMPLE_DEC_TYPE_WAV),
            Self::Ogg | Self::Opus | Self::Pcm => None,
        }
    }
}

impl fmt::Display for StreamFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Aac => "AAC",
            Self::AmrNb => "AMR-NB",
            Self::AmrWb => "AMR-WB",
            Self::Flac => "FLAC",
            Self::M4a => "M4A",
            Self::Mp3 => "MP3",
            Self::Ogg => "Ogg audio",
            Self::Opus => "Ogg Opus",
            Self::Pcm => "PCM",
            Self::TransportStream => "MPEG transport stream",
            Self::Wav => "WAV",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DecodedAudioInfo {
    pub(super) sample_rate_hz: u32,
    pub(super) bits_per_sample: u8,
    pub(super) channels: u8,
    pub(super) bitrate: u32,
    pub(super) frame_samples: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DecodeProgress {
    pub(super) consumed: usize,
    pub(super) produced: usize,
    pub(super) required_capacity: Option<usize>,
}

struct NativeDecodeProgress {
    status: RawStatus,
    consumed: u32,
    decoded: u32,
    needed: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EncoderFrameSizes {
    pub(super) input_bytes: usize,
    pub(super) output_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EncodeProgress {
    pub(super) output_bytes: usize,
    pub(super) presentation_time_ms: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum StreamCodecError {
    LibraryAlreadyOwned,
    RegisterDecoders {
        status: RawStatus,
    },
    RegisterStreamDecoders {
        status: RawStatus,
    },
    MissingDecoderRegistration {
        decoder: &'static str,
    },
    ConfigurationTooLarge {
        format: StreamFormat,
        bytes: usize,
    },
    OpenDecoder {
        format: StreamFormat,
        status: RawStatus,
    },
    NullDecoderHandle {
        format: StreamFormat,
    },
    DecoderRequiresContainerHeaders {
        format: StreamFormat,
    },
    InputTooLarge {
        bytes: usize,
    },
    OutputTooLarge {
        bytes: usize,
    },
    Decode {
        format: StreamFormat,
        status: RawStatus,
    },
    InvalidDecodeProgress {
        format: StreamFormat,
        consumed: u32,
        input: usize,
        decoded: u32,
        output: usize,
    },
    InvalidOutputRequirement {
        format: StreamFormat,
        required: u32,
        available: usize,
    },
    ReadDecoderInfo {
        format: StreamFormat,
        status: RawStatus,
    },
    EncoderSampleRateTooLarge {
        sample_rate_hz: u32,
    },
    EncoderConfigurationTooLarge {
        bytes: usize,
    },
    OpenAmrWbEncoder {
        status: RawStatus,
    },
    NullAmrWbEncoderHandle,
    ReadEncoderFrameSizes {
        status: RawStatus,
    },
    InvalidEncoderFrameSizes {
        input: c_int,
        output: c_int,
    },
    InvalidEncoderInput {
        expected: usize,
        actual: usize,
    },
    EncoderOutputTooSmall {
        required: usize,
        available: usize,
    },
    EncodeAmrWb {
        status: RawStatus,
    },
    InvalidEncodedSize {
        encoded: u32,
        available: usize,
    },
}

impl fmt::Display for StreamCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryAlreadyOwned => formatter.write_str("codec registry already has an owner"),
            Self::RegisterDecoders { status } => {
                write_codec_status(formatter, "register audio decoders", *status)
            }
            Self::RegisterStreamDecoders { status } => {
                write_codec_status(formatter, "register stream decoders", *status)
            }
            Self::MissingDecoderRegistration { decoder } => write!(formatter, "missing {decoder}"),
            Self::ConfigurationTooLarge { format, bytes } => write!(
                formatter,
                "{format} decoder configuration is {bytes} bytes and does not fit in int"
            ),
            Self::OpenDecoder { format, status } => write!(
                formatter,
                "failed to open {format} decoder: codec status {status}"
            ),
            Self::NullDecoderHandle { format } => write!(
                formatter,
                "opening the {format} decoder returned a null handle"
            ),
            Self::DecoderRequiresContainerHeaders { format } => {
                write!(formatter, "{format} requires container headers")
            }
            Self::InputTooLarge { bytes } => {
                write!(
                    formatter,
                    "codec input is {bytes} bytes and does not fit in uint32_t"
                )
            }
            Self::OutputTooLarge { bytes } => write!(
                formatter,
                "codec output is {bytes} bytes and does not fit in uint32_t"
            ),
            Self::Decode { format, status } => write!(
                formatter,
                "failed to decode {format}: codec status {status}"
            ),
            Self::InvalidDecodeProgress {
                format,
                consumed,
                input,
                decoded,
                output,
            } => write!(
                formatter,
                "{format} decoder reported {consumed}/{input} input bytes and {decoded}/{output} output bytes"
            ),
            Self::InvalidOutputRequirement {
                format,
                required,
                available,
            } => write!(
                formatter,
                "{format} decoder requested {required} output bytes with {available} already available"
            ),
            Self::ReadDecoderInfo { format, status } => write!(
                formatter,
                "failed to read {format} decoder information: codec status {status}"
            ),
            Self::EncoderSampleRateTooLarge { sample_rate_hz } => write!(
                formatter,
                "AMR-WB sample rate {sample_rate_hz} Hz does not fit in int"
            ),
            Self::EncoderConfigurationTooLarge { bytes } => write!(
                formatter,
                "AMR-WB encoder configuration is {bytes} bytes and does not fit in uint32_t"
            ),
            Self::OpenAmrWbEncoder { status } => {
                write_codec_status(formatter, "open the AMR-WB encoder", *status)
            }
            Self::NullAmrWbEncoderHandle => formatter.write_str("null AMR-WB encoder handle"),
            Self::ReadEncoderFrameSizes { status } => {
                write_codec_status(formatter, "read AMR-WB frame sizes", *status)
            }
            Self::InvalidEncoderFrameSizes { input, output } => write!(
                formatter,
                "AMR-WB encoder reported invalid frame sizes: input {input}, output {output}"
            ),
            Self::InvalidEncoderInput { expected, actual } => write!(
                formatter,
                "AMR-WB input frame is {actual} bytes; expected exactly {expected}"
            ),
            Self::EncoderOutputTooSmall {
                required,
                available,
            } => write!(
                formatter,
                "AMR-WB output has {available} bytes; at least {required} are required"
            ),
            Self::EncodeAmrWb { status } => write_codec_status(formatter, "encode AMR-WB", *status),
            Self::InvalidEncodedSize { encoded, available } => write!(
                formatter,
                "AMR-WB encoder reported {encoded} bytes with only {available} available"
            ),
        }
    }
}

impl std::error::Error for StreamCodecError {}

fn write_codec_status(
    formatter: &mut fmt::Formatter<'_>,
    operation: &str,
    status: RawStatus,
) -> fmt::Result {
    write!(formatter, "failed to {operation}: codec status {status}")
}

/// Owns the process-global decoder registrations.
///
/// Decoder handles borrow this value so the native registrations cannot be
/// removed while a decoder is active.
pub(super) struct CodecLibrary {
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl CodecLibrary {
    pub(super) fn new() -> Result<Self, StreamCodecError> {
        if LIBRARY_CLAIMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(StreamCodecError::LibraryAlreadyOwned);
        }

        // SAFETY: the atomic claim gives this owner exclusive access to the
        // component's process-global decoder registry.
        let status = unsafe { raw::esp_audio_dec_register_default() };
        if status != OK {
            // SAFETY: registration may have stopped after registering only a
            // subset of the enabled codecs, so undo every possible entry.
            unsafe { raw::esp_audio_dec_unregister_default() };
            LIBRARY_CLAIMED.store(false, Ordering::Release);
            return Err(StreamCodecError::RegisterDecoders { status });
        }
        if let Err(error) = verify_decoder_registrations() {
            // SAFETY: no handle exists yet, so all registrations can be
            // removed before giving up the exclusive claim.
            unsafe { raw::esp_audio_dec_unregister_default() };
            LIBRARY_CLAIMED.store(false, Ordering::Release);
            return Err(error);
        }

        // SAFETY: the base decoders are registered and remain owned until the
        // stream registrations are removed in Drop.
        let status = unsafe { raw::esp_audio_simple_dec_register_default() };
        if status != OK {
            // SAFETY: no decoder handle can exist because construction has not
            // returned. Either registration step may have stopped partway.
            unsafe {
                raw::esp_audio_simple_dec_unregister_default();
                raw::esp_audio_dec_unregister_default();
            }
            LIBRARY_CLAIMED.store(false, Ordering::Release);
            return Err(StreamCodecError::RegisterStreamDecoders { status });
        }

        Ok(Self {
            _not_send_or_sync: PhantomData,
        })
    }

    pub(super) fn open_decoder(
        &self,
        format: StreamFormat,
    ) -> Result<StreamDecoder<'_>, StreamCodecError> {
        StreamDecoder::open(self, format)
    }

    pub(super) fn open_opus_decoder(
        &self,
        sample_rate_hz: u32,
        channels: u8,
    ) -> Result<StreamDecoder<'_>, StreamCodecError> {
        StreamDecoder::open_opus(self, sample_rate_hz, channels)
    }

    pub(super) fn open_vorbis_decoder(
        &self,
        information_header: Vec<u8>,
        setup_header: Vec<u8>,
    ) -> Result<StreamDecoder<'_>, StreamCodecError> {
        StreamDecoder::open_vorbis(self, information_header, setup_header)
    }
}

fn verify_decoder_registrations() -> Result<(), StreamCodecError> {
    for (decoder, decoder_type) in REQUIRED_DECODERS {
        // SAFETY: this is a read-only query of the exclusively owned native
        // registry. A null operation table means the codec was not enabled.
        if unsafe { raw::esp_audio_dec_get_ops(decoder_type) }.is_null() {
            return Err(StreamCodecError::MissingDecoderRegistration { decoder });
        }
    }
    Ok(())
}

impl Drop for CodecLibrary {
    fn drop(&mut self) {
        // SAFETY: decoder lifetimes prevent a safe decoder handle from
        // outliving this owner. Registrations are removed in reverse order.
        unsafe {
            raw::esp_audio_simple_dec_unregister_default();
            raw::esp_audio_dec_unregister_default();
        }
        LIBRARY_CLAIMED.store(false, Ordering::Release);
    }
}

pub(super) struct StreamDecoder<'library> {
    format: StreamFormat,
    backend: DecoderBackend,
    _headers: Option<VorbisHeaders>,
    _library: PhantomData<&'library CodecLibrary>,
}

enum DecoderBackend {
    Base(NonNull<c_void>),
    Pcm,
    Simple(NonNull<c_void>),
}

struct VorbisHeaders {
    _information: Vec<u8>,
    _setup: Vec<u8>,
}

impl<'library> StreamDecoder<'library> {
    fn open(
        _library: &'library CodecLibrary,
        format: StreamFormat,
    ) -> Result<Self, StreamCodecError> {
        if matches!(format, StreamFormat::Ogg | StreamFormat::Opus) {
            return Err(StreamCodecError::DecoderRequiresContainerHeaders { format });
        }
        if matches!(format, StreamFormat::Pcm) {
            return Ok(Self {
                format,
                backend: DecoderBackend::Pcm,
                _headers: None,
                _library: PhantomData,
            });
        }

        let mut aac_configuration = raw::esp_aac_dec_cfg_t {
            aac_plus_enable: true,
            ..Default::default()
        };
        let mut m4a_configuration = raw::esp_m4a_dec_cfg_t {
            aac_plus_enable: true,
            ..Default::default()
        };
        let mut transport_stream_configuration = raw::esp_ts_dec_cfg_t {
            aac_plus_enable: true,
        };

        let (decoder_configuration, configuration_size) = match format {
            StreamFormat::Aac => native_configuration(format, &mut aac_configuration)?,
            StreamFormat::M4a => native_configuration(format, &mut m4a_configuration)?,
            StreamFormat::TransportStream => {
                native_configuration(format, &mut transport_stream_configuration)?
            }
            StreamFormat::AmrNb
            | StreamFormat::AmrWb
            | StreamFormat::Flac
            | StreamFormat::Mp3
            | StreamFormat::Wav => (ptr::null_mut(), 0),
            StreamFormat::Ogg | StreamFormat::Opus | StreamFormat::Pcm => {
                return Err(StreamCodecError::DecoderRequiresContainerHeaders { format });
            }
        };

        let mut configuration = raw::esp_audio_simple_dec_cfg_t {
            dec_type: format
                .simple_raw()
                .ok_or(StreamCodecError::DecoderRequiresContainerHeaders { format })?,
            dec_cfg: decoder_configuration,
            cfg_size: configuration_size,
        };
        let mut handle = ptr::null_mut();
        // SAFETY: all optional configuration values live through this call.
        // The component creates a new decoder handle and copies configuration.
        let status =
            unsafe { raw::esp_audio_simple_dec_open(&raw mut configuration, &raw mut handle) };
        if status != OK {
            return Err(StreamCodecError::OpenDecoder { format, status });
        }
        let raw = NonNull::new(handle).ok_or(StreamCodecError::NullDecoderHandle { format })?;

        Ok(Self {
            format,
            backend: DecoderBackend::Simple(raw),
            _headers: None,
            _library: PhantomData,
        })
    }

    fn open_opus(
        _library: &'library CodecLibrary,
        sample_rate_hz: u32,
        channels: u8,
    ) -> Result<Self, StreamCodecError> {
        let mut codec_configuration = raw::esp_opus_dec_cfg_t {
            sample_rate: sample_rate_hz,
            channel: channels,
            self_delimited: false,
        };
        Self::open_base(
            StreamFormat::Opus,
            raw::esp_audio_type_t_ESP_AUDIO_TYPE_OPUS,
            ptr::from_mut(&mut codec_configuration).cast(),
            size_of::<raw::esp_opus_dec_cfg_t>(),
            None,
        )
    }

    fn open_vorbis(
        _library: &'library CodecLibrary,
        mut information: Vec<u8>,
        mut setup: Vec<u8>,
    ) -> Result<Self, StreamCodecError> {
        let mut codec_configuration = raw::esp_vorbis_dec_cfg_t {
            info_header: information.as_mut_ptr(),
            info_size: u32::try_from(information.len()).map_err(|_| {
                StreamCodecError::ConfigurationTooLarge {
                    format: StreamFormat::Ogg,
                    bytes: information.len(),
                }
            })?,
            setup_header: setup.as_mut_ptr(),
            setup_size: u32::try_from(setup.len()).map_err(|_| {
                StreamCodecError::ConfigurationTooLarge {
                    format: StreamFormat::Ogg,
                    bytes: setup.len(),
                }
            })?,
        };
        Self::open_base(
            StreamFormat::Ogg,
            raw::esp_audio_type_t_ESP_AUDIO_TYPE_VORBIS,
            ptr::from_mut(&mut codec_configuration).cast(),
            size_of::<raw::esp_vorbis_dec_cfg_t>(),
            Some(VorbisHeaders {
                _information: information,
                _setup: setup,
            }),
        )
    }

    fn open_base(
        format: StreamFormat,
        decoder_type: raw::esp_audio_type_t,
        decoder_configuration: *mut c_void,
        configuration_bytes: usize,
        headers: Option<VorbisHeaders>,
    ) -> Result<Self, StreamCodecError> {
        let configuration_bytes = u32::try_from(configuration_bytes).map_err(|_| {
            StreamCodecError::ConfigurationTooLarge {
                format,
                bytes: configuration_bytes,
            }
        })?;
        let mut configuration = raw::esp_audio_dec_cfg_t {
            type_: decoder_type,
            cfg: decoder_configuration,
            cfg_sz: configuration_bytes,
        };
        let mut handle = ptr::null_mut();
        let status = unsafe { raw::esp_audio_dec_open(&raw mut configuration, &raw mut handle) };
        if status != OK {
            return Err(StreamCodecError::OpenDecoder { format, status });
        }
        let raw = NonNull::new(handle).ok_or(StreamCodecError::NullDecoderHandle { format })?;
        Ok(Self {
            format,
            backend: DecoderBackend::Base(raw),
            _headers: headers,
            _library: PhantomData,
        })
    }

    pub(super) fn process(
        &mut self,
        input: &mut [u8],
        end_of_stream: bool,
        output: &mut [u8],
    ) -> Result<DecodeProgress, StreamCodecError> {
        if matches!(self.backend, DecoderBackend::Pcm) {
            return Ok(process_pcm(input, output));
        }
        if end_of_stream && input.is_empty() && matches!(self.backend, DecoderBackend::Base(_)) {
            return Ok(DecodeProgress {
                consumed: 0,
                produced: 0,
                required_capacity: None,
            });
        }

        let input_length = u32::try_from(input.len())
            .map_err(|_| StreamCodecError::InputTooLarge { bytes: input.len() })?;
        let output_length =
            u32::try_from(output.len()).map_err(|_| StreamCodecError::OutputTooLarge {
                bytes: output.len(),
            })?;
        let progress = match self.backend {
            DecoderBackend::Simple(handle) => process_simple_decoder(
                handle,
                input,
                input_length,
                end_of_stream,
                output,
                output_length,
            ),
            DecoderBackend::Base(handle) => {
                process_base_decoder(handle, input, input_length, output, output_length)
            }
            DecoderBackend::Pcm => return Ok(process_pcm(input, output)),
        };
        if progress.status != OK && progress.status != OUTPUT_BUFFER_TOO_SMALL {
            return Err(StreamCodecError::Decode {
                format: self.format,
                status: progress.status,
            });
        }
        if progress.consumed > input_length || progress.decoded > output_length {
            return Err(StreamCodecError::InvalidDecodeProgress {
                format: self.format,
                consumed: progress.consumed,
                input: input.len(),
                decoded: progress.decoded,
                output: output.len(),
            });
        }

        let required_output_bytes = if progress.status == OUTPUT_BUFFER_TOO_SMALL {
            if progress.needed <= output_length {
                return Err(StreamCodecError::InvalidOutputRequirement {
                    format: self.format,
                    required: progress.needed,
                    available: output.len(),
                });
            }
            Some(progress.needed as usize)
        } else {
            None
        };

        Ok(DecodeProgress {
            consumed: progress.consumed as usize,
            produced: progress.decoded as usize,
            required_capacity: required_output_bytes,
        })
    }

    pub(super) fn information(&self) -> Result<Option<DecodedAudioInfo>, StreamCodecError> {
        if matches!(self.backend, DecoderBackend::Pcm) {
            return Ok(Some(DecodedAudioInfo {
                sample_rate_hz: pcm::OUTPUT_SAMPLE_RATE_HZ,
                bits_per_sample: 16,
                channels: 1,
                bitrate: pcm::OUTPUT_SAMPLE_RATE_HZ * 16,
                frame_samples: 1,
            }));
        }
        let (status, information) = match self.backend {
            DecoderBackend::Simple(handle) => {
                let mut information = raw::esp_audio_simple_dec_info_t::default();
                let status = unsafe {
                    raw::esp_audio_simple_dec_get_info(handle.as_ptr(), &raw mut information)
                };
                (
                    status,
                    DecodedAudioInfo {
                        sample_rate_hz: information.sample_rate,
                        bits_per_sample: information.bits_per_sample,
                        channels: information.channel,
                        bitrate: information.bitrate,
                        frame_samples: information.frame_size,
                    },
                )
            }
            DecoderBackend::Base(handle) => {
                let mut information = raw::esp_audio_dec_info_t::default();
                let status =
                    unsafe { raw::esp_audio_dec_get_info(handle.as_ptr(), &raw mut information) };
                (
                    status,
                    DecodedAudioInfo {
                        sample_rate_hz: information.sample_rate,
                        bits_per_sample: information.bits_per_sample,
                        channels: information.channel,
                        bitrate: information.bitrate,
                        frame_samples: information.frame_size,
                    },
                )
            }
            DecoderBackend::Pcm => {
                return Ok(Some(DecodedAudioInfo {
                    sample_rate_hz: pcm::OUTPUT_SAMPLE_RATE_HZ,
                    bits_per_sample: 16,
                    channels: 1,
                    bitrate: pcm::OUTPUT_SAMPLE_RATE_HZ * 16,
                    frame_samples: 1,
                }));
            }
        };
        if status == INFORMATION_NOT_READY {
            return Ok(None);
        }
        if status != OK {
            return Err(StreamCodecError::ReadDecoderInfo {
                format: self.format,
                status,
            });
        }

        Ok(Some(information))
    }
}

fn process_pcm(input: &mut [u8], output: &mut [u8]) -> DecodeProgress {
    let copied = input.len().min(output.len()) & !1;
    output[..copied].copy_from_slice(&input[..copied]);
    DecodeProgress {
        consumed: copied,
        produced: copied,
        required_capacity: (copied == 0 && !input.is_empty()).then_some(2),
    }
}

fn process_simple_decoder(
    handle: NonNull<c_void>,
    input: &mut [u8],
    input_length: u32,
    end_of_stream: bool,
    output: &mut [u8],
    output_length: u32,
) -> NativeDecodeProgress {
    let mut input_frame = raw::esp_audio_simple_dec_raw_t {
        buffer: input.as_mut_ptr(),
        len: input_length,
        eos: end_of_stream,
        consumed: 0,
    };
    let mut output_frame = raw::esp_audio_simple_dec_out_t {
        buffer: output.as_mut_ptr(),
        len: output_length,
        needed_size: 0,
        decoded_size: 0,
    };
    // SAFETY: the decoder is uniquely borrowed and both writable frames
    // describe live buffers using their checked lengths.
    let status = unsafe {
        raw::esp_audio_simple_dec_process(
            handle.as_ptr(),
            &raw mut input_frame,
            &raw mut output_frame,
        )
    };
    NativeDecodeProgress {
        status,
        consumed: input_frame.consumed,
        decoded: output_frame.decoded_size,
        needed: output_frame.needed_size,
    }
}

fn process_base_decoder(
    handle: NonNull<c_void>,
    input: &mut [u8],
    input_length: u32,
    output: &mut [u8],
    output_length: u32,
) -> NativeDecodeProgress {
    let mut input_frame = raw::esp_audio_dec_in_raw_t {
        buffer: input.as_mut_ptr(),
        len: input_length,
        consumed: 0,
    };
    let mut output_frame = raw::esp_audio_dec_out_frame_t {
        buffer: output.as_mut_ptr(),
        len: output_length,
        needed_size: 0,
        decoded_size: 0,
    };
    // SAFETY: the decoder is uniquely borrowed and both writable frames
    // describe live buffers using their checked lengths. The component's
    // header incorrectly declares a pointer to its void-pointer handle; its C
    // implementation and tests pass the handle value itself.
    let status = unsafe {
        raw::esp_audio_dec_process(
            handle.as_ptr().cast::<*mut c_void>(),
            &raw mut input_frame,
            &raw mut output_frame,
        )
    };
    NativeDecodeProgress {
        status,
        consumed: input_frame.consumed,
        decoded: output_frame.decoded_size,
        needed: output_frame.needed_size,
    }
}

impl Drop for StreamDecoder<'_> {
    fn drop(&mut self) {
        // SAFETY: this owner closes its unique decoder handle exactly once,
        // while the borrowed registry is still live.
        unsafe {
            match self.backend {
                DecoderBackend::Simple(handle) => raw::esp_audio_simple_dec_close(handle.as_ptr()),
                DecoderBackend::Base(handle) => raw::esp_audio_dec_close(handle.as_ptr()),
                DecoderBackend::Pcm => {}
            }
        };
    }
}

fn native_configuration<T>(
    format: StreamFormat,
    configuration: &mut T,
) -> Result<(*mut c_void, c_int), StreamCodecError> {
    let bytes = size_of::<T>();
    let bytes = c_int::try_from(bytes)
        .map_err(|_| StreamCodecError::ConfigurationTooLarge { format, bytes })?;
    Ok((ptr::from_mut(configuration).cast(), bytes))
}

pub(super) struct AmrWbEncoder {
    raw: NonNull<c_void>,
    frame_sizes: EncoderFrameSizes,
}

impl AmrWbEncoder {
    pub(super) fn new() -> Result<Self, StreamCodecError> {
        let mut configuration = raw::esp_amrwb_enc_config_t {
            sample_rate: c_int::try_from(pcm::OUTPUT_SAMPLE_RATE_HZ).map_err(|_| {
                StreamCodecError::EncoderSampleRateTooLarge {
                    sample_rate_hz: pcm::OUTPUT_SAMPLE_RATE_HZ,
                }
            })?,
            channel: AUDIO_CHANNELS,
            bits_per_sample: AUDIO_BITS_PER_SAMPLE,
            dtx_enable: false,
            bitrate_mode: raw::esp_amrwb_enc_bitrate_t_ESP_AMRWB_ENC_BITRATE_MD2385,
            // Preserve the old recorder's `contain_amrwb_header = true`.
            no_file_header: false,
        };
        let configuration_size =
            u32::try_from(size_of::<raw::esp_amrwb_enc_config_t>()).map_err(|_| {
                StreamCodecError::EncoderConfigurationTooLarge {
                    bytes: size_of::<raw::esp_amrwb_enc_config_t>(),
                }
            })?;
        let mut handle = ptr::null_mut();
        // SAFETY: the component copies the configuration while opening a new
        // handle, which is written to the valid out pointer.
        let status = unsafe {
            raw::esp_amrwb_enc_open(
                ptr::from_mut(&mut configuration).cast(),
                configuration_size,
                &raw mut handle,
            )
        };
        if status != OK {
            return Err(StreamCodecError::OpenAmrWbEncoder { status });
        }
        let raw = NonNull::new(handle).ok_or(StreamCodecError::NullAmrWbEncoderHandle)?;

        let frame_sizes = match read_encoder_frame_sizes(raw) {
            Ok(frame_sizes) => frame_sizes,
            Err(error) => {
                // SAFETY: opening succeeded, but ownership cannot be returned
                // without valid frame sizes. Close the unique handle now.
                unsafe { raw::esp_amrwb_enc_close(raw.as_ptr()) };
                return Err(error);
            }
        };
        Ok(Self { raw, frame_sizes })
    }

    pub(super) const fn frame_sizes(&self) -> EncoderFrameSizes {
        self.frame_sizes
    }

    pub(super) fn encode_frame(
        &mut self,
        input: &mut [u8],
        output: &mut [u8],
    ) -> Result<EncodeProgress, StreamCodecError> {
        if input.len() != self.frame_sizes.input_bytes {
            return Err(StreamCodecError::InvalidEncoderInput {
                expected: self.frame_sizes.input_bytes,
                actual: input.len(),
            });
        }
        if output.len() < self.frame_sizes.output_bytes {
            return Err(StreamCodecError::EncoderOutputTooSmall {
                required: self.frame_sizes.output_bytes,
                available: output.len(),
            });
        }

        let input_length = u32::try_from(input.len())
            .map_err(|_| StreamCodecError::InputTooLarge { bytes: input.len() })?;
        let output_length =
            u32::try_from(output.len()).map_err(|_| StreamCodecError::OutputTooLarge {
                bytes: output.len(),
            })?;
        let mut input_frame = raw::esp_audio_enc_in_frame_t {
            buffer: input.as_mut_ptr(),
            len: input_length,
        };
        let mut output_frame = raw::esp_audio_enc_out_frame_t {
            buffer: output.as_mut_ptr(),
            len: output_length,
            encoded_bytes: 0,
            pts: 0,
        };

        // SAFETY: the encoder is uniquely borrowed, and both writable buffers
        // remain live for the duration described by their checked lengths.
        let status = unsafe {
            raw::esp_amrwb_enc_process(
                self.raw.as_ptr(),
                &raw mut input_frame,
                &raw mut output_frame,
            )
        };
        if status != OK {
            return Err(StreamCodecError::EncodeAmrWb { status });
        }
        if output_frame.encoded_bytes > output_length {
            return Err(StreamCodecError::InvalidEncodedSize {
                encoded: output_frame.encoded_bytes,
                available: output.len(),
            });
        }

        Ok(EncodeProgress {
            output_bytes: output_frame.encoded_bytes as usize,
            presentation_time_ms: output_frame.pts,
        })
    }
}

impl Drop for AmrWbEncoder {
    fn drop(&mut self) {
        // SAFETY: this owner closes its unique encoder handle exactly once.
        unsafe { raw::esp_amrwb_enc_close(self.raw.as_ptr()) };
    }
}

fn read_encoder_frame_sizes(
    encoder: NonNull<c_void>,
) -> Result<EncoderFrameSizes, StreamCodecError> {
    let mut input = 0;
    let mut output = 0;
    // SAFETY: the encoder handle is live and both out pointers are valid.
    let status = unsafe {
        raw::esp_amrwb_enc_get_frame_size(encoder.as_ptr(), &raw mut input, &raw mut output)
    };
    if status != OK {
        return Err(StreamCodecError::ReadEncoderFrameSizes { status });
    }
    if input <= 0 || output <= 0 {
        return Err(StreamCodecError::InvalidEncoderFrameSizes { input, output });
    }

    Ok(EncoderFrameSizes {
        input_bytes: usize::try_from(input)
            .map_err(|_| StreamCodecError::InvalidEncoderFrameSizes { input, output })?,
        output_bytes: usize::try_from(output)
            .map_err(|_| StreamCodecError::InvalidEncoderFrameSizes { input, output })?,
    })
}
