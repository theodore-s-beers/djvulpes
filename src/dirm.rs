use crate::chunk::{Chunk, read_u16_be, read_u32_be, require_range};
use crate::error::{ParseError, Result};

#[derive(Debug, Clone)]
pub struct Dirm {
    pub flags: u8,
    pub entry_count: u16,
    pub offsets: Vec<u32>,
    pub compressed_tail_len: usize,
}

/// Parses the currently understood, uncompressed prefix of a `DIRM` chunk.
///
/// # Errors
///
/// Returns an error if the chunk is too small for the declared directory offset
/// table.
pub fn parse_dirm(bytes: &[u8], chunk: &Chunk<'_>) -> Result<Dirm> {
    require_range(bytes, chunk.data_start, 3)?;

    let flags = bytes[chunk.data_start];
    let entry_count = read_u16_be(bytes, chunk.data_start + 1)?;
    let offsets_start = chunk.data_start + 3;
    let offsets_len = usize::from(entry_count) * 4;
    let compressed_tail_start = offsets_start
        .checked_add(offsets_len)
        .ok_or_else(|| ParseError("offset overflow".to_string()))?;

    if compressed_tail_start > chunk.data_end {
        return Err(ParseError(format!(
            "DIRM declares {entry_count} entries, but the chunk is too small for their offsets"
        )));
    }

    let mut offsets = Vec::with_capacity(usize::from(entry_count));
    for index in 0..usize::from(entry_count) {
        offsets.push(read_u32_be(bytes, offsets_start + index * 4)?);
    }

    Ok(Dirm {
        flags,
        entry_count,
        offsets,
        compressed_tail_len: chunk.data_end - compressed_tail_start,
    })
}
