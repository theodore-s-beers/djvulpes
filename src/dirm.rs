use crate::chunk::{Chunk, read_u16_be, read_u32_be, require_range};
use crate::error::{ParseError, Result};
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct Dirm {
    pub flags: u8,
    pub entry_count: u16,
    pub offsets: Vec<u32>,
    pub compressed_tail_start: usize,
    pub compressed_tail_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirmTailEntry<'a> {
    pub offset: u32,
    pub size: u32,
    pub flags: u8,
    pub name: &'a str,
}

impl Dirm {
    #[must_use]
    pub const fn compressed_tail_end(&self) -> usize {
        self.compressed_tail_start + self.compressed_tail_len
    }

    #[must_use]
    pub const fn compressed_tail_range(&self) -> Range<usize> {
        self.compressed_tail_start..self.compressed_tail_end()
    }

    /// Returns the compressed directory tail bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory range is outside `bytes`.
    pub fn compressed_tail<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8]> {
        require_range(bytes, self.compressed_tail_start, self.compressed_tail_len)?;
        Ok(&bytes[self.compressed_tail_range()])
    }
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
        compressed_tail_start,
        compressed_tail_len: chunk.data_end - compressed_tail_start,
    })
}

/// Parses a decompressed `DIRM` tail into directory entries.
///
/// `DjVu` bundled-document directory stores the offsets in the uncompressed
/// `DIRM` prefix. The compressed tail begins with one entry per offset:
/// first a table of three-byte big-endian sizes, then a table of one-byte
/// flags. Null-terminated UTF-8 names follow those fixed-width tables.
///
/// # Errors
///
/// Returns an error if `raw` is too short, if it does not contain enough
/// null-terminated names, or if a name is not valid UTF-8.
pub fn parse_dirm_tail<'a>(dirm: &Dirm, raw: &'a [u8]) -> Result<Vec<DirmTailEntry<'a>>> {
    let entry_count = usize::from(dirm.entry_count);

    if dirm.offsets.len() != entry_count {
        return Err(ParseError(format!(
            "DIRM declares {entry_count} entries, but has {} offsets",
            dirm.offsets.len()
        )));
    }

    let sizes_len = entry_count
        .checked_mul(3)
        .ok_or_else(|| ParseError("DIRM tail size table length overflow".to_string()))?;
    let table_len = sizes_len
        .checked_add(entry_count)
        .ok_or_else(|| ParseError("DIRM tail table length overflow".to_string()))?;
    require_range(raw, 0, table_len)?;

    let mut entries = Vec::with_capacity(entry_count);
    let mut names_start = table_len;

    for index in 0..entry_count {
        let size_start = index * 3;
        let size = u32::from(raw[size_start]) << 16
            | u32::from(raw[size_start + 1]) << 8
            | u32::from(raw[size_start + 2]);
        let flags = raw[sizes_len + index];

        let Some(name_len) = raw[names_start..].iter().position(|byte| *byte == 0) else {
            return Err(ParseError(format!(
                "DIRM tail is missing name terminator for entry {}",
                index + 1
            )));
        };
        let name_end = names_start + name_len;
        let name = std::str::from_utf8(&raw[names_start..name_end]).map_err(|error| {
            ParseError(format!(
                "DIRM tail entry {} name is not valid UTF-8: {error}",
                index + 1
            ))
        })?;
        names_start = name_end + 1;

        entries.push(DirmTailEntry {
            offset: dirm.offsets[index],
            size,
            flags,
            name,
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{Dirm, DirmTailEntry, parse_dirm_tail};

    #[test]
    fn parses_decompressed_tail_entries() {
        let dirm = Dirm {
            flags: 0x81,
            entry_count: 2,
            offsets: vec![100, 130],
            compressed_tail_start: 27,
            compressed_tail_len: 99,
        };
        let raw = [
            0x00, 0x00, 0x1e, 0x00, 0x26, 0xed, 0x00, 0x02, b'a', b'.', b'd', b'j', b'v', b'u', 0,
            b's', b'h', b'a', b'r', b'e', b'd', b'.', b'd', b'j', b'b', b'z', 0,
        ];

        let entries = parse_dirm_tail(&dirm, &raw).expect("tail parses");

        assert_eq!(
            entries,
            vec![
                DirmTailEntry {
                    offset: 100,
                    size: 30,
                    flags: 0,
                    name: "a.djvu",
                },
                DirmTailEntry {
                    offset: 130,
                    size: 9965,
                    flags: 2,
                    name: "shared.djbz",
                },
            ]
        );
    }

    #[test]
    fn rejects_tail_with_missing_name_terminator() {
        let dirm = Dirm {
            flags: 0,
            entry_count: 1,
            offsets: vec![100],
            compressed_tail_start: 0,
            compressed_tail_len: 0,
        };
        let raw = [0x00, 0x00, 0x1e, 0x00, b'n', b'a', b'm', b'e'];

        let error = parse_dirm_tail(&dirm, &raw).expect_err("missing terminator fails");

        assert!(error.message().contains("missing name terminator"));
    }
}
