use super::{Sha256Digest, SrError};

pub(super) const PACK_HEADER_LENGTH: usize = 784;

const MAX_MODEL_COUNT: usize = 5;
const MAX_FILES_PER_MODEL: usize = 4;
const MAX_PACKED_FILE_COUNT: usize = 9;
const MAX_PACKED_RANGE_COUNT: usize = MAX_MODEL_COUNT * MAX_FILES_PER_MODEL;
const PACK_PARTITION_LENGTH: u32 = 0x0060_0000;

#[derive(Clone, Copy)]
struct ExpectedFile {
    name: &'static str,
    length: u32,
    sha256: Sha256Digest,
}

#[derive(Clone, Copy)]
struct ExpectedModel {
    name: &'static str,
    files: &'static [ExpectedFile],
}

#[derive(Clone, Copy)]
pub(super) struct PackedFile {
    model_name: &'static str,
    expected: ExpectedFile,
    offset: u32,
}

impl PackedFile {
    pub(super) const fn model_name(self) -> &'static str {
        self.model_name
    }

    pub(super) const fn file_name(self) -> &'static str {
        self.expected.name
    }

    pub(super) const fn offset(self) -> u32 {
        self.offset
    }

    pub(super) const fn length(self) -> u32 {
        self.expected.length
    }

    pub(super) const fn expected_sha256(self) -> Sha256Digest {
        self.expected.sha256
    }
}

#[derive(Clone, Copy)]
pub(super) struct ValidatedPack {
    files: [Option<PackedFile>; MAX_PACKED_FILE_COUNT],
    file_count: usize,
    model_count: usize,
}

impl ValidatedPack {
    pub(super) fn files(self) -> impl Iterator<Item = PackedFile> {
        self.files.into_iter().take(self.file_count).flatten()
    }

    pub(super) const fn model_count(self) -> usize {
        self.model_count
    }
}

const WAKE_ALEXA_FILES: [ExpectedFile; 3] = [
    ExpectedFile {
        name: "wn9_data",
        length: 289_638,
        sha256: Sha256Digest::from_hex(
            b"232af46bf553dd832733bb12d0cd4b4027c5b134693870ca6d32a61718c3c7ac",
        ),
    },
    ExpectedFile {
        name: "_MODEL_INFO_",
        length: 35,
        sha256: Sha256Digest::from_hex(
            b"a93580fe133549f8bb3806317a289d8802721ccfbde87431eff13120e3108a24",
        ),
    },
    ExpectedFile {
        name: "wn9_index",
        length: 1_200,
        sha256: Sha256Digest::from_hex(
            b"c5557ca5d1ec6943540a6abbb3ea235a693f4637369f92e873ff566b73a61cfd",
        ),
    },
];

const WAKE_HI_ESP_FILES: [ExpectedFile; 3] = [
    ExpectedFile {
        name: "wn9_data",
        length: 289_796,
        sha256: Sha256Digest::from_hex(
            b"2e9c1f0e7d6ecd8632baef7471896897478e49e49c1c54dcece635f54f456879",
        ),
    },
    ExpectedFile {
        name: "_MODEL_INFO_",
        length: 34,
        sha256: Sha256Digest::from_hex(
            b"5fa834ea17d00c410bc407f0033b073d93944ad96586e0e437b71d5eb656aa59",
        ),
    },
    ExpectedFile {
        name: "wn9_index",
        length: 1_152,
        sha256: Sha256Digest::from_hex(
            b"da36d558d0a378c0a7bdd8348a721b5898c2aa9aef27951f613cc71f7cb7c5cd",
        ),
    },
];

const WAKE_HI_LEXIN_FILES: [ExpectedFile; 3] = [
    ExpectedFile {
        name: "wn9_data",
        length: 289_796,
        sha256: Sha256Digest::from_hex(
            b"901fe90bcff4af61402a52e62d11993a0beac7659ccad355b4fbaa7b23594611",
        ),
    },
    ExpectedFile {
        name: "_MODEL_INFO_",
        length: 41,
        sha256: Sha256Digest::from_hex(
            b"ac7d629244030b22d48c87324300efe4c66b27df566d82212e46c0f0061e60d7",
        ),
    },
    ExpectedFile {
        name: "wn9_index",
        length: 1_152,
        sha256: Sha256Digest::from_hex(
            b"da36d558d0a378c0a7bdd8348a721b5898c2aa9aef27951f613cc71f7cb7c5cd",
        ),
    },
];

#[cfg(test)]
const MULTINET_FILES: [ExpectedFile; 4] = [
    ExpectedFile {
        name: "_MODEL_INFO_",
        length: 159,
        sha256: Sha256Digest::from_hex(
            b"bc155320b2b9f093c655eea2dbbe75157284f871ecb3fec10f523d1dbac5ef66",
        ),
    },
    ExpectedFile {
        name: "mn6_index",
        length: 4_032,
        sha256: Sha256Digest::from_hex(
            b"6fc49932b9cecda2d00a26241d485b94ef6d8ba864d056fcaf3b6591ec398dbe",
        ),
    },
    ExpectedFile {
        name: "vocab",
        length: 7_314,
        sha256: Sha256Digest::from_hex(
            b"93ada28f3e3fccb11b4da9b3b6265101d41b9b0faf4e7fc081a8d3c6d7ae3ab6",
        ),
    },
    ExpectedFile {
        name: "mn6_data",
        length: 3_766_762,
        sha256: Sha256Digest::from_hex(
            b"c277f32170f54d4578f6d50096cf5c3c2a288cea3585b43c99ec3d3ea0caf693",
        ),
    },
];

#[cfg(test)]
const FST_FILES: [ExpectedFile; 2] = [
    ExpectedFile {
        name: "commands_en.txt",
        length: 2_350,
        sha256: Sha256Digest::from_hex(
            b"517b7df8f3471105ae638cd910f4d1c85f9ba7eab9e960aad2c6ca82713ad47e",
        ),
    },
    ExpectedFile {
        name: "commands_cn.txt",
        length: 7_298,
        sha256: Sha256Digest::from_hex(
            b"a516f97ec1a826c7cd8396156857169aa3b73df122596f7854d90b23a15f2252",
        ),
    },
];

const WAKE_MODELS: [ExpectedModel; 3] = [
    ExpectedModel {
        name: "wn9_alexa",
        files: &WAKE_ALEXA_FILES,
    },
    ExpectedModel {
        name: "wn9_hiesp",
        files: &WAKE_HI_ESP_FILES,
    },
    ExpectedModel {
        name: "wn9_hilexin",
        files: &WAKE_HI_LEXIN_FILES,
    },
];

#[cfg(test)]
const LEGACY_MODELS: [ExpectedModel; 5] = [
    WAKE_MODELS[0],
    ExpectedModel {
        name: "mn6_en",
        files: &MULTINET_FILES,
    },
    WAKE_MODELS[1],
    WAKE_MODELS[2],
    ExpectedModel {
        name: "fst",
        files: &FST_FILES,
    },
];

pub(super) fn validate_pack(header: &[u8; PACK_HEADER_LENGTH]) -> Result<ValidatedPack, SrError> {
    if header.iter().all(|byte| *byte == 0xff) {
        return Err(SrError::ErasedPack);
    }

    let model_count = usize::try_from(read_u32(header, 0)?)
        .map_err(|_| SrError::InvalidPack("model count does not fit usize".to_owned()))?;
    if !(WAKE_MODELS.len()..=MAX_MODEL_COUNT).contains(&model_count) {
        return Err(SrError::InvalidPack(format!(
            "expected 3 to {MAX_MODEL_COUNT} packed models, got {model_count}"
        )));
    }

    let mut packed_files = [None; MAX_PACKED_FILE_COUNT];
    let mut packed_ranges = [None; MAX_PACKED_RANGE_COUNT];
    let mut seen_wake_models = [false; WAKE_MODELS.len()];
    let mut cursor = 4;
    let mut packed_file_index = 0;
    let mut packed_range_index = 0;

    for _ in 0..model_count {
        parse_model_entry(
            header,
            &mut cursor,
            &mut packed_files,
            &mut packed_file_index,
            &mut packed_ranges,
            &mut packed_range_index,
            &mut seen_wake_models,
        )?;
    }

    if seen_wake_models.iter().any(|seen| !seen) {
        return Err(SrError::InvalidPack(
            "one or more required WakeNet models are missing".to_owned(),
        ));
    }
    if packed_file_index != MAX_PACKED_FILE_COUNT {
        return Err(SrError::InternalInvariant(
            "validated WakeNet file count differs from the fixture",
        ));
    }

    validate_ranges(cursor, &mut packed_ranges, packed_range_index)?;

    Ok(ValidatedPack {
        files: packed_files,
        file_count: packed_file_index,
        model_count,
    })
}

fn parse_model_entry(
    header: &[u8; PACK_HEADER_LENGTH],
    cursor: &mut usize,
    packed_files: &mut [Option<PackedFile>; MAX_PACKED_FILE_COUNT],
    packed_file_index: &mut usize,
    packed_ranges: &mut [Option<(u32, u32)>; MAX_PACKED_RANGE_COUNT],
    packed_range_index: &mut usize,
    seen_wake_models: &mut [bool; WAKE_MODELS.len()],
) -> Result<(), SrError> {
    let model_name = read_name(header, *cursor)?;
    *cursor += 32;
    let wake_model_index = WAKE_MODELS
        .iter()
        .position(|model| model.name == model_name);
    if let Some(index) = wake_model_index {
        if seen_wake_models[index] {
            return Err(SrError::InvalidPack(format!(
                "duplicate packed WakeNet model {model_name}"
            )));
        }
        seen_wake_models[index] = true;
    }

    let file_count = usize::try_from(read_u32(header, *cursor)?)
        .map_err(|_| SrError::InvalidPack("model file count does not fit usize".to_owned()))?;
    *cursor += 4;
    if file_count > MAX_FILES_PER_MODEL {
        return Err(SrError::InvalidPack(format!(
            "model {model_name} contains {file_count} files; maximum supported is {MAX_FILES_PER_MODEL}"
        )));
    }

    let mut seen_files = [false; MAX_FILES_PER_MODEL];
    for _ in 0..file_count {
        let file_name = read_name(header, *cursor)?;
        let offset = read_u32(header, *cursor + 32)?;
        let length = read_u32(header, *cursor + 36)?;
        *cursor += 40;
        let end = offset.checked_add(length).ok_or_else(|| {
            SrError::InvalidPack(format!(
                "model {model_name} file {file_name} range overflows u32"
            ))
        })?;
        if end > PACK_PARTITION_LENGTH {
            return Err(SrError::InvalidPack(format!(
                "model {model_name} file {file_name} has out-of-range span {offset}..{end}"
            )));
        }
        packed_ranges[*packed_range_index] = Some((offset, end));
        *packed_range_index += 1;

        if let Some(model_index) = wake_model_index {
            let expected_model = WAKE_MODELS[model_index];
            let file_index = expected_model
                .files
                .iter()
                .position(|file| file.name == file_name)
                .ok_or_else(|| {
                    SrError::InvalidPack(format!(
                        "unexpected WakeNet model file {model_name}/{file_name}"
                    ))
                })?;
            if seen_files[file_index] {
                return Err(SrError::InvalidPack(format!(
                    "model {model_name} contains duplicate file {file_name}"
                )));
            }
            seen_files[file_index] = true;

            let expected = expected_model.files[file_index];
            if length != expected.length {
                return Err(SrError::InvalidPack(format!(
                    "model {model_name} file {file_name} has length {length}; expected {}",
                    expected.length
                )));
            }
            packed_files[*packed_file_index] = Some(PackedFile {
                model_name: expected_model.name,
                expected,
                offset,
            });
            *packed_file_index += 1;
        }
    }
    if wake_model_index.is_some()
        && seen_files[..WAKE_ALEXA_FILES.len()]
            .iter()
            .any(|seen| !seen)
    {
        return Err(SrError::InvalidPack(format!(
            "model {model_name} is missing one or more required files"
        )));
    }
    Ok(())
}

fn validate_ranges(
    table_end: usize,
    packed_ranges: &mut [Option<(u32, u32)>; MAX_PACKED_RANGE_COUNT],
    packed_range_count: usize,
) -> Result<(), SrError> {
    packed_ranges[..packed_range_count]
        .sort_unstable_by_key(|range| range.map_or(u32::MAX, |(offset, _end)| offset));
    let table_end = u32::try_from(table_end)
        .map_err(|_| SrError::InternalInvariant("model table end does not fit u32"))?;
    let mut previous_end = table_end;
    for (offset, end) in packed_ranges[..packed_range_count]
        .iter()
        .flatten()
        .copied()
    {
        if offset < previous_end {
            return Err(SrError::InvalidPack(format!(
                "packed file range {offset}..{end} overlaps the model table or preceding data ending at {previous_end}"
            )));
        }
        previous_end = end;
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SrError> {
    let encoded = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| SrError::InvalidPack(format!("truncated u32 at offset {offset}")))?;
    Ok(u32::from_le_bytes(encoded.try_into().map_err(|_| {
        SrError::InvalidPack(format!("invalid u32 at offset {offset}"))
    })?))
}

fn read_name(bytes: &[u8], offset: usize) -> Result<&str, SrError> {
    let field = bytes
        .get(offset..offset + 32)
        .ok_or_else(|| SrError::InvalidPack(format!("truncated name field at offset {offset}")))?;
    let nul = field.iter().position(|byte| *byte == 0).ok_or_else(|| {
        SrError::InvalidPack(format!(
            "name field at offset {offset} has no NUL terminator"
        ))
    })?;
    if field[nul + 1..].iter().any(|byte| *byte != 0) {
        return Err(SrError::InvalidPack(format!(
            "name field at offset {offset} has nonzero padding"
        )));
    }
    core::str::from_utf8(&field[..nul]).map_err(|error| {
        SrError::InvalidPack(format!(
            "name field at offset {offset} is not UTF-8: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{ExpectedModel, PACK_HEADER_LENGTH};

    #[test]
    fn accepts_current_pack_in_arbitrary_table_order() {
        let header = make_header(&super::WAKE_MODELS, None);
        let pack = super::validate_pack(&header).expect("current model pack should validate");

        assert_eq!(pack.model_count(), 3);
        assert_eq!(pack.files().count(), 9);
    }

    #[test]
    fn accepts_deployed_legacy_pack_in_arbitrary_table_order() {
        let header = make_header(&super::LEGACY_MODELS, None);
        let pack = super::validate_pack(&header).expect("legacy model pack should validate");

        assert_eq!(pack.model_count(), 5);
        assert_eq!(pack.files().count(), 9);
    }

    #[test]
    fn ignores_unrelated_legacy_file_lengths() {
        let header = make_header(
            &super::LEGACY_MODELS,
            Some(("fst", "commands_en.txt", 6_279)),
        );
        let pack =
            super::validate_pack(&header).expect("unrelated legacy file length should be ignored");

        assert_eq!(pack.model_count(), 5);
        assert_eq!(pack.files().count(), 9);
    }

    #[test]
    fn still_rejects_changed_wakenet_file_lengths() {
        let header = make_header(
            &super::LEGACY_MODELS,
            Some(("wn9_alexa", "wn9_data", 289_639)),
        );

        assert!(super::validate_pack(&header).is_err());
    }

    #[test]
    fn rejects_an_unreviewed_model_count() {
        let mut header = [0_u8; PACK_HEADER_LENGTH];
        header[..4].copy_from_slice(&4_u32.to_le_bytes());

        assert!(super::validate_pack(&header).is_err());
    }

    fn make_header(
        models: &[ExpectedModel],
        length_override: Option<(&str, &str, u32)>,
    ) -> [u8; PACK_HEADER_LENGTH] {
        let mut header = [0_u8; PACK_HEADER_LENGTH];
        let model_count = u32::try_from(models.len()).expect("model count should fit u32");
        header[..4].copy_from_slice(&model_count.to_le_bytes());
        let mut cursor = 4;
        let table_length = models.iter().fold(4_usize, |length, model| {
            length + 36 + model.files.len() * 40
        });
        let mut data_offset = u32::try_from(table_length).expect("table length should fit u32");

        for model in models.iter().rev() {
            write_name(&mut header, cursor, model.name);
            cursor += 32;
            let file_count = u32::try_from(model.files.len()).expect("file count should fit u32");
            write_u32(&mut header, cursor, file_count);
            cursor += 4;

            for file in model.files.iter().rev() {
                write_name(&mut header, cursor, file.name);
                write_u32(&mut header, cursor + 32, data_offset);
                let length =
                    length_override.map_or(file.length, |(model_name, file_name, length)| {
                        if model.name == model_name && file.name == file_name {
                            length
                        } else {
                            file.length
                        }
                    });
                write_u32(&mut header, cursor + 36, length);
                cursor += 40;
                data_offset += length;
            }
        }

        assert_eq!(cursor, table_length);
        header
    }

    fn write_name(header: &mut [u8], offset: usize, name: &str) {
        let end = offset + name.len();
        header[offset..end].copy_from_slice(name.as_bytes());
    }

    fn write_u32(header: &mut [u8], offset: usize, value: u32) {
        header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
