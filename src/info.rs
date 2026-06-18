use crate::chunk::{Form, parse_chunks, read_u16_be};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub width: u16,
    pub height: u16,
    pub version: u8,
    pub dpi: u16,
    pub gamma: f32,
    pub rotation: u8,
}

/// Reads the first `INFO` chunk from a `FORM:DJVU` page, if present.
///
/// # Errors
///
/// Returns an error if the page form's child chunk stream is malformed.
pub fn read_page_info(bytes: &[u8], form: &Form<'_>) -> Result<Option<PageInfo>> {
    if form.kind != "DJVU" {
        return Ok(None);
    }

    let children = parse_chunks(bytes, form.children_start, form.chunk.data_end)?;
    let Some(info_chunk) = children.first().filter(|chunk| chunk.id == "INFO") else {
        return Ok(None);
    };

    if info_chunk.size < 10 {
        return Ok(None);
    }

    let start = info_chunk.data_start;
    Ok(Some(PageInfo {
        width: read_u16_be(bytes, start)?,
        height: read_u16_be(bytes, start + 2)?,
        version: bytes[start + 4],
        dpi: read_u16_be(bytes, start + 5)?,
        gamma: f32::from(bytes[start + 8]) / 10.0,
        rotation: bytes[start + 9],
    }))
}
