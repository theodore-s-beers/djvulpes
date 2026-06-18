use crate::error::{ParseError, ParseResult};

pub const DJVU_MAGIC: &[u8; 4] = b"AT&T";

#[derive(Debug, Clone)]
pub struct Chunk<'a> {
    pub id: &'a str,
    pub size: u32,
    pub data_start: usize,
    pub data_end: usize,
    pub next_start: usize,
}

#[derive(Debug, Clone)]
pub struct Form<'a> {
    pub chunk: Chunk<'a>,
    pub kind: &'a str,
    pub children_start: usize,
}

/// Parses the top-level `DjVu` document `FORM`.
///
/// # Errors
///
/// Returns an error if the byte slice is truncated, does not start with the
/// `DjVu` magic bytes, or has an unsupported root form kind.
pub fn parse_document_root(bytes: &[u8]) -> ParseResult<Form<'_>> {
    require_range(bytes, 0, 4)?;
    if &bytes[0..4] != DJVU_MAGIC {
        return Err(ParseError("missing DjVu magic bytes `AT&T`".to_string()));
    }

    let root = parse_form_at(bytes, 4)?;
    match root.kind {
        "DJVU" | "DJVM" => Ok(root),
        other => Err(ParseError(format!(
            "unexpected DjVu root FORM kind `{other}`"
        ))),
    }
}

/// Parses a `FORM` chunk at an absolute byte offset.
///
/// # Errors
///
/// Returns an error if the chunk is truncated, is not a `FORM`, or is too small
/// to contain its four-byte form kind.
pub fn parse_form_at(bytes: &[u8], start: usize) -> ParseResult<Form<'_>> {
    let chunk = parse_chunk_at(bytes, start)?;
    if chunk.id != "FORM" {
        return Err(ParseError(format!(
            "expected FORM at offset {start}, found {}",
            chunk.id
        )));
    }

    if chunk.size < 4 {
        return Err(ParseError(format!(
            "FORM at offset {start} is too small to contain a form kind"
        )));
    }

    let kind = ascii_tag(bytes, chunk.data_start)?;
    Ok(Form {
        children_start: chunk.data_start + 4,
        chunk,
        kind,
    })
}

/// Parses sibling chunks in the half-open byte range `start..end`.
///
/// # Errors
///
/// Returns an error if any child chunk is malformed or extends beyond the
/// supplied parent range.
pub fn parse_chunks(bytes: &[u8], start: usize, end: usize) -> ParseResult<Vec<Chunk<'_>>> {
    let mut chunks = Vec::new();
    let mut cursor = start;

    while cursor < end {
        let chunk = parse_chunk_at(bytes, cursor)?;
        if chunk.data_end > end {
            return Err(ParseError(format!(
                "chunk {} at offset {cursor} extends past parent end {end}",
                chunk.id
            )));
        }

        let next_start = chunk.next_start;
        chunks.push(chunk);
        cursor = next_start;
    }

    Ok(chunks)
}

/// Parses one chunk at an absolute byte offset.
///
/// # Errors
///
/// Returns an error if the chunk header, payload, or required padding byte is
/// outside the available byte slice.
pub fn parse_chunk_at(bytes: &[u8], start: usize) -> ParseResult<Chunk<'_>> {
    require_range(bytes, start, 8)?;

    let id = ascii_tag(bytes, start)?;
    let size = read_u32_be(bytes, start + 4)?;
    let data_start = start + 8;
    let data_end = checked_add(data_start, size as usize)?;
    require_range(bytes, data_start, size as usize)?;

    let next_start = checked_add(data_end, (size & 1) as usize)?;
    if next_start > bytes.len() {
        return Err(ParseError(format!(
            "chunk {id} at offset {start} has invalid padding"
        )));
    }

    Ok(Chunk {
        id,
        size,
        data_start,
        data_end,
        next_start,
    })
}

fn ascii_tag(bytes: &[u8], start: usize) -> ParseResult<&str> {
    require_range(bytes, start, 4)?;
    std::str::from_utf8(&bytes[start..start + 4])
        .map_err(|_| ParseError(format!("non-ASCII chunk tag at offset {start}")))
}

pub fn read_u16_be(bytes: &[u8], start: usize) -> ParseResult<u16> {
    require_range(bytes, start, 2)?;
    Ok(u16::from_be_bytes([bytes[start], bytes[start + 1]]))
}

pub fn read_u16_le(bytes: &[u8], start: usize) -> ParseResult<u16> {
    require_range(bytes, start, 2)?;
    Ok(u16::from_le_bytes([bytes[start], bytes[start + 1]]))
}

pub fn read_u32_be(bytes: &[u8], start: usize) -> ParseResult<u32> {
    require_range(bytes, start, 4)?;
    Ok(u32::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ]))
}

fn checked_add(lhs: usize, rhs: usize) -> ParseResult<usize> {
    lhs.checked_add(rhs)
        .ok_or_else(|| ParseError("offset overflow".to_string()))
}

pub fn require_range(bytes: &[u8], start: usize, len: usize) -> ParseResult<()> {
    let end = checked_add(start, len)?;
    if end > bytes.len() {
        return Err(ParseError(format!(
            "need bytes [{start}..{end}), but file only has {} bytes",
            bytes.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_document_root_accepts_minimal_multipage_document() {
        let bytes = b"AT&TFORM\0\0\0\x04DJVM";

        let root = parse_document_root(bytes).expect("minimal root should parse");

        assert_eq!(root.kind, "DJVM");
        assert_eq!(root.children_start, 16);
        assert_eq!(root.chunk.data_end, 16);
    }

    #[test]
    fn parse_form_rejects_size_too_small_for_form_kind() {
        let bytes = b"FORM\0\0\0\x03ABC!";

        let error = parse_form_at(bytes, 0).expect_err("undersized FORM should fail");

        assert!(error.message().contains("too small"));
    }

    #[test]
    fn parse_chunks_accepts_final_child_padding_after_parent_data() {
        let bytes = b"ABCD\0\0\0\x01Z\0";

        let chunks = parse_chunks(bytes, 0, 9).expect("final padding may sit after parent data");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data_end, 9);
        assert_eq!(chunks[0].next_start, 10);
    }

    #[test]
    fn parse_chunks_accepts_odd_child_with_padding_inside_parent() {
        let bytes = b"ABCD\0\0\0\x01Z\0";

        let chunks = parse_chunks(bytes, 0, 10).expect("padded odd chunk should parse");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, "ABCD");
        assert_eq!(chunks[0].data_start, 8);
        assert_eq!(chunks[0].data_end, 9);
        assert_eq!(chunks[0].next_start, 10);
    }
}
