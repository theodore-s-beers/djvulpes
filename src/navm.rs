use crate::error::{ParseError, ParseResult};
use crate::{Document, decode_bzz};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub title: String,
    pub url: String,
    pub children: Vec<Self>,
}

/// Extracts decoded document bookmarks from the `NAVM` chunk, if present.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, the `NAVM` payload cannot
/// be BZZ-decoded, or the decoded bookmark payload is malformed.
pub fn extract_document_bookmarks(bytes: &[u8]) -> ParseResult<Option<Vec<Bookmark>>> {
    let document = Document::parse(bytes)?;
    let Some(navm) = document.root_chunks.iter().find(|chunk| chunk.id == "NAVM") else {
        return Ok(None);
    };
    let decoded = decode_bzz(&bytes[navm.data_start..navm.data_end])
        .map_err(|error| ParseError(format!("failed to decode NAVM payload: {error}")))?;

    parse_navm_bookmarks(&decoded).map(Some)
}

/// Parses a decoded `NAVM` bookmark payload.
///
/// `NAVM` stores one version byte followed by an obsolete header byte. Top-level
/// bookmarks then continue until the decoded payload ends. Each bookmark stores
/// a little-endian child count, a
/// big-endian title length and title bytes, then a three-byte big-endian URL
/// length and URL bytes. Child bookmarks immediately follow their parent.
///
/// # Errors
///
/// Returns an error if the payload is truncated, has an unsupported version,
/// contains invalid UTF-8 in a title or URL, or has trailing bytes.
pub fn parse_navm_bookmarks(bytes: &[u8]) -> ParseResult<Vec<Bookmark>> {
    let mut cursor = 0usize;
    let version = read_u8(bytes, &mut cursor)?;
    if version != 1 {
        return Err(ParseError(format!("unsupported NAVM version {version}")));
    }

    let _header = read_u8(bytes, &mut cursor)?;
    let mut bookmarks = Vec::new();
    while cursor < bytes.len() {
        let index = bookmarks.len();
        bookmarks.push(parse_bookmark(bytes, &mut cursor, 0).map_err(|error| {
            ParseError(format!(
                "{error} while parsing top-level NAVM bookmark {}",
                index + 1
            ))
        })?);
    }

    Ok(bookmarks)
}

#[cfg(test)]
fn count_bookmarks(bookmarks: &[Bookmark]) -> usize {
    bookmarks
        .iter()
        .map(|bookmark| 1 + count_bookmarks(&bookmark.children))
        .sum()
}

fn parse_bookmark_list(
    bytes: &[u8],
    cursor: &mut usize,
    count: usize,
    depth: usize,
) -> ParseResult<Vec<Bookmark>> {
    let mut bookmarks = Vec::with_capacity(count);
    for index in 0..count {
        bookmarks.push(parse_bookmark(bytes, cursor, depth).map_err(|error| {
            ParseError(format!(
                "{error} while parsing NAVM bookmark {} of {count} at depth {depth}",
                index + 1
            ))
        })?);
    }

    Ok(bookmarks)
}

fn parse_bookmark(bytes: &[u8], cursor: &mut usize, depth: usize) -> ParseResult<Bookmark> {
    let child_count = usize::from(read_u16_le(bytes, cursor)?);
    let title_len = usize::from(read_u16_be(bytes, cursor)?);
    let title = read_string(bytes, cursor, title_len, "title")?;
    let url_len = read_u24_be(bytes, cursor)?;
    let url = read_string(bytes, cursor, url_len, "URL")
        .map_err(|error| ParseError(format!("{error} after NAVM title {title:?}")))?;
    let children = parse_bookmark_list(bytes, cursor, child_count, depth + 1)
        .map_err(|error| ParseError(format!("{error} under NAVM title {title:?}")))?;

    Ok(Bookmark {
        title,
        url,
        children,
    })
}

fn read_string(bytes: &[u8], cursor: &mut usize, len: usize, field: &str) -> ParseResult<String> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| ParseError(format!("NAVM {field} length overflow")))?;
    if end > bytes.len() {
        return Err(ParseError(format!(
            "NAVM {field} is truncated at offset {} with length {len}",
            *cursor
        )));
    }
    let string = std::str::from_utf8(&bytes[*cursor..end])
        .map_err(|error| ParseError(format!("NAVM {field} is not valid UTF-8: {error}")))?
        .to_string();
    *cursor = end;

    Ok(string)
}

fn read_u8(bytes: &[u8], cursor: &mut usize) -> ParseResult<u8> {
    let Some(value) = bytes.get(*cursor).copied() else {
        return Err(ParseError("NAVM payload is truncated".to_string()));
    };
    *cursor += 1;

    Ok(value)
}

fn read_u16_le(bytes: &[u8], cursor: &mut usize) -> ParseResult<u16> {
    let end = cursor
        .checked_add(2)
        .ok_or_else(|| ParseError("NAVM offset overflow".to_string()))?;
    if end > bytes.len() {
        return Err(ParseError("NAVM payload is truncated".to_string()));
    }
    let value = u16::from_le_bytes([bytes[*cursor], bytes[*cursor + 1]]);
    *cursor = end;

    Ok(value)
}

fn read_u16_be(bytes: &[u8], cursor: &mut usize) -> ParseResult<u16> {
    let end = cursor
        .checked_add(2)
        .ok_or_else(|| ParseError("NAVM offset overflow".to_string()))?;
    if end > bytes.len() {
        return Err(ParseError("NAVM payload is truncated".to_string()));
    }
    let value = u16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]);
    *cursor = end;

    Ok(value)
}

fn read_u24_be(bytes: &[u8], cursor: &mut usize) -> ParseResult<usize> {
    let end = cursor
        .checked_add(3)
        .ok_or_else(|| ParseError("NAVM offset overflow".to_string()))?;
    if end > bytes.len() {
        return Err(ParseError("NAVM payload is truncated".to_string()));
    }
    let value = usize::from(bytes[*cursor]) << 16
        | usize::from(bytes[*cursor + 1]) << 8
        | usize::from(bytes[*cursor + 2]);
    *cursor = end;

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{count_bookmarks, parse_navm_bookmarks};
    use crate::{Document, decode_bzz};

    #[test]
    fn parses_rypka_navm_bookmarks() {
        const RYPKA: &[u8] = include_bytes!("../fixtures/Rypka-HIL.djvu");
        let document = Document::parse(RYPKA).expect("document should parse");
        let navm = document
            .root_chunks
            .iter()
            .find(|chunk| chunk.id == "NAVM")
            .expect("Rypka should contain NAVM");
        let decoded =
            decode_bzz(&RYPKA[navm.data_start..navm.data_end]).expect("NAVM payload should decode");
        let bookmarks = parse_navm_bookmarks(&decoded).expect("NAVM should parse");

        assert_eq!(count_bookmarks(&bookmarks), 389);
        assert_eq!(bookmarks.len(), 20);
        assert_eq!(bookmarks[0].title, "Cover ");
        assert_eq!(bookmarks[0].url, "#1");
        assert_eq!(bookmarks[8].children.len(), 4);
        assert_eq!(
            bookmarks[8].title,
            "OTAKAR KLÍMA: AVESTA. ANCIENT PERSIAN INSCRIPTIONS. MIDDLE PERSIAN LITERATURE "
        );
        assert_eq!(bookmarks[8].children[0].url, "#35");
        assert_eq!(bookmarks[19].title, "INDEX ");
        assert_eq!(bookmarks[19].url, "#907");
    }

    #[test]
    fn parses_modern_navm_child_count_larger_than_one_byte() {
        let mut bytes = vec![1, 0];
        push_bookmark_prefix(&mut bytes, 300, "parent", "#1");
        for index in 0..300 {
            push_bookmark_prefix(&mut bytes, 0, &format!("child {index}"), "#2");
        }

        let bookmarks = parse_navm_bookmarks(&bytes).expect("modern NAVM should parse");

        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].title, "parent");
        assert_eq!(bookmarks[0].children.len(), 300);
        assert_eq!(bookmarks[0].children[299].title, "child 299");
    }

    fn push_bookmark_prefix(bytes: &mut Vec<u8>, child_count: u16, title: &str, url: &str) {
        bytes.extend_from_slice(&child_count.to_le_bytes());
        bytes.extend_from_slice(
            &u16::try_from(title.len())
                .expect("test title should fit in NAVM")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(title.as_bytes());
        let url_len = u32::try_from(url.len()).expect("test URL should fit in NAVM");
        bytes.push(u8::try_from(url_len >> 16).expect("high URL length byte should fit"));
        bytes.push(u8::try_from((url_len >> 8) & 0xff).expect("mid URL length byte should fit"));
        bytes.push(u8::try_from(url_len & 0xff).expect("low URL length byte should fit"));
        bytes.extend_from_slice(url.as_bytes());
    }
}
