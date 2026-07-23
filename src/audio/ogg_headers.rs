//! Pure validation of the codec headers carried by Ogg audio streams.

const OPUS_HEAD_BYTES: usize = 19;
const VORBIS_IDENTIFICATION_BYTES: usize = 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OggCodec {
    Vorbis,
    Opus {
        channels: u8,
        pre_skip: u16,
        output_gain_q8: i16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvalidGranule {
    pub(crate) granule: u64,
    pub(crate) decoded_frames: u64,
}

#[derive(Debug, Default)]
pub(crate) struct PcmTrimmer {
    decoded_frames: u64,
    final_granule: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PacketTooLarge {
    pub(crate) bytes: usize,
    pub(crate) limit: usize,
}

#[derive(Debug, Default)]
pub(crate) struct PacketSizeTracker {
    continued_bytes: usize,
}

impl PacketSizeTracker {
    pub(crate) fn observe_page(
        &mut self,
        starts_with_continued_packet: bool,
        lacing_values: &[u8],
        limit: usize,
    ) -> Result<(), PacketTooLarge> {
        if !starts_with_continued_packet {
            self.continued_bytes = 0;
        }
        for &lacing in lacing_values {
            self.continued_bytes = self
                .continued_bytes
                .checked_add(usize::from(lacing))
                .ok_or(PacketTooLarge {
                    bytes: usize::MAX,
                    limit,
                })?;
            if self.continued_bytes > limit {
                return Err(PacketTooLarge {
                    bytes: self.continued_bytes,
                    limit,
                });
            }
            if lacing < u8::MAX {
                self.continued_bytes = 0;
            }
        }
        Ok(())
    }
}

impl PcmTrimmer {
    pub(crate) fn set_final_granule(&mut self, granule: u64) -> Result<(), InvalidGranule> {
        if granule < self.decoded_frames {
            return Err(InvalidGranule {
                granule,
                decoded_frames: self.decoded_frames,
            });
        }
        self.final_granule = Some(granule);
        Ok(())
    }

    pub(crate) fn retain_decoded_frames(&mut self, frames: usize) -> usize {
        let frames_u64 = u64::try_from(frames).unwrap_or(u64::MAX);
        let decoded_before = self.decoded_frames;
        self.decoded_frames = self.decoded_frames.saturating_add(frames_u64);
        let Some(final_granule) = self.final_granule else {
            return frames;
        };
        let retained = final_granule.saturating_sub(decoded_before).min(frames_u64);
        usize::try_from(retained).unwrap_or(frames)
    }
}

pub(crate) fn identify(packet: &[u8]) -> Option<OggCodec> {
    if is_vorbis_identification(packet) {
        Some(OggCodec::Vorbis)
    } else {
        opus_header(packet)
    }
}

pub(crate) fn is_vorbis_identification(packet: &[u8]) -> bool {
    packet.len() >= VORBIS_IDENTIFICATION_BYTES && is_vorbis_header(packet, 1)
}

pub(crate) fn is_vorbis_header(packet: &[u8], packet_type: u8) -> bool {
    packet.starts_with(&[packet_type]) && packet.get(1..7) == Some(b"vorbis")
}

pub(crate) fn is_opus_tags(packet: &[u8]) -> bool {
    packet.starts_with(b"OpusTags")
}

fn opus_header(packet: &[u8]) -> Option<OggCodec> {
    if packet.len() < OPUS_HEAD_BYTES || !packet.starts_with(b"OpusHead") {
        return None;
    }
    let channels = packet
        .get(9)
        .copied()
        .filter(|channels| matches!(channels, 1 | 2))?;
    if packet.get(18).copied()? != 0 {
        return None;
    }
    let pre_skip = u16::from_le_bytes([*packet.get(10)?, *packet.get(11)?]);
    let output_gain_q8 = i16::from_le_bytes([*packet.get(16)?, *packet.get(17)?]);
    Some(OggCodec::Opus {
        channels,
        pre_skip,
        output_gain_q8,
    })
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the firmware binary disables Cargo's test harness"
)]
mod tests {
    #[test]
    fn identifies_complete_vorbis_information() {
        let mut header = [0_u8; 30];
        header[..7].copy_from_slice(b"\x01vorbis");

        assert_eq!(super::identify(&header), Some(super::OggCodec::Vorbis));
        assert!(super::is_vorbis_header(b"\x03vorbiscomment", 3));
        assert!(super::is_vorbis_header(b"\x05vorbissetup", 5));
    }

    #[test]
    fn rejects_truncated_vorbis_information() {
        assert_eq!(super::identify(b"\x01vorbis"), None);
    }

    #[test]
    fn identifies_supported_opus_channel_counts() {
        for channels in [1, 2] {
            let mut header = [0_u8; 19];
            header[..8].copy_from_slice(b"OpusHead");
            header[9] = channels;
            header[10..12].copy_from_slice(&312_u16.to_le_bytes());
            header[16..18].copy_from_slice(&(-128_i16).to_le_bytes());
            assert_eq!(
                super::identify(&header),
                Some(super::OggCodec::Opus {
                    channels,
                    pre_skip: 312,
                    output_gain_q8: -128,
                })
            );
        }
    }

    #[test]
    fn rejects_invalid_opus_identification() {
        let mut truncated = [0_u8; 18];
        truncated[..8].copy_from_slice(b"OpusHead");
        truncated[9] = 1;
        assert_eq!(super::identify(&truncated), None);

        let mut too_many_channels = [0_u8; 19];
        too_many_channels[..8].copy_from_slice(b"OpusHead");
        too_many_channels[9] = 3;
        assert_eq!(super::identify(&too_many_channels), None);

        let mut mapped = [0_u8; 19];
        mapped[..8].copy_from_slice(b"OpusHead");
        mapped[9] = 2;
        mapped[18] = 1;
        assert_eq!(super::identify(&mapped), None);
    }

    #[test]
    fn recognizes_opus_tags() {
        assert!(super::is_opus_tags(b"OpusTags"));
        assert!(super::is_opus_tags(b"OpusTagsmetadata"));
        assert!(!super::is_opus_tags(b"OpusTag"));
    }

    #[test]
    fn trims_only_frames_beyond_the_final_granule() {
        let mut trimmer = super::PcmTrimmer::default();
        assert_eq!(trimmer.retain_decoded_frames(960), 960);
        assert_eq!(trimmer.set_final_granule(1_500), Ok(()));
        assert_eq!(trimmer.retain_decoded_frames(960), 540);
        assert_eq!(trimmer.retain_decoded_frames(960), 0);
    }

    #[test]
    fn rejects_a_final_granule_before_emitted_frames() {
        let mut trimmer = super::PcmTrimmer::default();
        assert_eq!(trimmer.retain_decoded_frames(960), 960);
        assert_eq!(
            trimmer.set_final_granule(959),
            Err(super::InvalidGranule {
                granule: 959,
                decoded_frames: 960,
            })
        );
    }

    #[test]
    fn bounds_packets_before_reading_their_page_body() {
        let mut tracker = super::PacketSizeTracker::default();
        assert_eq!(tracker.observe_page(false, &[255, 255], 1_000), Ok(()));
        assert_eq!(tracker.observe_page(true, &[255, 235], 1_000), Ok(()));
        assert_eq!(tracker.observe_page(false, &[255, 255], 1_000), Ok(()));
        assert_eq!(
            tracker.observe_page(true, &[255, 236], 1_000),
            Err(super::PacketTooLarge {
                bytes: 1_001,
                limit: 1_000,
            })
        );
    }

    #[test]
    fn resets_packet_size_at_each_complete_lacing_value() {
        let mut tracker = super::PacketSizeTracker::default();
        assert_eq!(
            tracker.observe_page(false, &[255, 100, 255, 100], 400),
            Ok(())
        );
    }
}
