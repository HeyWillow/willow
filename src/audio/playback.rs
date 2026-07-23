//! Bounded synchronous playback from encoded readers to I2S0.

#![allow(
    dead_code,
    reason = "the playback pump remains inactive until Rust owns runtime audio"
)]

use core::{fmt, slice};
use std::io::{self, Read};

use ogg::{
    OggReadError, Packet,
    reading::{BasePacketReader, PageParser},
};

use super::{
    i2s::{I2sError, TransmitChannel},
    ogg_headers::{self, OggCodec, PacketSizeTracker, PcmTrimmer},
    pcm::{ConversionProgress, PcmConverter, PcmError},
    stream_codec::{CodecLibrary, DecodedAudioInfo, StreamCodecError, StreamDecoder, StreamFormat},
};

const MAXIMUM_END_OF_STREAM_CALLS: usize = 64;
const MAXIMUM_OGG_PACKET_BYTES: usize = 128 * 1024;
const TRANSMIT_TIMEOUT_MS: u32 = 100;

#[derive(Debug)]
pub(super) enum PlaybackError {
    Cancelled,
    EmptyWorkspace {
        buffer: &'static str,
    },
    MisalignedWorkspace {
        buffer: &'static str,
        samples: usize,
    },
    Read {
        source: io::Error,
    },
    OggRead {
        source: OggReadError,
    },
    MissingOggHeader {
        header: &'static str,
    },
    InvalidOggHeader {
        format: StreamFormat,
        header: &'static str,
    },
    InvalidOggCapturePattern {
        capture: [u8; 4],
    },
    OggPacketTooLarge {
        bytes: usize,
        limit: usize,
    },
    UnexpectedOggStream {
        expected: u32,
        actual: u32,
    },
    InvalidOggFinalGranule {
        granule: u64,
        decoded_frames: u64,
    },
    Codec {
        source: StreamCodecError,
    },
    DecoderOutputTooSmall {
        required: usize,
        available: usize,
    },
    DecoderStalled {
        remaining: usize,
        end_of_stream: bool,
    },
    ExcessiveEndOfStreamOutput {
        calls: usize,
    },
    MissingAudioInformation,
    UnsupportedDecodedLayout {
        sample_rate_hz: u32,
        bits_per_sample: u8,
        channels: u8,
    },
    ChangedDecodedLayout {
        previous: DecodedAudioInfo,
        current: DecodedAudioInfo,
    },
    MisalignedDecodedPcm {
        bytes: usize,
    },
    DecodedSampleBufferTooSmall {
        required: usize,
        available: usize,
    },
    Pcm {
        source: PcmError,
    },
    PcmConverterStalled {
        remaining_frames: usize,
    },
    I2s {
        source: I2sError,
    },
    I2sWriteStalled {
        remaining: usize,
    },
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("audio playback was cancelled"),
            Self::EmptyWorkspace { buffer } => {
                write!(formatter, "playback {buffer} workspace is empty")
            }
            Self::MisalignedWorkspace { buffer, samples } => write!(
                formatter,
                "playback {buffer} workspace has {samples} samples; stereo output requires an even count"
            ),
            Self::Read { source } => write!(formatter, "failed to read audio source: {source}"),
            Self::OggRead { source } => write!(formatter, "failed to read Ogg stream: {source}"),
            Self::MissingOggHeader { header } => {
                write!(formatter, "Ogg stream ended before its {header} header")
            }
            Self::InvalidOggHeader { format, header } => {
                write!(formatter, "invalid {header} header for {format}")
            }
            Self::InvalidOggCapturePattern { capture } => write!(
                formatter,
                "invalid Ogg capture pattern {capture:02x?}; expected OggS"
            ),
            Self::OggPacketTooLarge { bytes, limit } => write!(
                formatter,
                "Ogg packet is {bytes} bytes; the playback limit is {limit} bytes"
            ),
            Self::UnexpectedOggStream { expected, actual } => write!(
                formatter,
                "Ogg stream changed serial number from {expected} to {actual}"
            ),
            Self::InvalidOggFinalGranule {
                granule,
                decoded_frames,
            } => write!(
                formatter,
                "Ogg final granule {granule} precedes {decoded_frames} already-decoded frames"
            ),
            Self::Codec { source } => write!(formatter, "stream codec failed: {source}"),
            Self::DecoderOutputTooSmall {
                required,
                available,
            } => write!(
                formatter,
                "decoder requires {required} output bytes; workspace has {available}"
            ),
            Self::DecoderStalled {
                remaining,
                end_of_stream,
            } => write!(
                formatter,
                "decoder made no progress with {remaining} input bytes and end_of_stream={end_of_stream}"
            ),
            Self::ExcessiveEndOfStreamOutput { calls } => write!(
                formatter,
                "decoder still produced end-of-stream output after {calls} calls"
            ),
            Self::MissingAudioInformation => {
                formatter.write_str("decoder produced PCM without audio information")
            }
            Self::UnsupportedDecodedLayout {
                sample_rate_hz,
                bits_per_sample,
                channels,
            } => write!(
                formatter,
                "decoder produced unsupported {sample_rate_hz} Hz, {bits_per_sample}-bit, {channels}-channel PCM"
            ),
            Self::ChangedDecodedLayout { previous, current } => write!(
                formatter,
                "decoded PCM layout changed during playback: {previous:?} to {current:?}"
            ),
            Self::MisalignedDecodedPcm { bytes } => write!(
                formatter,
                "decoder produced {bytes} bytes, which is not signed 16-bit PCM"
            ),
            Self::DecodedSampleBufferTooSmall {
                required,
                available,
            } => write!(
                formatter,
                "decoded PCM has {required} samples; workspace has {available}"
            ),
            Self::Pcm { source } => write!(formatter, "PCM conversion failed: {source}"),
            Self::PcmConverterStalled { remaining_frames } => write!(
                formatter,
                "PCM converter made no progress with {remaining_frames} frames remaining"
            ),
            Self::I2s { source } => write!(formatter, "I2S playback failed: {source}"),
            Self::I2sWriteStalled { remaining } => write!(
                formatter,
                "I2S0 accepted no data with {remaining} playback bytes remaining"
            ),
        }
    }
}

impl std::error::Error for PlaybackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source } => Some(source),
            Self::OggRead { source } => Some(source),
            Self::Codec { source } => Some(source),
            Self::Pcm { source } => Some(source),
            Self::I2s { source } => Some(source),
            _ => None,
        }
    }
}

impl From<StreamCodecError> for PlaybackError {
    fn from(source: StreamCodecError) -> Self {
        Self::Codec { source }
    }
}

impl From<PcmError> for PlaybackError {
    fn from(source: PcmError) -> Self {
        Self::Pcm { source }
    }
}

impl From<I2sError> for PlaybackError {
    fn from(source: I2sError) -> Self {
        Self::I2s { source }
    }
}

impl PlaybackError {
    pub(super) const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Caller-owned storage reused throughout one or more playback operations.
pub(super) struct PlaybackWorkspace<'buffer> {
    encoded: &'buffer mut [u8],
    decoded: &'buffer mut [u8],
    decoded_samples: &'buffer mut [i16],
    i2s_samples: &'buffer mut [i32],
}

impl<'buffer> PlaybackWorkspace<'buffer> {
    pub(super) fn new(
        encoded: &'buffer mut [u8],
        decoded: &'buffer mut [u8],
        decoded_samples: &'buffer mut [i16],
        i2s_samples: &'buffer mut [i32],
    ) -> Result<Self, PlaybackError> {
        for (buffer, empty) in [
            ("encoded", encoded.is_empty()),
            ("decoded", decoded.is_empty()),
            ("decoded sample", decoded_samples.is_empty()),
            ("I2S sample", i2s_samples.is_empty()),
        ] {
            if empty {
                return Err(PlaybackError::EmptyWorkspace { buffer });
            }
        }
        if i2s_samples.len() % 2 != 0 {
            return Err(PlaybackError::MisalignedWorkspace {
                buffer: "I2S sample",
                samples: i2s_samples.len(),
            });
        }

        Ok(Self {
            encoded,
            decoded,
            decoded_samples,
            i2s_samples,
        })
    }
}

pub(super) fn play_reader(
    reader: &mut impl Read,
    format: StreamFormat,
    codecs: &CodecLibrary,
    transmit: &mut TransmitChannel,
    workspace: &mut PlaybackWorkspace<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), PlaybackError> {
    ensure_not_cancelled(cancelled)?;
    if matches!(format, StreamFormat::Ogg | StreamFormat::Opus) {
        return play_ogg_reader(reader, format, codecs, transmit, workspace, cancelled);
    }
    let mut decoder = codecs.open_decoder(format)?;
    let mut pcm_state = None;
    let mut transform = PlaybackTransform::default();

    loop {
        let bytes_read = read_source(reader, workspace.encoded, cancelled)?;
        if bytes_read == 0 {
            let mut output = DecodeOutput {
                pcm_bytes: workspace.decoded,
                pcm_samples: workspace.decoded_samples,
                i2s_samples: workspace.i2s_samples,
                transmit,
                pcm_state: &mut pcm_state,
                transform: &mut transform,
                cancelled,
            };
            decode_chunk(&mut decoder, &mut [], true, &mut output)?;
            ensure_not_cancelled(cancelled)?;
            return Ok(());
        }

        let mut output = DecodeOutput {
            pcm_bytes: workspace.decoded,
            pcm_samples: workspace.decoded_samples,
            i2s_samples: workspace.i2s_samples,
            transmit,
            pcm_state: &mut pcm_state,
            transform: &mut transform,
            cancelled,
        };
        decode_chunk(
            &mut decoder,
            &mut workspace.encoded[..bytes_read],
            false,
            &mut output,
        )?;
    }
}

fn play_ogg_reader(
    reader: &mut impl Read,
    format: StreamFormat,
    codecs: &CodecLibrary,
    transmit: &mut TransmitChannel,
    workspace: &mut PlaybackWorkspace<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), PlaybackError> {
    let mut packets = BoundedOggPacketReader::new(reader);
    let information = read_ogg_packet(&mut packets, "identification", cancelled)?;
    let stream_serial = information.stream_serial();
    let codec =
        ogg_headers::identify(&information.data).ok_or(PlaybackError::InvalidOggHeader {
            format,
            header: "identification",
        })?;
    let (mut decoder, mut transform) = match (format, codec) {
        (StreamFormat::Ogg, OggCodec::Vorbis) => {
            let comment =
                read_ogg_stream_packet(&mut packets, stream_serial, "comment", cancelled)?;
            validate_vorbis_header(&comment.data, 3, "comment")?;
            let setup = read_ogg_stream_packet(&mut packets, stream_serial, "setup", cancelled)?;
            validate_vorbis_header(&setup.data, 5, "setup")?;
            (
                codecs.open_vorbis_decoder(information.data, setup.data)?,
                PlaybackTransform::ogg(0, 0),
            )
        }
        (
            StreamFormat::Ogg | StreamFormat::Opus,
            OggCodec::Opus {
                channels,
                pre_skip,
                output_gain_q8,
            },
        ) => {
            let tags = read_ogg_stream_packet(&mut packets, stream_serial, "tags", cancelled)?;
            if !ogg_headers::is_opus_tags(&tags.data) {
                return Err(PlaybackError::InvalidOggHeader {
                    format,
                    header: "tags",
                });
            }
            (
                codecs.open_opus_decoder(48_000, channels)?,
                PlaybackTransform::opus(pre_skip, output_gain_q8),
            )
        }
        _ => {
            return Err(PlaybackError::InvalidOggHeader {
                format,
                header: "identification",
            });
        }
    };
    let mut pcm_state = None;

    while let Some(mut packet) = read_optional_ogg_packet(&mut packets, cancelled)? {
        validate_ogg_packet(&packet, stream_serial)?;
        let last_in_stream = packet.last_in_stream();
        if last_in_stream {
            transform.set_final_granule(packet.absgp_page())?;
        }
        let mut output = DecodeOutput {
            pcm_bytes: workspace.decoded,
            pcm_samples: workspace.decoded_samples,
            i2s_samples: workspace.i2s_samples,
            transmit,
            pcm_state: &mut pcm_state,
            transform: &mut transform,
            cancelled,
        };
        decode_chunk(&mut decoder, &mut packet.data, false, &mut output)?;
        if last_in_stream {
            break;
        }
    }

    let mut output = DecodeOutput {
        pcm_bytes: workspace.decoded,
        pcm_samples: workspace.decoded_samples,
        i2s_samples: workspace.i2s_samples,
        transmit,
        pcm_state: &mut pcm_state,
        transform: &mut transform,
        cancelled,
    };
    decode_chunk(&mut decoder, &mut [], true, &mut output)
}

fn read_ogg_packet<R: Read>(
    packets: &mut BoundedOggPacketReader<'_, R>,
    header: &'static str,
    cancelled: &dyn Fn() -> bool,
) -> Result<Packet, PlaybackError> {
    read_optional_ogg_packet(packets, cancelled)?.ok_or(PlaybackError::MissingOggHeader { header })
}

fn read_ogg_stream_packet<R: Read>(
    packets: &mut BoundedOggPacketReader<'_, R>,
    stream_serial: u32,
    header: &'static str,
    cancelled: &dyn Fn() -> bool,
) -> Result<Packet, PlaybackError> {
    let packet = read_ogg_packet(packets, header, cancelled)?;
    validate_ogg_packet(&packet, stream_serial)?;
    Ok(packet)
}

fn read_optional_ogg_packet<R: Read>(
    packets: &mut BoundedOggPacketReader<'_, R>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Packet>, PlaybackError> {
    ensure_not_cancelled(cancelled)?;
    let packet = packets.read_packet(cancelled)?;
    ensure_not_cancelled(cancelled)?;
    if let Some(packet) = &packet
        && packet.data.len() > MAXIMUM_OGG_PACKET_BYTES
    {
        return Err(PlaybackError::OggPacketTooLarge {
            bytes: packet.data.len(),
            limit: MAXIMUM_OGG_PACKET_BYTES,
        });
    }
    Ok(packet)
}

fn validate_ogg_packet(packet: &Packet, expected: u32) -> Result<(), PlaybackError> {
    let actual = packet.stream_serial();
    if actual == expected {
        Ok(())
    } else {
        Err(PlaybackError::UnexpectedOggStream { expected, actual })
    }
}

fn validate_vorbis_header(
    packet: &[u8],
    packet_type: u8,
    header: &'static str,
) -> Result<(), PlaybackError> {
    if ogg_headers::is_vorbis_header(packet, packet_type) {
        Ok(())
    } else {
        Err(PlaybackError::InvalidOggHeader {
            format: StreamFormat::Ogg,
            header,
        })
    }
}

struct BoundedOggPacketReader<'reader, Reader> {
    reader: &'reader mut Reader,
    packets: BasePacketReader,
    packet_sizes: PacketSizeTracker,
    stream_serial: Option<u32>,
}

impl<'reader, Reader: Read> BoundedOggPacketReader<'reader, Reader> {
    fn new(reader: &'reader mut Reader) -> Self {
        Self {
            reader,
            packets: BasePacketReader::new(),
            packet_sizes: PacketSizeTracker::default(),
            stream_serial: None,
        }
    }

    fn read_packet(
        &mut self,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Packet>, PlaybackError> {
        loop {
            if let Some(packet) = self.packets.read_packet() {
                return Ok(Some(packet));
            }
            let Some(page) = self.read_page(cancelled)? else {
                return Ok(None);
            };
            self.packets
                .push_page(page)
                .map_err(|source| PlaybackError::OggRead { source })?;
        }
    }

    fn read_page(
        &mut self,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<ogg::reading::OggPage>, PlaybackError> {
        let mut header = [0_u8; 27];
        if read_source(self.reader, &mut header[..1], cancelled)? == 0 {
            return Ok(None);
        }
        read_exact_source(self.reader, &mut header[1..], cancelled)?;
        if header[..4] != *b"OggS" {
            return Err(PlaybackError::InvalidOggCapturePattern {
                capture: [header[0], header[1], header[2], header[3]],
            });
        }
        let stream_serial = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
        if let Some(expected) = self.stream_serial {
            if stream_serial != expected {
                return Err(PlaybackError::UnexpectedOggStream {
                    expected,
                    actual: stream_serial,
                });
            }
        } else {
            self.stream_serial = Some(stream_serial);
        }

        let (mut parser, segment_count) =
            PageParser::new(header).map_err(|source| PlaybackError::OggRead { source })?;
        let mut segments = vec![0_u8; segment_count];
        read_exact_source(self.reader, &mut segments, cancelled)?;
        self.packet_sizes
            .observe_page(header[5] & 1 != 0, &segments, MAXIMUM_OGG_PACKET_BYTES)
            .map_err(|source| PlaybackError::OggPacketTooLarge {
                bytes: source.bytes,
                limit: source.limit,
            })?;

        let page_bytes = parser.parse_segments(segments);
        let mut body = vec![0_u8; page_bytes];
        read_exact_source(self.reader, &mut body, cancelled)?;
        parser
            .parse_packet_data(body)
            .map(Some)
            .map_err(|source| PlaybackError::OggRead { source })
    }
}

fn ensure_not_cancelled(cancelled: &dyn Fn() -> bool) -> Result<(), PlaybackError> {
    if cancelled() {
        Err(PlaybackError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_source(
    reader: &mut impl Read,
    buffer: &mut [u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<usize, PlaybackError> {
    loop {
        ensure_not_cancelled(cancelled)?;
        match reader.read(buffer) {
            Ok(bytes) => {
                ensure_not_cancelled(cancelled)?;
                return Ok(bytes);
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(PlaybackError::Read { source }),
        }
    }
}

fn read_exact_source(
    reader: &mut impl Read,
    buffer: &mut [u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<(), PlaybackError> {
    let mut filled = 0;
    while filled < buffer.len() {
        let bytes = read_source(reader, &mut buffer[filled..], cancelled)?;
        if bytes == 0 {
            return Err(PlaybackError::Read {
                source: io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Ogg stream ended in the middle of a page",
                ),
            });
        }
        filled += bytes;
    }
    Ok(())
}

struct DecodeOutput<'buffer, 'state, 'cancelled> {
    pcm_bytes: &'buffer mut [u8],
    pcm_samples: &'buffer mut [i16],
    i2s_samples: &'buffer mut [i32],
    transmit: &'state mut TransmitChannel,
    pcm_state: &'state mut Option<PcmState>,
    transform: &'state mut PlaybackTransform,
    cancelled: &'cancelled dyn Fn() -> bool,
}

fn decode_chunk(
    codec: &mut StreamDecoder<'_>,
    input: &mut [u8],
    end_of_stream: bool,
    output: &mut DecodeOutput<'_, '_, '_>,
) -> Result<(), PlaybackError> {
    let mut consumed = 0;
    let mut end_of_stream_calls = 0;

    loop {
        ensure_not_cancelled(output.cancelled)?;
        let progress = codec.process(&mut input[consumed..], end_of_stream, output.pcm_bytes)?;
        ensure_not_cancelled(output.cancelled)?;
        if let Some(required) = progress.required_capacity {
            return Err(PlaybackError::DecoderOutputTooSmall {
                required,
                available: output.pcm_bytes.len(),
            });
        }
        consumed += progress.consumed;

        if progress.produced > 0 {
            convert_and_write(codec, progress.produced, output)?;
        }

        if consumed < input.len() {
            if progress.consumed == 0 && progress.produced == 0 {
                return Err(PlaybackError::DecoderStalled {
                    remaining: input.len() - consumed,
                    end_of_stream,
                });
            }
            continue;
        }
        if !end_of_stream || progress.produced == 0 {
            return Ok(());
        }

        end_of_stream_calls += 1;
        if end_of_stream_calls == MAXIMUM_END_OF_STREAM_CALLS {
            return Err(PlaybackError::ExcessiveEndOfStreamOutput {
                calls: end_of_stream_calls,
            });
        }
    }
}

fn convert_and_write(
    codec: &StreamDecoder<'_>,
    produced: usize,
    output: &mut DecodeOutput<'_, '_, '_>,
) -> Result<(), PlaybackError> {
    ensure_not_cancelled(output.cancelled)?;
    let information = codec
        .information()?
        .ok_or(PlaybackError::MissingAudioInformation)?;
    validate_information(information)?;

    let state = match output.pcm_state {
        Some(state) => {
            if !same_pcm_layout(state.information, information) {
                return Err(PlaybackError::ChangedDecodedLayout {
                    previous: state.information,
                    current: information,
                });
            }
            state
        }
        None => output.pcm_state.insert(PcmState {
            information,
            converter: PcmConverter::new(
                information.sample_rate_hz,
                usize::from(information.channels),
            )?,
        }),
    };

    let pcm_bytes = &output.pcm_bytes[..produced];
    if pcm_bytes.len() % size_of_i16() != 0 {
        return Err(PlaybackError::MisalignedDecodedPcm {
            bytes: pcm_bytes.len(),
        });
    }
    let samples = pcm_bytes.len() / size_of_i16();
    if samples > output.pcm_samples.len() {
        return Err(PlaybackError::DecodedSampleBufferTooSmall {
            required: samples,
            available: output.pcm_samples.len(),
        });
    }
    for (sample, bytes) in output.pcm_samples.iter_mut().zip(pcm_bytes.chunks_exact(2)) {
        *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
    }

    let channels = usize::from(information.channels);
    let frames = samples / channels;
    let retained_frames = output.transform.retain_decoded_frames(frames);
    let skipped_frames = retained_frames.min(output.transform.remaining_skip_frames);
    output.transform.remaining_skip_frames -= skipped_frames;
    let retained_frames = retained_frames.saturating_sub(skipped_frames);
    let retained_samples = skipped_frames
        .saturating_add(retained_frames)
        .saturating_mul(channels);
    let samples = &mut output.pcm_samples[skipped_frames * channels..retained_samples];
    output.transform.apply_gain(samples);

    convert_samples(
        &mut state.converter,
        samples,
        channels,
        output.i2s_samples,
        output.transmit,
        output.cancelled,
    )
}

const fn same_pcm_layout(previous: DecodedAudioInfo, current: DecodedAudioInfo) -> bool {
    previous.sample_rate_hz == current.sample_rate_hz
        && previous.bits_per_sample == current.bits_per_sample
        && previous.channels == current.channels
}

const fn size_of_i16() -> usize {
    core::mem::size_of::<i16>()
}

fn validate_information(information: DecodedAudioInfo) -> Result<(), PlaybackError> {
    if information.bits_per_sample != 16 || !matches!(information.channels, 1 | 2) {
        return Err(PlaybackError::UnsupportedDecodedLayout {
            sample_rate_hz: information.sample_rate_hz,
            bits_per_sample: information.bits_per_sample,
            channels: information.channels,
        });
    }
    Ok(())
}

struct PcmState {
    information: DecodedAudioInfo,
    converter: PcmConverter,
}

struct PlaybackTransform {
    remaining_skip_frames: usize,
    output_gain: f32,
    adjust_gain: bool,
    ogg_trimmer: PcmTrimmer,
    tracks_ogg_granule: bool,
}

impl PlaybackTransform {
    fn opus(pre_skip: u16, output_gain_q8: i16) -> Self {
        Self::ogg(pre_skip, output_gain_q8)
    }

    fn ogg(pre_skip: u16, output_gain_q8: i16) -> Self {
        let decibels = f32::from(output_gain_q8) / 256.0;
        Self {
            remaining_skip_frames: usize::from(pre_skip),
            output_gain: 10.0_f32.powf(decibels / 20.0),
            adjust_gain: output_gain_q8 != 0,
            ogg_trimmer: PcmTrimmer::default(),
            tracks_ogg_granule: true,
        }
    }

    fn set_final_granule(&mut self, granule: u64) -> Result<(), PlaybackError> {
        self.ogg_trimmer
            .set_final_granule(granule)
            .map_err(|source| PlaybackError::InvalidOggFinalGranule {
                granule: source.granule,
                decoded_frames: source.decoded_frames,
            })
    }

    fn retain_decoded_frames(&mut self, frames: usize) -> usize {
        if !self.tracks_ogg_granule {
            return frames;
        }
        self.ogg_trimmer.retain_decoded_frames(frames)
    }

    fn apply_gain(&self, samples: &mut [i16]) {
        if !self.adjust_gain {
            return;
        }
        for sample in samples {
            let amplified = (f32::from(*sample) * self.output_gain)
                .round()
                .clamp(f32::from(i16::MIN), f32::from(i16::MAX));
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the rounded gain result is clamped to the complete i16 range"
            )]
            let amplified = amplified as i16;
            *sample = amplified;
        }
    }
}

impl Default for PlaybackTransform {
    fn default() -> Self {
        Self {
            remaining_skip_frames: 0,
            output_gain: 1.0,
            adjust_gain: false,
            ogg_trimmer: PcmTrimmer::default(),
            tracks_ogg_granule: false,
        }
    }
}

fn convert_samples(
    converter: &mut PcmConverter,
    input: &[i16],
    channels: usize,
    output: &mut [i32],
    transmit: &mut TransmitChannel,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), PlaybackError> {
    let input_frames = input.len() / channels;
    let mut consumed_frames = 0;

    loop {
        ensure_not_cancelled(cancelled)?;
        let progress = converter.process(&input[consumed_frames * channels..], output)?;
        consumed_frames += progress.input_frames;
        write_converted(progress, output, transmit, cancelled)?;

        if consumed_frames < input_frames {
            if progress.input_frames == 0 && progress.output_frames == 0 {
                return Err(PlaybackError::PcmConverterStalled {
                    remaining_frames: input_frames - consumed_frames,
                });
            }
            continue;
        }
        if progress.output_frames * 2 < output.len() {
            return Ok(());
        }
    }
}

fn write_converted(
    progress: ConversionProgress,
    samples: &[i32],
    transmit: &mut TransmitChannel,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), PlaybackError> {
    let samples = &samples[..progress.output_frames * 2];
    let bytes = i32_slice_as_bytes(samples);
    let mut written = 0;
    while written < bytes.len() {
        ensure_not_cancelled(cancelled)?;
        let count = transmit.write(&bytes[written..], TRANSMIT_TIMEOUT_MS)?;
        if count == 0 {
            return Err(PlaybackError::I2sWriteStalled {
                remaining: bytes.len() - written,
            });
        }
        written += count;
    }
    Ok(())
}

fn i32_slice_as_bytes(samples: &[i32]) -> &[u8] {
    let length = core::mem::size_of_val(samples);
    // SAFETY: u8 has alignment one and the byte slice covers exactly the live,
    // initialized i32 slice for the duration of the shared borrow.
    unsafe { slice::from_raw_parts(samples.as_ptr().cast(), length) }
}
