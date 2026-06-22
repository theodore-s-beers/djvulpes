use crate::chunk::{Form, parse_chunks, read_u16_be, read_u16_le};
use crate::error::ParseResult;

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub width: u16,
    pub height: u16,
    pub version: u8,
    pub dpi: u16,
    pub gamma: f32,
    pub rotation: u8,
}

impl PageInfo {
    /// Returns the raw `DjVu` orientation flag, normalized like `DjVuLibre`.
    ///
    /// `DjVuLibre` documents `{1, 6, 2, 5}` as the valid `INFO` orientation
    /// values and treats any other value as `1`, the right-side-up default.
    #[must_use]
    pub const fn normalized_rotation_flag(&self) -> u8 {
        normalized_rotation_flag(self.rotation)
    }

    #[must_use]
    pub const fn is_rightside_up(&self) -> bool {
        self.normalized_rotation_flag() == 1
    }
}

#[must_use]
const fn normalized_rotation_flag(rotation: u8) -> u8 {
    match rotation {
        1 | 6 | 2 | 5 => rotation,
        _ => 1,
    }
}

/// Reads the first `INFO` chunk from a `FORM:DJVU` page, if present.
///
/// # Errors
///
/// Returns an error if the page form's child chunk stream is malformed.
pub fn read_page_info(bytes: &[u8], form: &Form<'_>) -> ParseResult<Option<PageInfo>> {
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
        dpi: read_u16_le(bytes, start + 6)?,
        gamma: f32::from(bytes[start + 8]) / 10.0,
        rotation: bytes[start + 9],
    }))
}

#[cfg(test)]
mod tests {
    use super::{PageInfo, read_page_info};
    use crate::chunk::parse_form_at;

    fn push_chunk(bytes: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) {
        let payload_len = u32::try_from(payload.len()).expect("test payload should fit in u32");

        bytes.extend_from_slice(&id);
        bytes.extend_from_slice(&payload_len.to_be_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            bytes.push(0);
        }
    }

    fn page_form(info_payload: &[u8]) -> Vec<u8> {
        let mut children = Vec::new();
        push_chunk(&mut children, *b"INFO", info_payload);

        let mut payload = Vec::new();
        payload.extend_from_slice(b"DJVU");
        payload.extend_from_slice(&children);

        let mut bytes = Vec::new();
        push_chunk(&mut bytes, *b"FORM", &payload);
        bytes
    }

    #[test]
    fn reads_info_dpi_as_little_endian_after_reserved_byte() {
        let bytes = page_form(&[0x0d, 0x60, 0x13, 0xd2, 25, 0, 0x58, 0x02, 22, 1]);
        let form = parse_form_at(&bytes, 0).expect("form should parse");

        let info = read_page_info(&bytes, &form)
            .expect("INFO should parse")
            .expect("INFO should exist");

        assert_eq!(info.width, 3424);
        assert_eq!(info.height, 5074);
        assert_eq!(info.version, 25);
        assert_eq!(info.dpi, 600);
        assert!((info.gamma - 2.2).abs() < f32::EPSILON);
        assert_eq!(info.rotation, 1);
        assert_eq!(info.normalized_rotation_flag(), 1);
        assert!(info.is_rightside_up());
    }

    #[test]
    fn page_info_normalizes_invalid_rotation_flags_to_rightside_up() {
        let info = PageInfo {
            width: 1,
            height: 1,
            version: 25,
            dpi: 300,
            gamma: 2.2,
            rotation: 0,
        };

        assert_eq!(info.normalized_rotation_flag(), 1);
        assert!(info.is_rightside_up());
    }

    #[test]
    fn page_info_preserves_valid_rotation_flags() {
        for flag in [1, 6, 2, 5] {
            let info = PageInfo {
                width: 1,
                height: 1,
                version: 25,
                dpi: 300,
                gamma: 2.2,
                rotation: flag,
            };

            assert_eq!(info.normalized_rotation_flag(), flag);
        }
    }
}
