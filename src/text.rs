use crate::chunk::require_range;
use crate::error::{ParseError, ParseResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPayload<'a> {
    pub text: &'a str,
    pub text_len: usize,
    pub zone_data: &'a [u8],
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

#[cfg(test)]
mod tests {
    use super::parse_text_payload;

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
}
