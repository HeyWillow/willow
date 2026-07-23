use super::{ModelFixture, SrError};

pub(super) const PACK_HEADER_LENGTH: usize = 160;
const PACK_HEADER_LENGTH_U32: u32 = 160;

#[derive(Clone, Copy)]
struct FileRecord {
    length: u32,
}

pub(super) fn validate_pack(
    header: &[u8; PACK_HEADER_LENGTH],
    fixture: &ModelFixture,
) -> Result<(), SrError> {
    if read_u32(header, 0)? != 1 {
        return Err(SrError::InvalidPack("expected exactly one packed model"));
    }

    let model_name = read_name(header, 4)?;
    if model_name.as_bytes() != fixture.model_name().to_bytes() {
        return Err(SrError::InvalidPack(
            "packed model name is not wn9_heywillow_tts",
        ));
    }

    if read_u32(header, 36)? != 3 {
        return Err(SrError::InvalidPack(
            "Hey Willow must contain exactly three files",
        ));
    }

    let mut ranges = [(0_u32, 0_u32); 3];
    let mut data = None;
    let mut info = None;
    let mut index = None;

    for (entry, table_offset) in (0..3).zip([40_usize, 80, 120]) {
        let name = read_name(header, table_offset)?;
        let offset = read_u32(header, table_offset + 32)?;
        let length = read_u32(header, table_offset + 36)?;
        let end = offset
            .checked_add(length)
            .ok_or(SrError::InvalidPack("file range overflows u32"))?;

        if offset < PACK_HEADER_LENGTH_U32 || end > fixture.image_len() {
            return Err(SrError::InvalidPack(
                "file range falls outside the reviewed image",
            ));
        }

        let record = FileRecord { length };
        match name.as_bytes() {
            b"wn9_data" if data.replace(record).is_none() => {}
            b"_MODEL_INFO_" if info.replace(record).is_none() => {}
            b"wn9_index" if index.replace(record).is_none() => {}
            _ => return Err(SrError::InvalidPack("unexpected or duplicate model file")),
        }
        ranges[entry] = (offset, end);
    }

    if data.map(|record| record.length) != Some(289_638)
        || info.map(|record| record.length) != Some(42)
        || index.map(|record| record.length) != Some(1_200)
    {
        return Err(SrError::InvalidPack("reviewed model file length changed"));
    }

    ranges.sort_unstable_by_key(|range| range.0);
    let mut expected_offset = PACK_HEADER_LENGTH_U32;
    for (offset, end) in ranges {
        if offset != expected_offset {
            return Err(SrError::InvalidPack(
                "model file ranges overlap or contain gaps",
            ));
        }
        expected_offset = end;
    }
    if expected_offset != fixture.image_len() {
        return Err(SrError::InvalidPack(
            "packed files do not consume the reviewed image length",
        ));
    }

    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SrError> {
    let encoded = bytes
        .get(offset..offset + 4)
        .ok_or(SrError::InvalidPack("truncated u32"))?;
    Ok(u32::from_le_bytes(
        encoded
            .try_into()
            .map_err(|_| SrError::InvalidPack("invalid u32"))?,
    ))
}

fn read_name(bytes: &[u8], offset: usize) -> Result<&str, SrError> {
    let field = bytes
        .get(offset..offset + 32)
        .ok_or(SrError::InvalidPack("truncated name field"))?;
    let nul = field
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(SrError::InvalidPack("name field has no NUL terminator"))?;
    if field[nul + 1..].iter().any(|byte| *byte != 0) {
        return Err(SrError::InvalidPack("name field has nonzero padding"));
    }
    core::str::from_utf8(&field[..nul]).map_err(|_| SrError::InvalidPack("name field is not UTF-8"))
}
