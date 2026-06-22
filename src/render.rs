use crate::dirm::DirmTailEntry;
use crate::document::{Document, Page, ResolvedPageChunk};
use crate::error::{ParseError, ParseResult};
use crate::info::PageInfo;
use crate::iw44::{
    Iw44Decoder, Iw44Error, Iw44LayerSummary, Iw44PageMapping, Iw44Result, Iw44RgbImage,
    summarize_iw44_layer,
};
use crate::jb2::{
    Jb2Dictionary, Jb2Error, Jb2ImageHeader, Jb2PartialImage, Jb2RecordPrefix,
    decode_jb2_dictionary, read_jb2_image_header, read_jb2_record_prefix, render_jb2_image,
    render_jb2_image_with_dictionary, render_jb2_supported_prefix,
};
use crate::page::PageChunkKind;
use crate::{BzzError, decode_dirm_tail, parse_dirm_tail};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PixelFormat {
    Rgb8,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PageBitmap {
    pub width: u32,
    pub height: u32,
    pub dpi: u16,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PageBitmapStats {
    pub width: u32,
    pub height: u32,
    pub pixel_count: usize,
    pub black_pixels: usize,
    pub white_pixels: usize,
    pub non_gray_pixels: usize,
    pub component_sum: u64,
    pub fingerprint: u64,
}

mod diff;
pub use diff::{
    PageBitmapChannelDiff, PageBitmapDiff, PageBitmapDiffBounds, PageBitmapDiffPixel,
    PageBitmapDiffRegionSummary, PageBitmapDiffTileSummary, RenderCompareLimits,
    bitmap_diff_failures, bitmap_diff_region_summary, bitmap_diff_tile_summaries,
};

pub type RenderResult<T> = Result<T, RenderError>;

pub type DjvuRenderResult<T> = Result<T, DjvuRenderError>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RenderError(String);

#[derive(Debug, thiserror::Error)]
pub enum DjvuRenderError {
    #[error("{0}")]
    Parse(#[from] ParseError),
    #[error("{0}")]
    Bzz(#[from] BzzError),
    #[error("{0}")]
    Render(#[from] RenderError),
    #[error("page number must be 1 or greater")]
    ZeroPage,
    #[error("from page must be 1 or greater")]
    ZeroFromPage,
    #[error("to page must be greater than or equal to from page")]
    ReversedPageRange,
    #[error("page {page} not found; document has {page_count} pages")]
    PageOutOfRange { page: usize, page_count: usize },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DjvuPageRenderEvent<'a> {
    PageStarted {
        page_number: usize,
        end_page: usize,
    },
    PageRendered {
        page_number: usize,
        render: &'a PartialPageRender,
    },
}

impl RenderError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

impl From<Jb2Error> for RenderError {
    fn from(error: Jb2Error) -> Self {
        Self(error.to_string())
    }
}

impl From<Iw44Error> for RenderError {
    fn from(error: Iw44Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BitonalBitmap {
    pub width: u32,
    pub height: u32,
    bits: Vec<u8>,
}

impl BitonalBitmap {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let bit_count = (width as usize).saturating_mul(height as usize);
        let bytes = bit_count.div_ceil(8);

        Self {
            width,
            height,
            bits: vec![0; bytes],
        }
    }

    #[must_use]
    pub fn from_bits(width: u32, height: u32, bits: Vec<u8>) -> Option<Self> {
        let bit_count = (width as usize).checked_mul(height as usize)?;
        if bits.len() != bit_count.div_ceil(8) {
            return None;
        }

        Some(Self {
            width,
            height,
            bits,
        })
    }

    #[must_use]
    pub fn bit(&self, x: u32, y: u32) -> Option<bool> {
        let bit_index = self.bit_index(x, y)?;
        let byte = self.bits[bit_index / 8];
        let mask = 0x80 >> (bit_index % 8);

        Some(byte & mask != 0)
    }

    pub fn set_bit(&mut self, x: u32, y: u32, value: bool) -> bool {
        let Some(bit_index) = self.bit_index(x, y) else {
            return false;
        };
        let byte = &mut self.bits[bit_index / 8];
        let mask = 0x80 >> (bit_index % 8);

        if value {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }

        true
    }

    #[must_use]
    pub fn to_image_mask_bytes(&self) -> Vec<u8> {
        let row_bytes = (self.width as usize).div_ceil(8);
        if self.width.is_multiple_of(8) {
            return self.bits.clone();
        }

        let mut bytes = Vec::with_capacity(row_bytes.saturating_mul(self.height as usize));

        for y in 0..self.height {
            let mut row = vec![0; row_bytes];
            for x in 0..self.width {
                if self.bit(x, y).unwrap_or(false) {
                    let x = x as usize;
                    row[x / 8] |= 0x80 >> (x % 8);
                }
            }
            bytes.extend_from_slice(&row);
        }

        bytes
    }

    #[must_use]
    pub fn to_pbm_bytes(&self) -> Vec<u8> {
        let mut bytes = format!("P4\n{} {}\n", self.width, self.height).into_bytes();
        bytes.extend_from_slice(&self.to_image_mask_bytes());

        bytes
    }

    #[must_use]
    pub fn black_pixel_count(&self) -> usize {
        self.bits
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum()
    }

    fn bit_index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let width = usize::try_from(self.width).ok()?;
        let x = usize::try_from(x).ok()?;
        let y = usize::try_from(y).ok()?;
        y.checked_mul(width)?.checked_add(x)
    }
}

impl PageBitmap {
    #[must_use]
    pub fn new_rgb8(width: u32, height: u32, dpi: u16, fill: [u8; 3]) -> Self {
        let pixel_count = (width as usize).saturating_mul(height as usize);
        let mut pixels = Vec::with_capacity(pixel_count.saturating_mul(3));

        for _ in 0..pixel_count {
            pixels.extend_from_slice(&fill);
        }

        Self {
            width,
            height,
            dpi,
            format: PixelFormat::Rgb8,
            pixels,
        }
    }

    #[must_use]
    pub fn white_rgb8(info: &PageInfo) -> Self {
        Self::new_rgb8(
            u32::from(info.width),
            u32::from(info.height),
            info.dpi,
            [0xff, 0xff, 0xff],
        )
    }

    #[must_use]
    pub fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let width = usize::try_from(self.width).ok()?;
        let x = usize::try_from(x).ok()?;
        let y = usize::try_from(y).ok()?;
        y.checked_mul(width)?.checked_add(x)?.checked_mul(3)
    }

    pub fn set_rgb(&mut self, x: u32, y: u32, color: [u8; 3]) -> bool {
        let Some(offset) = self.pixel_offset(x, y) else {
            return false;
        };

        self.pixels[offset..offset + 3].copy_from_slice(&color);
        true
    }

    pub fn paint_bitonal_mask(&mut self, mask: &BitonalBitmap, color: [u8; 3]) -> bool {
        if mask.width != self.width || mask.height != self.height {
            return false;
        }

        for y in 0..mask.height {
            for x in 0..mask.width {
                if mask.bit(x, y).unwrap_or(false) {
                    self.set_rgb(x, y, color);
                }
            }
        }

        true
    }

    pub fn paint_iw44_rgb_layer(
        &mut self,
        image: &Iw44RgbImage,
        mapping: &Iw44PageMapping,
    ) -> bool {
        if !iw44_rgb_layer_matches_page(image, mapping, self.width, self.height) {
            return false;
        }

        for y in 0..self.height {
            for x in 0..self.width {
                self.set_rgb(x, y, iw44_background_pixel(image, mapping, x, y));
            }
        }

        true
    }

    pub fn paint_iw44_rgb_layer_through_mask(
        &mut self,
        image: &Iw44RgbImage,
        mapping: &Iw44PageMapping,
        mask: &BitonalBitmap,
    ) -> bool {
        if mask.width != self.width
            || mask.height != self.height
            || !iw44_rgb_layer_matches_page(image, mapping, self.width, self.height)
        {
            return false;
        }

        for y in 0..self.height {
            let source_y = iw44_source_coordinate(
                y,
                mapping.vertical_overscan,
                mapping.subsample,
                image.height,
            );
            for x in 0..self.width {
                if !mask.bit(x, y).unwrap_or(false) {
                    continue;
                }

                let source_x = iw44_source_coordinate(x, 0, mapping.subsample, image.width);
                let source_offset = (source_y * image.width + source_x) * 3;
                self.set_rgb(
                    x,
                    y,
                    [
                        image.pixels[source_offset],
                        image.pixels[source_offset + 1],
                        image.pixels[source_offset + 2],
                    ],
                );
            }
        }

        true
    }

    #[must_use]
    pub fn to_ppm_bytes(&self) -> Vec<u8> {
        let mut bytes = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        bytes.extend_from_slice(&self.pixels);
        bytes
    }

    /// Reads a binary PPM/P6 RGB bitmap.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not a supported PPM/P6 image, has
    /// dimensions that overflow the RGB buffer size, or has truncated pixel data.
    pub fn from_ppm_bytes(bytes: &[u8], dpi: u16) -> RenderResult<Self> {
        let mut cursor = PpmTokenCursor::new(bytes);
        let magic = cursor.next_token()?;
        if magic != b"P6" {
            return Err(RenderError::new("PPM oracle must use binary P6 format"));
        }
        let width = parse_ppm_u32(cursor.next_token()?, "width")?;
        let height = parse_ppm_u32(cursor.next_token()?, "height")?;
        let max_value = parse_ppm_u32(cursor.next_token()?, "max value")?;
        if max_value != 255 {
            return Err(RenderError::new(format!(
                "PPM oracle max value {max_value} is not supported"
            )));
        }
        let pixel_start = cursor.pixel_start()?;
        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| RenderError::new("PPM oracle dimensions overflow"))?;
        let pixel_bytes = pixel_count
            .checked_mul(3)
            .ok_or_else(|| RenderError::new("PPM oracle pixel buffer overflows"))?;
        let pixel_end = pixel_start
            .checked_add(pixel_bytes)
            .ok_or_else(|| RenderError::new("PPM oracle pixel range overflows"))?;
        let pixels = bytes
            .get(pixel_start..pixel_end)
            .ok_or_else(|| RenderError::new("PPM oracle pixel data is truncated"))?
            .to_vec();

        Ok(Self {
            width,
            height,
            dpi,
            format: PixelFormat::Rgb8,
            pixels,
        })
    }

    #[must_use]
    pub fn stats(&self) -> PageBitmapStats {
        let mut black_pixels = 0;
        let mut white_pixels = 0;
        let mut non_gray_pixels = 0;
        let mut component_sum = 0u64;
        let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;

        fingerprint = fnv1a_u64(fingerprint, &self.width.to_be_bytes());
        fingerprint = fnv1a_u64(fingerprint, &self.height.to_be_bytes());
        fingerprint = fnv1a_u64(fingerprint, &self.dpi.to_be_bytes());

        for pixel in self.pixels.chunks_exact(3) {
            let [red, green, blue] = [pixel[0], pixel[1], pixel[2]];
            if [red, green, blue] == [0, 0, 0] {
                black_pixels += 1;
            }
            if [red, green, blue] == [0xff, 0xff, 0xff] {
                white_pixels += 1;
            }
            if red != green || green != blue {
                non_gray_pixels += 1;
            }
            component_sum += u64::from(red) + u64::from(green) + u64::from(blue);
            fingerprint = fnv1a_u64(fingerprint, pixel);
        }

        PageBitmapStats {
            width: self.width,
            height: self.height,
            pixel_count: self.pixels.len() / 3,
            black_pixels,
            white_pixels,
            non_gray_pixels,
            component_sum,
            fingerprint,
        }
    }
}

struct PpmTokenCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> PpmTokenCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn next_token(&mut self) -> RenderResult<&'a [u8]> {
        self.skip_space_and_comments();
        let start = self.offset;
        while self.offset < self.bytes.len() && !self.bytes[self.offset].is_ascii_whitespace() {
            self.offset += 1;
        }
        if start == self.offset {
            return Err(RenderError::new("PPM header is truncated"));
        }
        Ok(&self.bytes[start..self.offset])
    }

    fn pixel_start(&mut self) -> RenderResult<usize> {
        if self.offset >= self.bytes.len() || !self.bytes[self.offset].is_ascii_whitespace() {
            return Err(RenderError::new(
                "PPM header is missing pixel-data separator",
            ));
        }
        self.offset += 1;
        Ok(self.offset)
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            while self.offset < self.bytes.len() && self.bytes[self.offset].is_ascii_whitespace() {
                self.offset += 1;
            }
            if self.bytes.get(self.offset) != Some(&b'#') {
                break;
            }
            while self.offset < self.bytes.len() && self.bytes[self.offset] != b'\n' {
                self.offset += 1;
            }
        }
    }
}

fn parse_ppm_u32(token: &[u8], field: &str) -> RenderResult<u32> {
    let text = std::str::from_utf8(token)
        .map_err(|_| RenderError::new(format!("PPM {field} is not UTF-8")))?;
    text.parse::<u32>()
        .map_err(|_| RenderError::new(format!("PPM {field} is not an integer")))
}

fn fnv1a_u64(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn iw44_rgb_layer_matches_page(
    image: &Iw44RgbImage,
    mapping: &Iw44PageMapping,
    page_width: u32,
    page_height: u32,
) -> bool {
    mapping.page_width == page_width
        && mapping.page_height == page_height
        && usize::try_from(mapping.layer_width).ok() == Some(image.width)
        && usize::try_from(mapping.layer_height).ok() == Some(image.height)
        && mapping.subsample != 0
        && image.width != 0
        && image.height != 0
        && image.pixels.len() == image.width.saturating_mul(image.height).saturating_mul(3)
}

fn iw44_source_coordinate(
    page_coordinate: u32,
    overscan: u32,
    subsample: u32,
    limit: usize,
) -> usize {
    let source = (page_coordinate + overscan) / subsample;
    usize::try_from(source)
        .unwrap_or(usize::MAX)
        .min(limit.saturating_sub(1))
}

fn iw44_background_pixel(
    image: &Iw44RgbImage,
    mapping: &Iw44PageMapping,
    x: u32,
    y: u32,
) -> [u8; 3] {
    if mapping.subsample == 1 {
        let source_y = iw44_source_coordinate(
            y,
            mapping.vertical_overscan,
            mapping.subsample,
            image.height,
        );
        let source_x = iw44_source_coordinate(
            x,
            mapping.horizontal_overscan,
            mapping.subsample,
            image.width,
        );
        return iw44_native_pixel(image, source_x, source_y);
    }

    iw44_scaled_pixel(image, mapping, x, y)
}

fn iw44_scaled_pixel(image: &Iw44RgbImage, mapping: &Iw44PageMapping, x: u32, y: u32) -> [u8; 3] {
    let fixed_x = iw44_scaler_coordinate(image.width, mapping.subsample, x);
    let fixed_y = iw44_top_down_scaler_coordinate(image.height, mapping, y);
    let (left_x, x_fraction) = iw44_scaler_index_and_fraction(fixed_x);
    let (top_y, y_fraction) = iw44_scaler_index_and_fraction(fixed_y);
    let right_x = left_x + 1;
    let bottom_y = top_y + 1;

    let mut pixel = [0; 3];
    for (channel, component) in pixel.iter_mut().enumerate() {
        let left = iw44_interpolate_fixed(
            u16::from(iw44_native_channel_at(image, left_x, top_y, channel)),
            u16::from(iw44_native_channel_at(image, left_x, bottom_y, channel)),
            y_fraction,
        );
        let right = iw44_interpolate_fixed(
            u16::from(iw44_native_channel_at(image, right_x, top_y, channel)),
            u16::from(iw44_native_channel_at(image, right_x, bottom_y, channel)),
            y_fraction,
        );
        let value = iw44_interpolate_fixed(left, right, x_fraction);
        *component = u8::try_from(value).expect("weighted RGB value should fit u8");
    }

    pixel
}

fn iw44_scaler_coordinate(input_size: usize, subsample: u32, output_coordinate: u32) -> i32 {
    let input = i64::try_from(input_size).expect("IW44 image dimension should fit i64");
    let numer = i64::from(subsample.max(1));
    let len = 16i64;
    let coordinate = ((len + numer) / (2 * numer)) - 8
        + ((numer / 2) + (i64::from(output_coordinate) * len)) / numer;
    let max_coordinate = input.saturating_sub(1).saturating_mul(16);
    i32::try_from(coordinate.min(max_coordinate)).expect("IW44 scaler coordinate should fit i32")
}

fn iw44_top_down_scaler_coordinate(
    input_size: usize,
    mapping: &Iw44PageMapping,
    top_down_coordinate: u32,
) -> i32 {
    let bottom_up_coordinate = mapping
        .page_height
        .saturating_sub(1)
        .saturating_sub(top_down_coordinate);
    let bottom_up_fixed =
        iw44_scaler_coordinate(input_size, mapping.subsample, bottom_up_coordinate);
    let max_fixed = i32::try_from(input_size.saturating_sub(1).saturating_mul(16))
        .expect("IW44 scaler coordinate should fit i32");
    max_fixed - bottom_up_fixed
}

fn iw44_scaler_index_and_fraction(fixed_coordinate: i32) -> (i32, u16) {
    let index = fixed_coordinate.div_euclid(16);
    let fraction = fixed_coordinate.rem_euclid(16);
    let fraction = u16::try_from(fraction).expect("fixed-point fraction should fit u16");
    (index, fraction)
}

fn iw44_interpolate_fixed(first: u16, second: u16, fraction: u16) -> u16 {
    let delta = i32::from(second) - i32::from(first);
    let rounded_delta = ((delta * i32::from(fraction)) + 8) >> 4;
    u16::try_from(i32::from(first) + rounded_delta)
        .expect("interpolated RGB channel should fit u16")
}

fn iw44_native_pixel(image: &Iw44RgbImage, x: usize, y: usize) -> [u8; 3] {
    let offset = (y * image.width + x) * 3;
    [
        image.pixels[offset],
        image.pixels[offset + 1],
        image.pixels[offset + 2],
    ]
}

fn iw44_native_channel_at(image: &Iw44RgbImage, x: i32, y: i32, channel: usize) -> u8 {
    let x = usize::try_from(x)
        .unwrap_or(0)
        .min(image.width.saturating_sub(1));
    let y = usize::try_from(y)
        .unwrap_or(0)
        .min(image.height.saturating_sub(1));
    image.pixels[(y * image.width + x) * 3 + channel]
}

#[derive(Debug, Clone)]
pub struct PageRenderPlan<'a> {
    pub info: PageInfo,
    pub chunks: Vec<ResolvedPageChunk<'a>>,
    pub bitonal_dictionaries: Vec<usize>,
    pub bitonal_images: Vec<usize>,
    pub foreground_layers: Vec<usize>,
    pub background_layers: Vec<usize>,
    pub text_chunks: Vec<usize>,
    pub unknown_chunks: Vec<usize>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RenderChunkPayload<'a> {
    pub index: usize,
    pub bytes: &'a [u8],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BitonalImageHeader {
    pub index: usize,
    pub header: Jb2ImageHeader,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44LayerGeometry {
    pub summary: Iw44LayerSummary,
    pub mapping: Iw44PageMapping,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Iw44LayerRole {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PageRenderMode {
    Full,
    Background,
    Foreground,
    Mask,
}

impl PageRenderMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Background => "background",
            Self::Foreground => "foreground",
            Self::Mask => "mask",
        }
    }
}

impl FromStr for PageRenderMode {
    type Err = RenderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "full" => Ok(Self::Full),
            "background" => Ok(Self::Background),
            "foreground" => Ok(Self::Foreground),
            "mask" => Ok(Self::Mask),
            _ => Err(RenderError::new(format!(
                "unknown render mode {value:?}; expected full, background, foreground, or mask"
            ))),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RenderedIw44Layer {
    pub role: Iw44LayerRole,
    pub chunk_indices: Vec<usize>,
    pub geometry: Iw44LayerGeometry,
    pub image: Iw44RgbImage,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PartialPageRender {
    pub bitmap: PageBitmap,
    pub iw44_layers: Vec<RenderedIw44Layer>,
    pub bitonal_masks: Vec<(usize, Jb2PartialImage)>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RenderedDocumentPage {
    pub page_number: usize,
    pub render: PartialPageRender,
}

impl<'a> PageRenderPlan<'a> {
    #[must_use]
    pub fn new(info: PageInfo, chunks: Vec<ResolvedPageChunk<'a>>) -> Self {
        let mut plan = Self {
            info,
            chunks,
            bitonal_dictionaries: Vec::new(),
            bitonal_images: Vec::new(),
            foreground_layers: Vec::new(),
            background_layers: Vec::new(),
            text_chunks: Vec::new(),
            unknown_chunks: Vec::new(),
        };

        for (index, chunk) in plan.chunks.iter().enumerate() {
            match chunk.chunk.kind {
                PageChunkKind::Djbz => plan.bitonal_dictionaries.push(index),
                PageChunkKind::Sjbz => plan.bitonal_images.push(index),
                PageChunkKind::Fg44 => plan.foreground_layers.push(index),
                PageChunkKind::Bg44 => plan.background_layers.push(index),
                PageChunkKind::Txta | PageChunkKind::Txtz => plan.text_chunks.push(index),
                PageChunkKind::Unknown => plan.unknown_chunks.push(index),
                PageChunkKind::Info | PageChunkKind::Include | PageChunkKind::Cida => {}
            }
        }

        plan
    }

    #[must_use]
    pub const fn has_image_data(&self) -> bool {
        !self.bitonal_images.is_empty()
            || !self.foreground_layers.is_empty()
            || !self.background_layers.is_empty()
    }

    #[must_use]
    pub const fn has_text(&self) -> bool {
        !self.text_chunks.is_empty()
    }

    #[must_use]
    pub fn chunk(&self, index: usize) -> Option<&ResolvedPageChunk<'a>> {
        self.chunks.get(index)
    }

    #[must_use]
    pub fn chunk_payload<'bytes>(&self, bytes: &'bytes [u8], index: usize) -> Option<&'bytes [u8]> {
        let chunk = &self.chunk(index)?.chunk.chunk;
        bytes.get(chunk.data_start..chunk.data_end)
    }

    #[must_use]
    pub fn bitonal_dictionary_payloads<'bytes>(
        &self,
        bytes: &'bytes [u8],
    ) -> Vec<RenderChunkPayload<'bytes>> {
        self.payloads_for_indices(bytes, &self.bitonal_dictionaries)
    }

    #[must_use]
    pub fn bitonal_image_payloads<'bytes>(
        &self,
        bytes: &'bytes [u8],
    ) -> Vec<RenderChunkPayload<'bytes>> {
        self.payloads_for_indices(bytes, &self.bitonal_images)
    }

    #[must_use]
    pub fn foreground_layer_payloads<'bytes>(
        &self,
        bytes: &'bytes [u8],
    ) -> Vec<RenderChunkPayload<'bytes>> {
        self.payloads_for_indices(bytes, &self.foreground_layers)
    }

    #[must_use]
    pub fn background_layer_payloads<'bytes>(
        &self,
        bytes: &'bytes [u8],
    ) -> Vec<RenderChunkPayload<'bytes>> {
        self.payloads_for_indices(bytes, &self.background_layers)
    }

    /// Reads and validates the progressive foreground IW44 layer, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if any `FG44` chunk header is malformed or the serial
    /// sequence is not contiguous.
    pub fn foreground_layer_summary(
        &self,
        bytes: &[u8],
    ) -> Result<Option<Iw44LayerSummary>, Iw44Error> {
        let payloads = self.foreground_layer_payloads(bytes);
        if payloads.is_empty() {
            return Ok(None);
        }

        summarize_iw44_layer(payloads.iter().map(|payload| payload.bytes)).map(Some)
    }

    /// Reads and validates the progressive background IW44 layer, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if any `BG44` chunk header is malformed or the serial
    /// sequence is not contiguous.
    pub fn background_layer_summary(
        &self,
        bytes: &[u8],
    ) -> Result<Option<Iw44LayerSummary>, Iw44Error> {
        let payloads = self.background_layer_payloads(bytes);
        if payloads.is_empty() {
            return Ok(None);
        }

        summarize_iw44_layer(payloads.iter().map(|payload| payload.bytes)).map(Some)
    }

    /// Reads the foreground IW44 layer and maps it into page space.
    ///
    /// # Errors
    ///
    /// Returns an error if the `FG44` layer summary cannot be decoded.
    pub fn foreground_layer_geometry(
        &self,
        bytes: &[u8],
    ) -> Result<Option<Iw44LayerGeometry>, Iw44Error> {
        Ok(self.foreground_layer_summary(bytes)?.map(|summary| {
            let mapping =
                summary.page_mapping(u32::from(self.info.width), u32::from(self.info.height));
            Iw44LayerGeometry { summary, mapping }
        }))
    }

    /// Reads the background IW44 layer and maps it into page space.
    ///
    /// # Errors
    ///
    /// Returns an error if the `BG44` layer summary cannot be decoded.
    pub fn background_layer_geometry(
        &self,
        bytes: &[u8],
    ) -> Result<Option<Iw44LayerGeometry>, Iw44Error> {
        Ok(self.background_layer_summary(bytes)?.map(|summary| {
            let mapping =
                summary.page_mapping(u32::from(self.info.width), u32::from(self.info.height));
            Iw44LayerGeometry { summary, mapping }
        }))
    }

    /// Decodes the foreground IW44 layer, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the progressive IW44 layer is malformed.
    pub fn foreground_iw44_layer(&self, bytes: &[u8]) -> RenderResult<Option<RenderedIw44Layer>> {
        let payloads = self.foreground_layer_payloads(bytes);
        if payloads.is_empty() {
            return Ok(None);
        }
        let Some(geometry) = self.foreground_layer_geometry(bytes)? else {
            return Err(RenderError::new(
                "foreground IW44 payloads did not produce layer geometry",
            ));
        };

        decode_iw44_layer(Iw44LayerRole::Foreground, &payloads, geometry).map(Some)
    }

    /// Decodes the background IW44 layer, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the progressive IW44 layer is malformed.
    pub fn background_iw44_layer(&self, bytes: &[u8]) -> RenderResult<Option<RenderedIw44Layer>> {
        let payloads = self.background_layer_payloads(bytes);
        if payloads.is_empty() {
            return Ok(None);
        }
        let Some(geometry) = self.background_layer_geometry(bytes)? else {
            return Err(RenderError::new(
                "background IW44 payloads did not produce layer geometry",
            ));
        };

        decode_iw44_layer(Iw44LayerRole::Background, &payloads, geometry).map(Some)
    }

    fn payloads_for_indices<'bytes>(
        &self,
        bytes: &'bytes [u8],
        indices: &[usize],
    ) -> Vec<RenderChunkPayload<'bytes>> {
        indices
            .iter()
            .filter_map(|index| {
                self.chunk_payload(bytes, *index)
                    .map(|payload| RenderChunkPayload {
                        index: *index,
                        bytes: payload,
                    })
            })
            .collect()
    }

    /// Reads JB2 headers for each `Sjbz` bitonal image payload.
    ///
    /// # Errors
    ///
    /// Returns an error if any `Sjbz` payload has an invalid JB2 image header.
    pub fn bitonal_image_headers(&self, bytes: &[u8]) -> Result<Vec<BitonalImageHeader>, Jb2Error> {
        self.bitonal_image_payloads(bytes)
            .into_iter()
            .map(|payload| {
                read_jb2_image_header(payload.bytes).map(|header| BitonalImageHeader {
                    index: payload.index,
                    header,
                })
            })
            .collect()
    }

    /// Reads a bounded JB2 record prefix for each `Sjbz` bitonal image payload.
    ///
    /// # Errors
    ///
    /// Returns an error if any `Sjbz` payload has an invalid JB2 header or
    /// malformed supported prefix record.
    pub fn bitonal_record_prefixes(
        &self,
        bytes: &[u8],
        max_records: usize,
    ) -> Result<Vec<(usize, Jb2RecordPrefix)>, Jb2Error> {
        self.bitonal_image_payloads(bytes)
            .into_iter()
            .map(|payload| {
                read_jb2_record_prefix(payload.bytes, max_records)
                    .map(|prefix| (payload.index, prefix))
            })
            .collect()
    }

    /// Decodes and paints the supported JB2 record prefix for each `Sjbz`
    /// payload, stopping before the first unsupported reset/dictionary record.
    ///
    /// # Errors
    ///
    /// Returns an error if any `Sjbz` payload has an invalid JB2 header or
    /// malformed supported prefix record.
    pub fn partial_bitonal_masks(
        &self,
        bytes: &[u8],
        max_records: usize,
    ) -> Result<Vec<(usize, Jb2PartialImage)>, Jb2Error> {
        self.bitonal_image_payloads(bytes)
            .into_iter()
            .map(|payload| {
                render_jb2_supported_prefix(payload.bytes, max_records)
                    .map(|partial| (payload.index, partial))
            })
            .collect()
    }

    /// Decodes and paints complete JB2 masks for each `Sjbz` bitonal image
    /// payload.
    ///
    /// # Errors
    ///
    /// Returns an error if any `Sjbz` payload has an invalid or unsupported JB2
    /// image stream.
    pub fn bitonal_masks(&self, bytes: &[u8]) -> Result<Vec<(usize, Jb2PartialImage)>, Jb2Error> {
        let dictionary = self.bitonal_dictionary(bytes)?;
        self.bitonal_image_payloads(bytes)
            .into_iter()
            .map(|payload| {
                render_jb2_image_with_optional_dictionary(payload.bytes, dictionary.as_ref())
                    .map(|image| (payload.index, image))
            })
            .collect()
    }

    fn bitonal_dictionary(&self, bytes: &[u8]) -> Result<Option<Jb2Dictionary>, Jb2Error> {
        let mut dictionary = None;
        for payload in self.bitonal_dictionary_payloads(bytes) {
            dictionary = Some(decode_jb2_dictionary(payload.bytes)?);
        }

        Ok(dictionary)
    }

    #[must_use]
    pub fn render_base_bitmap(&self) -> PageBitmap {
        PageBitmap::white_rgb8(&self.info)
    }

    /// Renders a selected view of the currently supported page layers.
    ///
    /// # Errors
    ///
    /// Returns an error if any selected image layer is malformed or cannot be
    /// mapped into page space.
    pub fn render_bitmap_with_mode(
        &self,
        bytes: &[u8],
        mode: PageRenderMode,
    ) -> RenderResult<PartialPageRender> {
        match mode {
            PageRenderMode::Full => self.render_partial_bitmap(bytes),
            PageRenderMode::Background => self.render_background_bitmap(bytes),
            PageRenderMode::Foreground => self.render_foreground_bitmap(bytes),
            PageRenderMode::Mask => self.render_mask_bitmap(bytes),
        }
    }

    /// Renders the currently supported page layers into an RGB bitmap.
    ///
    /// This paints decoded `BG44` IW44 background pixels first, then complete
    /// `Sjbz` JB2 masks over the result. When an `FG44` layer is present, mask
    /// pixels receive the corresponding foreground IW44 color; otherwise they
    /// are painted black.
    ///
    /// # Errors
    ///
    /// Returns an error if any supported image layer is malformed or cannot be
    /// mapped into page space.
    pub fn render_partial_bitmap(&self, bytes: &[u8]) -> RenderResult<PartialPageRender> {
        let mut iw44_layers = Vec::with_capacity(2);
        let mut bitmap = self.render_base_bitmap();

        if let Some(background) = self.background_iw44_layer(bytes)? {
            if !bitmap.paint_iw44_rgb_layer(&background.image, &background.geometry.mapping) {
                return Err(RenderError::new(format!(
                    "background IW44 layer dimensions {}x{} do not map to page {}x{}",
                    background.image.width, background.image.height, bitmap.width, bitmap.height
                )));
            }
            iw44_layers.push(background);
        }

        let foreground = self.foreground_iw44_layer(bytes)?;
        let bitonal_masks = self.bitonal_masks(bytes)?;
        for (chunk_index, partial) in &bitonal_masks {
            let painted = if let Some(foreground) = &foreground {
                bitmap.paint_iw44_rgb_layer_through_mask(
                    &foreground.image,
                    &foreground.geometry.mapping,
                    &partial.mask,
                )
            } else {
                bitmap.paint_bitonal_mask(&partial.mask, [0, 0, 0])
            };

            if !painted {
                return Err(RenderError::new(format!(
                    "bitonal image #{chunk_index} dimensions {}x{} do not match page {}x{}",
                    partial.mask.width, partial.mask.height, bitmap.width, bitmap.height
                )));
            }
        }
        if let Some(foreground) = foreground {
            iw44_layers.push(foreground);
        }

        Ok(PartialPageRender {
            bitmap,
            iw44_layers,
            bitonal_masks,
        })
    }

    /// Renders only the decoded `BG44` background layer, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the background layer is malformed or cannot be
    /// mapped into page space.
    pub fn render_background_bitmap(&self, bytes: &[u8]) -> RenderResult<PartialPageRender> {
        let mut bitmap = self.render_base_bitmap();
        let mut iw44_layers = Vec::new();
        if let Some(background) = self.background_iw44_layer(bytes)? {
            if !bitmap.paint_iw44_rgb_layer(&background.image, &background.geometry.mapping) {
                return Err(RenderError::new(format!(
                    "background IW44 layer dimensions {}x{} do not map to page {}x{}",
                    background.image.width, background.image.height, bitmap.width, bitmap.height
                )));
            }
            iw44_layers.push(background);
        }

        Ok(PartialPageRender {
            bitmap,
            iw44_layers,
            bitonal_masks: Vec::new(),
        })
    }

    /// Renders foreground content over a white page.
    ///
    /// This paints `FG44` colors through the decoded `Sjbz` masks when a
    /// foreground IW44 layer is present, otherwise it paints the masks black.
    ///
    /// # Errors
    ///
    /// Returns an error if a selected layer is malformed or cannot be mapped
    /// into page space.
    pub fn render_foreground_bitmap(&self, bytes: &[u8]) -> RenderResult<PartialPageRender> {
        let mut bitmap = self.render_base_bitmap();
        let mut iw44_layers = Vec::new();
        let bitonal_masks = self.bitonal_masks(bytes)?;
        if let Some(foreground) = self.foreground_iw44_layer(bytes)? {
            for (chunk_index, partial) in &bitonal_masks {
                if !bitmap.paint_iw44_rgb_layer_through_mask(
                    &foreground.image,
                    &foreground.geometry.mapping,
                    &partial.mask,
                ) {
                    return Err(RenderError::new(format!(
                        "bitonal image #{chunk_index} dimensions {}x{} do not match page {}x{}",
                        partial.mask.width, partial.mask.height, bitmap.width, bitmap.height
                    )));
                }
            }
            iw44_layers.push(foreground);
        } else {
            for (chunk_index, partial) in &bitonal_masks {
                if !bitmap.paint_bitonal_mask(&partial.mask, [0, 0, 0]) {
                    return Err(RenderError::new(format!(
                        "bitonal image #{chunk_index} dimensions {}x{} do not match page {}x{}",
                        partial.mask.width, partial.mask.height, bitmap.width, bitmap.height
                    )));
                }
            }
        }

        Ok(PartialPageRender {
            bitmap,
            iw44_layers,
            bitonal_masks,
        })
    }

    /// Renders decoded `Sjbz` masks as black pixels over a white page.
    ///
    /// # Errors
    ///
    /// Returns an error if any selected bitonal image is malformed or does not
    /// match the page dimensions.
    pub fn render_mask_bitmap(&self, bytes: &[u8]) -> RenderResult<PartialPageRender> {
        let bitonal_masks = self.bitonal_masks(bytes)?;
        let mut bitmap = self.render_base_bitmap();
        for (chunk_index, partial) in &bitonal_masks {
            if !bitmap.paint_bitonal_mask(&partial.mask, [0, 0, 0]) {
                return Err(RenderError::new(format!(
                    "bitonal image #{chunk_index} dimensions {}x{} do not match page {}x{}",
                    partial.mask.width, partial.mask.height, bitmap.width, bitmap.height
                )));
            }
        }

        Ok(PartialPageRender {
            bitmap,
            iw44_layers: Vec::new(),
            bitonal_masks,
        })
    }
}

fn decode_iw44_layer(
    role: Iw44LayerRole,
    payloads: &[RenderChunkPayload<'_>],
    geometry: Iw44LayerGeometry,
) -> RenderResult<RenderedIw44Layer> {
    let chunk_indices = payloads
        .iter()
        .map(|payload| payload.index)
        .collect::<Vec<_>>();
    let image = decode_iw44_rgb_image(payloads.iter().map(|payload| payload.bytes))?;

    Ok(RenderedIw44Layer {
        role,
        chunk_indices,
        geometry,
        image,
    })
}

fn decode_iw44_rgb_image<'a, I>(payloads: I) -> Iw44Result<Iw44RgbImage>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut decoder = Iw44Decoder::new();
    for payload in payloads {
        decoder.decode_chunk(payload)?;
    }
    decoder.to_rgb_image()
}

fn render_jb2_image_with_optional_dictionary(
    bytes: &[u8],
    dictionary: Option<&Jb2Dictionary>,
) -> Result<Jb2PartialImage, Jb2Error> {
    dictionary.map_or_else(
        || render_jb2_image(bytes),
        |dictionary| render_jb2_image_with_dictionary(bytes, dictionary),
    )
}

/// Renders one page from a `DjVu` document byte slice.
///
/// Page numbers are 1-based. The selected [`PageRenderMode`] controls whether
/// the full compositor output or one of the supported layer views is rendered.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page number is invalid, the page cannot be planned,
/// or the selected image layers cannot be rendered.
pub fn render_document_page(
    bytes: &[u8],
    page_number: usize,
    mode: PageRenderMode,
) -> DjvuRenderResult<PartialPageRender> {
    if page_number == 0 {
        return Err(DjvuRenderError::ZeroPage);
    }

    let mut rendered = None;
    render_document_page_range(
        bytes,
        page_number,
        Some(page_number),
        mode,
        |_, _, render| {
            rendered = Some(render?);
            Ok(())
        },
    )?;

    rendered.ok_or_else(|| {
        RenderError::new("validated page range did not render the requested page").into()
    })
}

/// Renders a page range from a `DjVu` document byte slice.
///
/// Page numbers are 1-based. If `to_page` is `None`, rendering continues
/// through the final page.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page range is invalid, a selected page cannot be
/// planned, or the selected image layers cannot be rendered.
pub fn render_document_pages(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
    mode: PageRenderMode,
) -> DjvuRenderResult<Vec<RenderedDocumentPage>> {
    render_document_pages_with_events(bytes, from_page, to_page, mode, |_| {})
}

/// Renders a page range from a `DjVu` document byte slice, reporting events.
///
/// The event callback is invoked before each selected page is rendered and
/// after the page's compositor output is available. The rendered page reference
/// passed to `PageRendered` is valid only for the duration of the callback.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page range is invalid, a selected page cannot be
/// planned, or the selected image layers cannot be rendered.
pub fn render_document_pages_with_events(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
    mode: PageRenderMode,
    mut event: impl FnMut(DjvuPageRenderEvent<'_>),
) -> DjvuRenderResult<Vec<RenderedDocumentPage>> {
    let mut pages = Vec::new();
    render_document_page_range(
        bytes,
        from_page,
        to_page,
        mode,
        |page_number, end_page, render| {
            event(DjvuPageRenderEvent::PageStarted {
                page_number,
                end_page,
            });
            let render = render?;
            event(DjvuPageRenderEvent::PageRendered {
                page_number,
                render: &render,
            });
            pages.push(RenderedDocumentPage {
                page_number,
                render,
            });

            Ok(())
        },
    )?;

    Ok(pages)
}

fn render_document_page_range(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
    mode: PageRenderMode,
    mut render: impl FnMut(usize, usize, RenderResult<PartialPageRender>) -> DjvuRenderResult<()>,
) -> DjvuRenderResult<()> {
    let document = Document::parse(bytes)?;
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(bytes, dirm)?;
        parse_dirm_tail(dirm, &decoded_tail)?
    } else {
        Vec::new()
    };
    let page_count = document.form_kind_counts().pages;
    let end_page = checked_document_render_page_range(page_count, from_page, to_page)?;
    let mut rendered_page_count = 0usize;

    for (index, page) in document.pages(bytes).enumerate() {
        let page_number = index + 1;
        if page_number < from_page || page_number > end_page {
            continue;
        }

        let page = page?;
        let plan = document.page_render_plan(bytes, &page, &tail_entries)?;
        render(
            page_number,
            end_page,
            plan.render_bitmap_with_mode(bytes, mode),
        )?;
        rendered_page_count += 1;
    }

    if rendered_page_count == 0 {
        return Err(DjvuRenderError::PageOutOfRange {
            page: from_page,
            page_count,
        });
    }

    Ok(())
}

fn checked_document_render_page_range(
    page_count: usize,
    from_page: usize,
    to_page: Option<usize>,
) -> DjvuRenderResult<usize> {
    if from_page == 0 {
        return Err(DjvuRenderError::ZeroFromPage);
    }
    if let Some(to_page) = to_page
        && to_page < from_page
    {
        return Err(DjvuRenderError::ReversedPageRange);
    }

    let end_page = to_page.unwrap_or(page_count);
    if from_page > page_count {
        return Err(DjvuRenderError::PageOutOfRange {
            page: from_page,
            page_count,
        });
    }
    if end_page > page_count {
        return Err(DjvuRenderError::PageOutOfRange {
            page: end_page,
            page_count,
        });
    }

    Ok(end_page)
}

impl<'a> Document<'a> {
    /// Builds a renderer-facing view of a page with shared includes expanded and
    /// effective chunks classified by image/text role.
    ///
    /// # Errors
    ///
    /// Returns an error if the page has no `INFO` metadata, if page chunks are
    /// malformed, or if shared includes cannot be resolved.
    pub fn page_render_plan<'tail>(
        &'a self,
        bytes: &'a [u8],
        page: &Page<'a>,
        tail_entries: &'tail [DirmTailEntry<'tail>],
    ) -> ParseResult<PageRenderPlan<'a>> {
        let Some(info) = page.info.clone() else {
            return Err(ParseError(format!(
                "page at offset {} has no INFO metadata",
                page.offset
            )));
        };
        let chunks = self.resolved_page_chunks(bytes, page, tail_entries)?;

        Ok(PageRenderPlan::new(info, chunks))
    }
}

#[cfg(test)]
mod tests;
