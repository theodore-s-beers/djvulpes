use crate::chunk::require_range;
use crate::document::Document;
use crate::error::{ParseError, ParseResult};
use crate::page::PageChunkKind;
use crate::{BzzError, decode_bzz, decode_dirm_tail, parse_dirm_tail};
use std::fmt;

#[derive(Debug)]
pub enum TextError {
    Parse(ParseError),
    Bzz(BzzError),
    ZeroFromPage,
    ReversedPageRange,
    PageOutOfRange { page: usize, page_count: usize },
}

impl fmt::Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::Bzz(error) => error.fmt(formatter),
            Self::ZeroFromPage => formatter.write_str("from_page must be 1 or greater"),
            Self::ReversedPageRange => {
                formatter.write_str("to_page must be greater than or equal to from_page")
            }
            Self::PageOutOfRange { page, page_count } => {
                write!(
                    formatter,
                    "page {page} not found; document has {page_count} pages"
                )
            }
        }
    }
}

impl std::error::Error for TextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Bzz(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ParseError> for TextError {
    fn from(error: ParseError) -> Self {
        Self::Parse(error)
    }
}

impl From<BzzError> for TextError {
    fn from(error: BzzError) -> Self {
        Self::Bzz(error)
    }
}

pub type TextResult<T> = Result<T, TextError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPayload<'a> {
    pub text: &'a str,
    pub text_len: usize,
    pub zone_data: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedTextPage {
    pub page_number: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedTextZonePage {
    pub page_number: usize,
    pub text: String,
    pub zone: Option<TextZone>,
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

/// Extracts hidden text from a page range.
///
/// Page numbers are 1-based. If `to_page` is `None`, extraction continues
/// through the final page. Shared `INCL` forms are resolved in the same order as
/// the rendering path before `TXTa`/`TXTz` chunks are collected.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, the bundled directory
/// cannot be decoded, the range is invalid, or a selected text chunk is
/// malformed.
pub fn extract_document_text_pages(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> TextResult<Vec<ExtractedTextPage>> {
    let document = Document::parse(bytes)?;
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(bytes, dirm)?;
        parse_dirm_tail(dirm, &decoded_tail)?
    } else {
        Vec::new()
    };
    let page_count = document.form_kind_counts().pages;
    let end_page = checked_text_page_range(page_count, from_page, to_page)?;
    let mut pages = Vec::new();
    let mut selected_pages = 0usize;

    for (index, page) in document.pages(bytes).enumerate() {
        let page_number = index + 1;
        if page_number < from_page || page_number > end_page {
            continue;
        }

        let page = page?;
        let chunks = document.resolved_page_chunks(bytes, &page, &tail_entries)?;
        let mut text = String::new();
        for chunk in chunks
            .iter()
            .filter(|chunk| matches!(chunk.chunk.kind, PageChunkKind::Txta | PageChunkKind::Txtz))
        {
            let data = &chunk.chunk.chunk;
            let payload =
                text_chunk_payload(bytes, chunk.chunk.kind, data.data_start, data.data_end)?;
            text.push_str(parse_text_payload(&payload)?.text);
        }
        pages.push(ExtractedTextPage { page_number, text });
        selected_pages += 1;
    }

    if selected_pages == 0 {
        return Err(TextError::PageOutOfRange {
            page: from_page,
            page_count,
        });
    }

    Ok(pages)
}

/// Extracts hidden text and its zone tree from a page range.
///
/// Empty pages are preserved with an empty text string and no zone tree,
/// matching `djvused print-txt` page selection behavior.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, the bundled directory
/// cannot be decoded, the range is invalid, or a selected text chunk or zone
/// stream is malformed.
pub fn extract_document_text_zone_pages(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> TextResult<Vec<ExtractedTextZonePage>> {
    let document = Document::parse(bytes)?;
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(bytes, dirm)?;
        parse_dirm_tail(dirm, &decoded_tail)?
    } else {
        Vec::new()
    };
    let page_count = document.form_kind_counts().pages;
    let end_page = checked_text_page_range(page_count, from_page, to_page)?;
    let mut pages = Vec::new();
    let mut selected_pages = 0usize;

    for (index, page) in document.pages(bytes).enumerate() {
        let page_number = index + 1;
        if page_number < from_page || page_number > end_page {
            continue;
        }

        let page = page?;
        let chunks = document.resolved_page_chunks(bytes, &page, &tail_entries)?;
        let mut text = String::new();
        let mut zone = None;
        for chunk in chunks
            .iter()
            .filter(|chunk| matches!(chunk.chunk.kind, PageChunkKind::Txta | PageChunkKind::Txtz))
        {
            let data = &chunk.chunk.chunk;
            let payload =
                text_chunk_payload(bytes, chunk.chunk.kind, data.data_start, data.data_end)?;
            let parsed = parse_text_payload(&payload)?;
            text.push_str(parsed.text);
            if zone.is_none() {
                zone = parse_text_zones(parsed.zone_data)?;
            }
        }
        pages.push(ExtractedTextZonePage {
            page_number,
            text,
            zone,
        });
        selected_pages += 1;
    }

    if selected_pages == 0 {
        return Err(TextError::PageOutOfRange {
            page: from_page,
            page_count,
        });
    }

    Ok(pages)
}

/// Extracts hidden text from a page range using `djvutxt`-compatible page
/// separators.
///
/// Empty pages emit no bytes. Non-empty pages emit the page text followed by a
/// newline and a form-feed byte, matching `djvutxt --page=N`.
///
/// # Errors
///
/// Returns the same errors as [`extract_document_text_pages`].
pub fn extract_document_text(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> TextResult<String> {
    let mut text = String::new();
    for page in extract_document_text_pages(bytes, from_page, to_page)? {
        if !page.text.is_empty() {
            push_without_nul(&mut text, &page.text);
            text.push('\n');
            text.push('\x0c');
        }
    }

    Ok(text)
}

fn push_without_nul(output: &mut String, text: &str) {
    output.extend(text.chars().filter(|&character| character != '\0'));
}

/// Formats a page range as `djvused print-txt` style text-zone expressions.
///
/// # Errors
///
/// Returns the same errors as [`extract_document_text_zone_pages`].
pub fn format_document_text_zones(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> TextResult<String> {
    let mut output = String::new();
    for page in extract_document_text_zone_pages(bytes, from_page, to_page)? {
        match &page.zone {
            Some(zone) => {
                format_text_zone_expression(&mut output, zone, &page.text, 0, 0);
            }
            None => output.push_str("(page 0 0 0 0 \"\")\n"),
        }
    }

    Ok(output)
}

fn format_text_zone_expression(
    output: &mut String,
    zone: &TextZone,
    text: &str,
    depth: usize,
    trailing_closes: usize,
) {
    output.push_str(&" ".repeat(depth));
    output.push('(');
    output.push_str(zone.kind.as_str());
    {
        use std::fmt::Write as _;
        write!(
            output,
            " {} {} {} {}",
            zone.x_min(),
            zone.y_min(),
            zone.x_max(),
            zone.y_max()
        )
        .expect("writing to string should not fail");
    }

    if zone.children.is_empty() {
        output.push_str(" \"");
        output.push_str(&escape_text_zone_string(zone_text(zone, text)));
        output.push_str("\")");
        for _ in 0..trailing_closes {
            output.push(')');
        }
        output.push('\n');
        return;
    }

    output.push('\n');
    for (index, child) in zone.children.iter().enumerate() {
        let child_trailing_closes = if index + 1 == zone.children.len() {
            trailing_closes + 1
        } else {
            0
        };
        format_text_zone_expression(output, child, text, depth + 1, child_trailing_closes);
    }
}

fn zone_text<'a>(zone: &TextZone, text: &'a str) -> &'a str {
    text.get(zone.text_start..zone.text_end())
        .unwrap_or("")
        .trim_end()
}

fn escape_text_zone_string(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let mut buffer = [0; 4];
                for byte in character.encode_utf8(&mut buffer).as_bytes() {
                    write!(&mut escaped, "\\{byte:03o}")
                        .expect("writing to string should not fail");
                }
            }
            character => escaped.push(character),
        }
    }

    escaped
}

fn checked_text_page_range(
    page_count: usize,
    from_page: usize,
    to_page: Option<usize>,
) -> TextResult<usize> {
    if from_page == 0 {
        return Err(TextError::ZeroFromPage);
    }
    if let Some(to_page) = to_page
        && to_page < from_page
    {
        return Err(TextError::ReversedPageRange);
    }

    let end_page = to_page.unwrap_or(page_count);
    if from_page > page_count {
        return Err(TextError::PageOutOfRange {
            page: from_page,
            page_count,
        });
    }
    if end_page > page_count {
        return Err(TextError::PageOutOfRange {
            page: end_page,
            page_count,
        });
    }

    Ok(end_page)
}

fn text_chunk_payload(
    bytes: &[u8],
    kind: PageChunkKind,
    start: usize,
    end: usize,
) -> TextResult<Vec<u8>> {
    match kind {
        PageChunkKind::Txta => Ok(bytes[start..end].to_vec()),
        PageChunkKind::Txtz => Ok(decode_bzz(&bytes[start..end])?),
        _ => Ok(Vec::new()),
    }
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
    pub const fn y_min(&self) -> i32 {
        self.y_top
    }

    #[must_use]
    pub const fn y_max(&self) -> i32 {
        self.y_top + self.height
    }
}

#[derive(Debug, Clone, Copy)]
struct ZoneContext {
    x: i32,
    y_top: i32,
    height: i32,
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
        (Some(_), Some(previous)) if uses_previous_sibling_left(kind) => previous.x + encoded_x,
        (Some(_), Some(previous)) => previous.x + previous.width + encoded_x,
        (Some(parent), None) => parent.x + encoded_x,
        (None, _) => encoded_x,
    };
    let y_top = match (parent, previous_sibling) {
        (Some(_), Some(previous)) if uses_previous_sibling_lower_left(kind) => {
            previous.y_top - encoded_y - height
        }
        (Some(_), Some(previous)) => previous.y_top + encoded_y,
        (Some(parent), None) => parent.y_top + parent.height - encoded_y - height,
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
        height,
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

const fn uses_previous_sibling_left(kind: TextZoneKind) -> bool {
    matches!(
        kind,
        TextZoneKind::Page | TextZoneKind::Paragraph | TextZoneKind::Line
    )
}

const fn uses_previous_sibling_lower_left(kind: TextZoneKind) -> bool {
    matches!(
        kind,
        TextZoneKind::Page | TextZoneKind::Paragraph | TextZoneKind::Line
    )
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
    use super::{TextZone, TextZoneKind, parse_text_payload, parse_text_zones};

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
            0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x04, 0x06, 0x80, 0x00, 0x80, 0x04, 0x81, 0xb9,
            0x80, 0x3d, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x06, 0x80, 0x32, 0x80,
            0x01, 0x80, 0x7c, 0x80, 0x3d, 0x80, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x06,
            0x80, 0x33, 0x80, 0x00, 0x81, 0xba, 0x80, 0x40, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00,
            0x00, 0x00, 0x06, 0x80, 0x32, 0x80, 0x00, 0x82, 0x80, 0x80, 0x3e, 0x80, 0x00, 0x00,
            0x00, 0x0b, 0x00, 0x00, 0x00,
        ];

        let page = parse_text_zones(&zone_data)
            .expect("zone data should parse")
            .expect("root zone should exist");

        assert_eq!(page.kind, TextZoneKind::Page);
        assert_eq!(page.x_min(), 0);
        assert_eq!(page.y_min(), 0);
        assert_eq!(page.x_max(), 3444);
        assert_eq!(page.y_max(), 5088);
        assert_eq!(page.text_len, 31);
        assert_eq!(page.children.len(), 1);

        let line = &page.children[0];
        assert_eq!(line.kind, TextZoneKind::Line);
        assert_eq!(line.x_min(), 814);
        assert_eq!(line.y_min(), 4415);
        assert_eq!(line.x_max(), 2612);
        assert_eq!(line.y_max(), 4480);

        let word = &line.children[0];
        assert_eq!(word.kind, TextZoneKind::Word);
        assert_eq!(word.x_min(), 814);
        assert_eq!(word.y_min(), 4415);
        assert_eq!(word.x_max(), 1255);
        assert_eq!(word.y_max(), 4476);
        assert_eq!(word.text_start, 0);
        assert_eq!(word.text_len, 8);

        let word = &line.children[1];
        assert_eq!(word.kind, TextZoneKind::Word);
        assert_eq!(word.x_min(), 1305);
        assert_eq!(word.y_min(), 4416);
        assert_eq!(word.x_max(), 1429);
        assert_eq!(word.y_max(), 4477);
        assert_eq!(word.text_start, 8);
        assert_eq!(word.text_len, 3);
    }

    #[test]
    fn formats_root_zone_with_nonzero_page_offset() {
        let page = TextZone {
            kind: TextZoneKind::Page,
            text_start: 0,
            text_len: 0,
            x: 167,
            y_top: 314,
            width: 1264,
            height: 2147,
            children: Vec::new(),
        };

        assert_eq!(page.y_min(), 314);
        assert_eq!(page.y_max(), 2461);
    }
}
