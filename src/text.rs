use crate::chunk::require_range;
use crate::error::{ParseError, ParseResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPayload<'a> {
    pub text: &'a str,
    pub text_len: usize,
    pub zone_data: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextZone {
    pub kind: TextZoneKind,
    pub text_start: usize,
    pub text_len: usize,
    pub x: i32,
    pub y_top: i32,
    pub width: i32,
    pub height: i32,
    pub children: Vec<Self>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextZoneKind {
    Page,
    Column,
    Region,
    Paragraph,
    Line,
    Word,
    Character,
    Unknown(u8),
}

/// Parses a decompressed `TXTa`/`TXTz` payload.
///
/// The payload begins with a three-byte big-endian text byte length, followed
/// by that many UTF-8 text bytes. The remaining bytes are text-zone/layout
/// data, which this parser preserves but does not interpret yet.
///
/// # Errors
///
/// Returns an error if the payload is truncated or if the declared text bytes
/// are not valid UTF-8.
pub fn parse_text_payload(bytes: &[u8]) -> ParseResult<TextPayload<'_>> {
    require_range(bytes, 0, 3)?;

    let text_len = usize::from(bytes[0]) << 16 | usize::from(bytes[1]) << 8 | usize::from(bytes[2]);
    let text_start = 3usize;
    let text_end = text_start
        .checked_add(text_len)
        .ok_or_else(|| ParseError("text payload length overflow".to_string()))?;
    require_range(bytes, text_start, text_len)?;

    let text = std::str::from_utf8(&bytes[text_start..text_end])
        .map_err(|error| ParseError(format!("text payload is not valid UTF-8: {error}")))?;

    Ok(TextPayload {
        text,
        text_len,
        zone_data: &bytes[text_end..],
    })
}

/// Parses the binary text-zone tree following a `TXTa`/`TXTz` text string.
///
/// # Errors
///
/// Returns an error if the zone stream is truncated or contains trailing bytes
/// after the root zone.
pub fn parse_text_zones(bytes: &[u8]) -> ParseResult<Option<TextZone>> {
    if bytes.is_empty() {
        return Ok(None);
    }

    let mut cursor = 0usize;
    let version = read_u8(bytes, &mut cursor)?;
    if version != 1 {
        return Err(ParseError(format!(
            "unsupported text zone stream version {version}"
        )));
    }

    let zone = parse_zone(bytes, &mut cursor, None, None)?;

    if cursor != bytes.len() {
        return Err(ParseError(format!(
            "text zone stream has {} trailing bytes",
            bytes.len() - cursor
        )));
    }

    Ok(Some(zone))
}

impl TextZoneKind {
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Page,
            2 => Self::Column,
            3 => Self::Region,
            4 => Self::Paragraph,
            5 => Self::Line,
            6 => Self::Word,
            7 => Self::Character,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Column => "column",
            Self::Region => "region",
            Self::Paragraph => "para",
            Self::Line => "line",
            Self::Word => "word",
            Self::Character => "char",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl TextZone {
    #[must_use]
    pub const fn text_end(&self) -> usize {
        self.text_start.saturating_add(self.text_len)
    }

    #[must_use]
    pub const fn x_min(&self) -> i32 {
        self.x
    }

    #[must_use]
    pub const fn x_max(&self) -> i32 {
        self.x + self.width
    }

    #[must_use]
    pub const fn y_min(&self, page_height: i32) -> i32 {
        page_height - self.y_top - self.height
    }

    #[must_use]
    pub const fn y_max(&self, page_height: i32) -> i32 {
        page_height - self.y_top
    }
}

#[derive(Debug, Clone, Copy)]
struct ZoneContext {
    x: i32,
    y_top: i32,
    text_start: usize,
}

fn parse_zone(
    bytes: &[u8],
    cursor: &mut usize,
    parent: Option<ZoneContext>,
    previous_sibling: Option<&TextZone>,
) -> ParseResult<TextZone> {
    let kind = TextZoneKind::from_byte(read_u8(bytes, cursor)?);
    let encoded_x = read_biased_u16(bytes, cursor)?;
    let encoded_y = read_biased_u16(bytes, cursor)?;
    let width = read_biased_u16(bytes, cursor)?;
    let height = read_biased_u16(bytes, cursor)?;
    let text_delta = read_biased_u16(bytes, cursor)?;
    let text_len = read_u24(bytes, cursor)?;
    let child_count = read_u24(bytes, cursor)?;

    let x = match (parent, previous_sibling) {
        (Some(_), Some(previous)) if uses_previous_sibling_right(kind) => {
            previous.x + previous.width + encoded_x
        }
        (Some(_), Some(previous)) => previous.x + encoded_x,
        (Some(parent), None) => parent.x + encoded_x,
        (None, _) => encoded_x,
    };
    let y_top = match (parent, previous_sibling) {
        (Some(parent), Some(_)) if uses_parent_y(kind) => parent.y_top + encoded_y,
        (Some(_), Some(previous)) => previous.y_top + previous.height + encoded_y,
        (Some(parent), None) => parent.y_top + encoded_y,
        (None, _) => encoded_y,
    };
    let text_start = match (parent, previous_sibling) {
        (Some(_), Some(previous)) => add_text_delta(previous.text_end(), text_delta)?,
        (Some(parent), None) => add_text_delta(parent.text_start, text_delta)?,
        (None, _) => add_text_delta(0, text_delta)?,
    };

    let mut children = Vec::with_capacity(child_count);
    let child_context = ZoneContext {
        x,
        y_top,
        text_start,
    };
    for _ in 0..child_count {
        let previous_child = children.last();
        children.push(parse_zone(
            bytes,
            cursor,
            Some(child_context),
            previous_child,
        )?);
    }

    Ok(TextZone {
        kind,
        text_start,
        text_len,
        x,
        y_top,
        width,
        height,
        children,
    })
}

fn read_u8(bytes: &[u8], cursor: &mut usize) -> ParseResult<u8> {
    require_range(bytes, *cursor, 1)?;
    let value = bytes[*cursor];
    *cursor += 1;
    Ok(value)
}

const fn uses_previous_sibling_right(kind: TextZoneKind) -> bool {
    matches!(kind, TextZoneKind::Word | TextZoneKind::Character)
}

const fn uses_parent_y(kind: TextZoneKind) -> bool {
    matches!(kind, TextZoneKind::Word | TextZoneKind::Character)
}

fn read_biased_u16(bytes: &[u8], cursor: &mut usize) -> ParseResult<i32> {
    require_range(bytes, *cursor, 2)?;
    let value = u16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]);
    *cursor += 2;
    Ok(i32::from(value) - 0x8000)
}

fn read_u24(bytes: &[u8], cursor: &mut usize) -> ParseResult<usize> {
    require_range(bytes, *cursor, 3)?;
    let value = usize::from(bytes[*cursor]) << 16
        | usize::from(bytes[*cursor + 1]) << 8
        | usize::from(bytes[*cursor + 2]);
    *cursor += 3;
    Ok(value)
}

fn add_text_delta(base: usize, delta: i32) -> ParseResult<usize> {
    let value = i64::try_from(base)
        .map_err(|_| ParseError("text zone offset overflow".to_string()))?
        + i64::from(delta);

    usize::try_from(value).map_err(|_| {
        ParseError(format!(
            "text zone offset {value} is outside the supported range"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{TextZoneKind, parse_text_payload, parse_text_zones};

    #[test]
    fn parses_text_and_keeps_zone_data() {
        let payload = [
            0x00, 0x00, 0x05, b'H', b'e', b'l', b'l', b'o', 0x01, 0x02, 0x03,
        ];

        let parsed = parse_text_payload(&payload).expect("text payload should parse");

        assert_eq!(parsed.text, "Hello");
        assert_eq!(parsed.text_len, 5);
        assert_eq!(parsed.zone_data, &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn parses_text_zone_tree() {
        let zone_data = [
            0x01, 0x01, 0x80, 0x00, 0x80, 0x00, 0x8d, 0x74, 0x93, 0xe0, 0x80, 0x00, 0x00, 0x00,
            0x1f, 0x00, 0x00, 0x01, 0x05, 0x83, 0x2e, 0x82, 0x60, 0x87, 0x06, 0x80, 0x41, 0x80,
            0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x01, 0x06, 0x80, 0x00, 0x80, 0x04, 0x81, 0xb9,
            0x80, 0x3d, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
        ];

        let page = parse_text_zones(&zone_data)
            .expect("zone data should parse")
            .expect("root zone should exist");

        assert_eq!(page.kind, TextZoneKind::Page);
        assert_eq!(page.x_min(), 0);
        assert_eq!(page.y_min(page.height), 0);
        assert_eq!(page.x_max(), 3444);
        assert_eq!(page.y_max(page.height), 5088);
        assert_eq!(page.text_len, 31);
        assert_eq!(page.children.len(), 1);

        let line = &page.children[0];
        assert_eq!(line.kind, TextZoneKind::Line);
        assert_eq!(line.x_min(), 814);
        assert_eq!(line.y_min(page.height), 4415);
        assert_eq!(line.x_max(), 2612);
        assert_eq!(line.y_max(page.height), 4480);

        let word = &line.children[0];
        assert_eq!(word.kind, TextZoneKind::Word);
        assert_eq!(word.x_min(), 814);
        assert_eq!(word.y_min(page.height), 4415);
        assert_eq!(word.x_max(), 1255);
        assert_eq!(word.y_max(page.height), 4476);
        assert_eq!(word.text_start, 0);
        assert_eq!(word.text_len, 8);
    }
}
