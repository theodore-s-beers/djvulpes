use std::fmt;

pub type Iw44Result<T> = Result<T, Iw44Error>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44Error(String);

impl Iw44Error {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Iw44Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Iw44Error {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44ChunkHeader {
    pub serial: u8,
    pub slices: u8,
    pub image: Option<Iw44ImageHeader>,
    pub payload_start: usize,
    pub payload_len: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44ImageHeader {
    pub major_version: u8,
    pub minor_version: u8,
    pub width: u16,
    pub height: u16,
    pub grayscale: bool,
    pub delay: u8,
    pub chroma_half: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44LayerSummary {
    pub image: Iw44ImageHeader,
    pub chunks: Vec<Iw44ChunkHeader>,
    pub total_slices: u32,
    pub total_payload_bytes: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44PageMapping {
    pub page_width: u32,
    pub page_height: u32,
    pub layer_width: u32,
    pub layer_height: u32,
    pub subsample: u32,
    pub scaled_width: u32,
    pub scaled_height: u32,
    pub horizontal_overscan: u32,
    pub vertical_overscan: u32,
}

impl Iw44LayerSummary {
    #[must_use]
    pub fn page_mapping(&self, page_width: u32, page_height: u32) -> Iw44PageMapping {
        let layer_width = u32::from(self.image.width);
        let layer_height = u32::from(self.image.height);
        let subsample = page_width
            .div_ceil(layer_width)
            .max(page_height.div_ceil(layer_height))
            .max(1);
        let scaled_width = layer_width.saturating_mul(subsample);
        let scaled_height = layer_height.saturating_mul(subsample);

        Iw44PageMapping {
            page_width,
            page_height,
            layer_width,
            layer_height,
            subsample,
            scaled_width,
            scaled_height,
            horizontal_overscan: scaled_width.saturating_sub(page_width),
            vertical_overscan: scaled_height.saturating_sub(page_height),
        }
    }
}

/// Reads the IW44 chunk-local header from a raw `FG44`, `BG44`, or `TH44`
/// payload.
///
/// # Errors
///
/// Returns an error if the payload is too short, declares a truncated first
/// chunk header, or declares zero image dimensions.
pub fn read_iw44_chunk_header(bytes: &[u8]) -> Iw44Result<Iw44ChunkHeader> {
    if bytes.len() < 2 {
        return Err(Iw44Error::new("IW44 chunk is too short"));
    }

    let serial = bytes[0];
    let slices = bytes[1];
    let (image, payload_start) = if serial == 0 {
        if bytes.len() < 9 {
            return Err(Iw44Error::new("IW44 first chunk header is too short"));
        }
        let major_version = bytes[2];
        let minor_version = bytes[3];
        let width = u16::from_be_bytes([bytes[4], bytes[5]]);
        let height = u16::from_be_bytes([bytes[6], bytes[7]]);
        if width == 0 || height == 0 {
            return Err(Iw44Error::new("IW44 image has zero dimension"));
        }
        let delay_byte = bytes[8];
        let grayscale = (major_version & 0x80) != 0;
        let delay = if minor_version >= 2 {
            delay_byte & 0x7f
        } else {
            0
        };
        let chroma_half = !grayscale && minor_version >= 2 && (delay_byte & 0x80) == 0;

        (
            Some(Iw44ImageHeader {
                major_version,
                minor_version,
                width,
                height,
                grayscale,
                delay,
                chroma_half,
            }),
            9,
        )
    } else {
        (None, 2)
    };

    Ok(Iw44ChunkHeader {
        serial,
        slices,
        image,
        payload_start,
        payload_len: bytes.len() - payload_start,
    })
}

/// Reads and validates a progressive IW44 chunk sequence.
///
/// # Errors
///
/// Returns an error if the sequence is empty, does not start with serial `0`,
/// has missing image metadata on the first chunk, or has non-contiguous serials.
pub fn summarize_iw44_layer<'a, I>(chunks: I) -> Iw44Result<Iw44LayerSummary>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut headers = Vec::new();
    let mut total_slices = 0u32;
    let mut total_payload_bytes = 0usize;

    for (index, chunk) in chunks.into_iter().enumerate() {
        let header = read_iw44_chunk_header(chunk)?;
        let expected_serial = u8::try_from(index).map_err(|_| {
            Iw44Error::new(format!(
                "IW44 layer has more than {} chunks",
                usize::from(u8::MAX) + 1
            ))
        })?;
        if header.serial != expected_serial {
            return Err(Iw44Error::new(format!(
                "IW44 chunk serial {} does not match expected {expected_serial}",
                header.serial
            )));
        }
        total_slices += u32::from(header.slices);
        total_payload_bytes += header.payload_len;
        headers.push(header);
    }

    let first = headers
        .first()
        .ok_or_else(|| Iw44Error::new("IW44 layer has no chunks"))?;
    let image = first
        .image
        .ok_or_else(|| Iw44Error::new("IW44 layer first chunk has no image header"))?;

    Ok(Iw44LayerSummary {
        image,
        chunks: headers,
        total_slices,
        total_payload_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_first_iw44_chunk_header() {
        let header =
            read_iw44_chunk_header(&[0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff])
                .expect("IW44 header should parse");

        assert_eq!(
            header,
            Iw44ChunkHeader {
                serial: 0,
                slices: 74,
                image: Some(Iw44ImageHeader {
                    major_version: 1,
                    minor_version: 2,
                    width: 780,
                    height: 817,
                    grayscale: false,
                    delay: 10,
                    chroma_half: false,
                }),
                payload_start: 9,
                payload_len: 1,
            }
        );
    }

    #[test]
    fn reads_subsequent_iw44_chunk_header() {
        let header = read_iw44_chunk_header(&[0x01, 0x0a, 0xaa]).expect("IW44 header should parse");

        assert_eq!(
            header,
            Iw44ChunkHeader {
                serial: 1,
                slices: 10,
                image: None,
                payload_start: 2,
                payload_len: 1,
            }
        );
    }

    #[test]
    fn reads_grayscale_first_iw44_chunk_header() {
        let header =
            read_iw44_chunk_header(&[0x00, 0x01, 0x80, 0x02, 0x00, 0x20, 0x00, 0x10, 0x00])
                .expect("IW44 header should parse");

        assert_eq!(
            header.image,
            Some(Iw44ImageHeader {
                major_version: 0x80,
                minor_version: 2,
                width: 32,
                height: 16,
                grayscale: true,
                delay: 0,
                chroma_half: false,
            })
        );
    }

    #[test]
    fn rejects_short_iw44_chunks() {
        assert_eq!(
            read_iw44_chunk_header(&[0]).expect_err("short chunk should fail"),
            Iw44Error::new("IW44 chunk is too short")
        );
        assert_eq!(
            read_iw44_chunk_header(&[0, 1, 0]).expect_err("short first header should fail"),
            Iw44Error::new("IW44 first chunk header is too short")
        );
    }

    #[test]
    fn rejects_zero_iw44_dimensions() {
        let error = read_iw44_chunk_header(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00])
            .expect_err("zero dimension should fail");

        assert_eq!(error, Iw44Error::new("IW44 image has zero dimension"));
    }

    #[test]
    fn summarizes_iw44_layer_chunks() {
        let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
        let second = [0x01, 0x0a, 0xaa, 0xbb];

        let summary = summarize_iw44_layer([first.as_slice(), second.as_slice()])
            .expect("layer should parse");

        assert_eq!(summary.image.width, 780);
        assert_eq!(summary.image.height, 817);
        assert_eq!(summary.chunks.len(), 2);
        assert_eq!(summary.total_slices, 84);
        assert_eq!(summary.total_payload_bytes, 3);
    }

    #[test]
    fn maps_iw44_layer_to_page_space() {
        let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
        let summary = summarize_iw44_layer([first.as_slice()]).expect("layer should parse");

        assert_eq!(
            summary.page_mapping(1560, 1633),
            Iw44PageMapping {
                page_width: 1560,
                page_height: 1633,
                layer_width: 780,
                layer_height: 817,
                subsample: 2,
                scaled_width: 1560,
                scaled_height: 1634,
                horizontal_overscan: 0,
                vertical_overscan: 1,
            }
        );
    }

    #[test]
    fn rejects_empty_iw44_layer() {
        let chunks: [&[u8]; 0] = [];
        let error = summarize_iw44_layer(chunks).expect_err("empty layer should fail");

        assert_eq!(error, Iw44Error::new("IW44 layer has no chunks"));
    }

    #[test]
    fn rejects_iw44_layer_without_first_chunk() {
        let error = summarize_iw44_layer([[0x01, 0x0a].as_slice()])
            .expect_err("missing first chunk should fail");

        assert_eq!(
            error,
            Iw44Error::new("IW44 chunk serial 1 does not match expected 0")
        );
    }

    #[test]
    fn rejects_iw44_layer_with_serial_gap() {
        let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
        let third = [0x02, 0x0a, 0xaa];
        let error = summarize_iw44_layer([first.as_slice(), third.as_slice()])
            .expect_err("serial gap should fail");

        assert_eq!(
            error,
            Iw44Error::new("IW44 chunk serial 2 does not match expected 1")
        );
    }
}
