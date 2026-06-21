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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBitmapDiff {
    pub width: u32,
    pub height: u32,
    pub compared_pixels: usize,
    pub exact_pixels: usize,
    pub differing_pixels: usize,
    pub total_abs_delta: u64,
    pub max_abs_delta: u8,
    pub max_delta_pixels: usize,
    pub mean_abs_delta: f64,
    pub channels: [PageBitmapChannelDiff; 3],
    pub bounds: Option<PageBitmapDiffBounds>,
    pub first_difference: Option<PageBitmapDiffPixel>,
    pub max_difference: Option<PageBitmapDiffPixel>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderCompareLimits {
    pub different_pixels: usize,
    pub abs_delta: u8,
    pub delta_pixels: Option<usize>,
    pub mean_abs_delta: f64,
}

impl RenderCompareLimits {
    #[must_use]
    pub const fn new(
        different_pixels: usize,
        abs_delta: u8,
        delta_pixels: Option<usize>,
        mean_abs_delta: f64,
    ) -> Self {
        Self {
            different_pixels,
            abs_delta,
            delta_pixels,
            mean_abs_delta,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBitmapChannelDiff {
    pub total_abs_delta: u64,
    pub signed_delta: i64,
    pub max_abs_delta: u8,
    pub mean_abs_delta: f64,
    pub mean_signed_delta: f64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PageBitmapDiffBounds {
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PageBitmapDiffPixel {
    pub x: u32,
    pub y: u32,
    pub actual: [u8; 3],
    pub expected: [u8; 3],
    pub abs_delta_sum: u16,
    pub max_abs_delta: u8,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct PageBitmapDiffRegionSummary {
    pub pixels: usize,
    pub differing_pixels: usize,
    pub total_abs_delta: u64,
    pub max_abs_delta: u8,
    pub max_delta_pixels: usize,
    pub actual_black_pixels: usize,
    pub expected_black_pixels: usize,
    pub actual_white_pixels: usize,
    pub expected_white_pixels: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PageBitmapDiffTileSummary {
    pub bounds: PageBitmapDiffBounds,
    pub summary: PageBitmapDiffRegionSummary,
}

#[must_use]
pub fn bitmap_diff_failures(diff: &PageBitmapDiff, limits: RenderCompareLimits) -> Vec<String> {
    let mut failures = Vec::new();
    if diff.differing_pixels > limits.different_pixels {
        failures.push(format!(
            "render differs in {} pixels; allowed {}",
            diff.differing_pixels, limits.different_pixels
        ));
    }
    if diff.max_abs_delta > limits.abs_delta {
        failures.push(format!(
            "render max absolute delta is {}; allowed {}",
            diff.max_abs_delta, limits.abs_delta
        ));
    }
    if let Some(delta_pixels) = limits.delta_pixels
        && diff.max_delta_pixels > delta_pixels
    {
        failures.push(format!(
            "render has {} pixels at max absolute delta; allowed {delta_pixels}",
            diff.max_delta_pixels
        ));
    }
    if diff.mean_abs_delta > limits.mean_abs_delta {
        failures.push(format!(
            "render mean absolute delta is {:.6}; allowed {:.6}",
            diff.mean_abs_delta, limits.mean_abs_delta
        ));
    }

    failures
}

#[must_use]
pub fn bitmap_diff_region_summary(
    actual: &PageBitmap,
    expected: &PageBitmap,
    bounds: PageBitmapDiffBounds,
) -> Option<PageBitmapDiffRegionSummary> {
    if actual.width != expected.width || actual.height != expected.height {
        return None;
    }
    if bounds.max_x >= actual.width || bounds.max_y >= actual.height {
        return None;
    }

    let mut summary = PageBitmapDiffRegionSummary::default();
    for y in bounds.min_y..=bounds.max_y {
        for x in bounds.min_x..=bounds.max_x {
            let actual_offset = actual.pixel_offset(x, y)?;
            let expected_offset = expected.pixel_offset(x, y)?;
            let actual_pixel = &actual.pixels[actual_offset..actual_offset + 3];
            let expected_pixel = &expected.pixels[expected_offset..expected_offset + 3];
            summary.pixels += 1;
            summary.actual_black_pixels += usize::from(actual_pixel == [0, 0, 0]);
            summary.expected_black_pixels += usize::from(expected_pixel == [0, 0, 0]);
            summary.actual_white_pixels += usize::from(actual_pixel == [0xff, 0xff, 0xff]);
            summary.expected_white_pixels += usize::from(expected_pixel == [0xff, 0xff, 0xff]);

            let mut pixel_max_delta = 0u8;
            for (actual, expected) in actual_pixel.iter().zip(expected_pixel.iter()) {
                let delta = actual.abs_diff(*expected);
                summary.total_abs_delta += u64::from(delta);
                pixel_max_delta = pixel_max_delta.max(delta);
            }
            if pixel_max_delta != 0 {
                summary.differing_pixels += 1;
            }
            if pixel_max_delta > summary.max_abs_delta {
                summary.max_abs_delta = pixel_max_delta;
                summary.max_delta_pixels = 1;
            } else if pixel_max_delta == summary.max_abs_delta {
                summary.max_delta_pixels += 1;
            }
        }
    }

    Some(summary)
}

#[must_use]
pub fn bitmap_diff_tile_summaries(
    actual: &PageBitmap,
    expected: &PageBitmap,
    tile_width: u32,
    tile_height: u32,
) -> Option<Vec<PageBitmapDiffTileSummary>> {
    if actual.width != expected.width
        || actual.height != expected.height
        || actual.width == 0
        || actual.height == 0
        || tile_width == 0
        || tile_height == 0
    {
        return None;
    }

    let mut tiles = Vec::new();
    for min_y in (0..actual.height).step_by(usize::try_from(tile_height).ok()?) {
        for min_x in (0..actual.width).step_by(usize::try_from(tile_width).ok()?) {
            let bounds = PageBitmapDiffBounds {
                min_x,
                min_y,
                max_x: min_x.saturating_add(tile_width - 1).min(actual.width - 1),
                max_y: min_y.saturating_add(tile_height - 1).min(actual.height - 1),
            };
            let summary = bitmap_diff_region_summary(actual, expected, bounds)?;
            if summary.differing_pixels != 0 {
                tiles.push(PageBitmapDiffTileSummary { bounds, summary });
            }
        }
    }

    Some(tiles)
}

impl PageBitmapDiffBounds {
    #[must_use]
    pub const fn width(self) -> u32 {
        self.max_x - self.min_x + 1
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.max_y - self.min_y + 1
    }
}

fn page_bitmap_diff_pixel(
    index: usize,
    width: usize,
    actual: &[u8],
    expected: &[u8],
) -> RenderResult<PageBitmapDiffPixel> {
    let x = u32::try_from(index % width)
        .map_err(|_| RenderError::new("bitmap x coordinate exceeds decoder range"))?;
    let y = u32::try_from(index / width)
        .map_err(|_| RenderError::new("bitmap y coordinate exceeds decoder range"))?;
    let mut abs_delta_sum = 0u16;
    let mut max_abs_delta = 0u8;

    for (actual, expected) in actual.iter().zip(expected.iter()) {
        let delta = actual.abs_diff(*expected);
        abs_delta_sum += u16::from(delta);
        max_abs_delta = max_abs_delta.max(delta);
    }

    Ok(PageBitmapDiffPixel {
        x,
        y,
        actual: [actual[0], actual[1], actual[2]],
        expected: [expected[0], expected[1], expected[2]],
        abs_delta_sum,
        max_abs_delta,
    })
}

const fn should_replace_max_diff(
    current: Option<PageBitmapDiffPixel>,
    candidate: PageBitmapDiffPixel,
) -> bool {
    match current {
        Some(current) => {
            candidate.abs_delta_sum > current.abs_delta_sum
                || (candidate.abs_delta_sum == current.abs_delta_sum
                    && candidate.max_abs_delta > current.max_abs_delta)
        }
        None => true,
    }
}

fn expand_diff_bounds(
    bounds: Option<PageBitmapDiffBounds>,
    pixel: PageBitmapDiffPixel,
) -> PageBitmapDiffBounds {
    bounds.map_or(
        PageBitmapDiffBounds {
            min_x: pixel.x,
            min_y: pixel.y,
            max_x: pixel.x,
            max_y: pixel.y,
        },
        |bounds| PageBitmapDiffBounds {
            min_x: bounds.min_x.min(pixel.x),
            min_y: bounds.min_y.min(pixel.y),
            max_x: bounds.max_x.max(pixel.x),
            max_y: bounds.max_y.max(pixel.y),
        },
    )
}

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

                let source_x = iw44_source_coordinate(
                    x,
                    mapping.horizontal_overscan,
                    mapping.subsample,
                    image.width,
                );
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

    /// Compares two RGB bitmaps with identical dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if the bitmaps differ in dimensions or pixel buffer size.
    pub fn diff(&self, expected: &Self) -> RenderResult<PageBitmapDiff> {
        if self.width != expected.width || self.height != expected.height {
            return Err(RenderError::new(format!(
                "bitmap dimensions {}x{} do not match expected {}x{}",
                self.width, self.height, expected.width, expected.height
            )));
        }
        if self.pixels.len() != expected.pixels.len() || !self.pixels.len().is_multiple_of(3) {
            return Err(RenderError::new("bitmap pixel buffers are incompatible"));
        }
        let expected_len = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| RenderError::new("bitmap dimensions overflow"))?;
        if self.pixels.len() != expected_len {
            return Err(RenderError::new(
                "bitmap pixel buffer does not match dimensions",
            ));
        }

        let mut exact_pixels = 0;
        let mut differing_pixels = 0;
        let mut total_abs_delta = 0u64;
        let mut max_abs_delta = 0u8;
        let mut max_delta_pixels = 0usize;
        let mut channel_total_abs_delta = [0u64; 3];
        let mut channel_signed_delta = [0i64; 3];
        let mut channel_max_abs_delta = [0u8; 3];
        let mut bounds: Option<PageBitmapDiffBounds> = None;
        let mut first_difference = None;
        let mut max_difference = None;
        let width = usize::try_from(self.width)
            .map_err(|_| RenderError::new("bitmap width exceeds platform range"))?;

        for (index, (actual, expected)) in self
            .pixels
            .chunks_exact(3)
            .zip(expected.pixels.chunks_exact(3))
            .enumerate()
        {
            if actual == expected {
                exact_pixels += 1;
            } else {
                differing_pixels += 1;
                let pixel = page_bitmap_diff_pixel(index, width, actual, expected)?;
                if pixel.max_abs_delta > max_abs_delta {
                    max_abs_delta = pixel.max_abs_delta;
                    max_delta_pixels = 1;
                } else if pixel.max_abs_delta == max_abs_delta {
                    max_delta_pixels += 1;
                }

                first_difference.get_or_insert(pixel);
                if should_replace_max_diff(max_difference, pixel) {
                    max_difference = Some(pixel);
                }

                bounds = Some(expand_diff_bounds(bounds, pixel));
            }

            for (channel, (actual, expected)) in actual.iter().zip(expected.iter()).enumerate() {
                let delta = actual.abs_diff(*expected);
                total_abs_delta += u64::from(delta);
                channel_total_abs_delta[channel] += u64::from(delta);
                channel_signed_delta[channel] += i64::from(*actual) - i64::from(*expected);
                channel_max_abs_delta[channel] = channel_max_abs_delta[channel].max(delta);
            }
        }

        let component_count = self.pixels.len();
        let mean_abs_delta = if component_count == 0 {
            0.0
        } else {
            mean_delta(total_abs_delta, component_count)
        };
        let channels = std::array::from_fn(|channel| PageBitmapChannelDiff {
            total_abs_delta: channel_total_abs_delta[channel],
            signed_delta: channel_signed_delta[channel],
            max_abs_delta: channel_max_abs_delta[channel],
            mean_abs_delta: if self.pixels.is_empty() {
                0.0
            } else {
                mean_delta(channel_total_abs_delta[channel], self.pixels.len() / 3)
            },
            mean_signed_delta: if self.pixels.is_empty() {
                0.0
            } else {
                mean_signed_delta(channel_signed_delta[channel], self.pixels.len() / 3)
            },
        });

        Ok(PageBitmapDiff {
            width: self.width,
            height: self.height,
            compared_pixels: self.pixels.len() / 3,
            exact_pixels,
            differing_pixels,
            total_abs_delta,
            max_abs_delta,
            max_delta_pixels,
            mean_abs_delta,
            channels,
            bounds,
            first_difference,
            max_difference,
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

#[expect(
    clippy::cast_precision_loss,
    reason = "bitmap comparison reports a human-readable floating-point mean"
)]
fn mean_delta(total_abs_delta: u64, component_count: usize) -> f64 {
    total_abs_delta as f64 / component_count as f64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "bitmap comparison reports a human-readable floating-point mean"
)]
fn mean_signed_delta(signed_delta: i64, component_count: usize) -> f64 {
    signed_delta as f64 / component_count as f64
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
    if mapping.subsample == 2 {
        return iw44_upsampled_2x_pixel(
            image,
            x.saturating_add(mapping.horizontal_overscan),
            y.saturating_add(mapping.vertical_overscan),
        );
    }

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
    iw44_native_pixel(image, source_x, source_y)
}

fn iw44_upsampled_2x_pixel(image: &Iw44RgbImage, scaled_x: u32, scaled_y: u32) -> [u8; 3] {
    let mut pixel = [0; 3];
    for (channel, component) in pixel.iter_mut().enumerate() {
        let top = iw44_upsampled_2x_row_channel(image, scaled_x, scaled_y / 2, channel);
        let bottom = iw44_upsampled_2x_row_channel(image, scaled_x, (scaled_y / 2) + 1, channel);
        let previous = iw44_upsampled_2x_row_channel(
            image,
            scaled_x,
            (scaled_y / 2).saturating_sub(1),
            channel,
        );
        let value = if scaled_y.is_multiple_of(2) {
            (previous + (top * 3) + 2) / 4
        } else {
            ((top * 3) + bottom + 2) / 4
        };
        *component = u8::try_from(value).expect("weighted RGB value should fit u8");
    }
    pixel
}

fn iw44_upsampled_2x_row_channel(
    image: &Iw44RgbImage,
    scaled_x: u32,
    source_y: u32,
    channel: usize,
) -> u16 {
    let source_x = scaled_x / 2;
    let current = u16::from(iw44_native_channel(image, source_x, source_y, channel));
    if scaled_x.is_multiple_of(2) {
        let previous = u16::from(iw44_native_channel(
            image,
            source_x.saturating_sub(1),
            source_y,
            channel,
        ));
        (previous + (current * 3) + 2) / 4
    } else {
        let next = u16::from(iw44_native_channel(image, source_x + 1, source_y, channel));
        ((current * 3) + next + 2) / 4
    }
}

fn iw44_native_pixel(image: &Iw44RgbImage, x: usize, y: usize) -> [u8; 3] {
    let offset = (y * image.width + x) * 3;
    [
        image.pixels[offset],
        image.pixels[offset + 1],
        image.pixels[offset + 2],
    ]
}

fn iw44_native_channel(image: &Iw44RgbImage, x: u32, y: u32, channel: usize) -> u8 {
    let x = usize::try_from(x)
        .unwrap_or(usize::MAX)
        .min(image.width.saturating_sub(1));
    let y = usize::try_from(y)
        .unwrap_or(usize::MAX)
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
                PageChunkKind::Info | PageChunkKind::Include => {}
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
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::document::{PageChunkSource, ResolvedPageChunk};
    use crate::page::{PageChunk, PageChunkPayload};
    use crate::{Document, decode_dirm_tail, parse_dirm_tail, write_bitmap_pdf};

    fn page_info() -> PageInfo {
        PageInfo {
            width: 100,
            height: 200,
            version: 25,
            dpi: 300,
            gamma: 2.2,
            rotation: 1,
        }
    }

    #[test]
    fn page_render_mode_parses_cli_mode_strings() {
        assert_eq!("full".parse::<PageRenderMode>(), Ok(PageRenderMode::Full));
        assert_eq!(
            "background".parse::<PageRenderMode>(),
            Ok(PageRenderMode::Background)
        );
        assert_eq!(
            "foreground".parse::<PageRenderMode>(),
            Ok(PageRenderMode::Foreground)
        );
        assert_eq!("mask".parse::<PageRenderMode>(), Ok(PageRenderMode::Mask));
        assert_eq!(PageRenderMode::Foreground.as_str(), "foreground");

        let error = "bad"
            .parse::<PageRenderMode>()
            .expect_err("unknown mode should fail");

        assert_eq!(
            error,
            RenderError::new(
                "unknown render mode \"bad\"; expected full, background, foreground, or mask"
            )
        );
    }

    #[test]
    fn page_bitmap_allocates_rgb_pixels_and_writes_in_bounds() {
        let mut bitmap = PageBitmap::new_rgb8(3, 2, 300, [0xff, 0xff, 0xff]);

        assert_eq!(bitmap.pixels.len(), 18);
        assert_eq!(bitmap.pixel_offset(2, 1), Some(15));
        assert!(bitmap.set_rgb(1, 1, [0x12, 0x34, 0x56]));
        assert!(!bitmap.set_rgb(3, 1, [0, 0, 0]));
        assert_eq!(&bitmap.pixels[12..15], &[0x12, 0x34, 0x56]);
    }

    #[test]
    fn bitonal_bitmap_sets_and_reads_msb_first_bits() {
        let mut mask = BitonalBitmap::new(3, 3);

        assert_eq!(mask.bit(0, 0), Some(false));
        assert!(mask.set_bit(0, 0, true));
        assert!(mask.set_bit(2, 1, true));
        assert!(mask.set_bit(2, 1, false));
        assert!(!mask.set_bit(3, 0, true));

        assert_eq!(mask.bit(0, 0), Some(true));
        assert_eq!(mask.bit(2, 1), Some(false));
        assert_eq!(mask.bit(3, 0), None);
        assert_eq!(mask.bits, [0b1000_0000, 0]);
    }

    #[test]
    fn bitonal_bitmap_validates_packed_bit_length() {
        assert!(BitonalBitmap::from_bits(9, 1, vec![0; 2]).is_some());
        assert!(BitonalBitmap::from_bits(9, 1, vec![0; 1]).is_none());
    }

    #[test]
    fn bitonal_bitmap_writes_binary_pbm_rows() {
        let mut mask = BitonalBitmap::new(3, 2);
        assert!(mask.set_bit(0, 0, true));
        assert!(mask.set_bit(2, 1, true));

        assert_eq!(mask.to_pbm_bytes(), b"P4\n3 2\n\x80\x20".to_vec());
    }

    #[test]
    fn page_bitmap_paints_bitonal_mask_pixels() {
        let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0xff, 0xff, 0xff]);
        let mut mask = BitonalBitmap::new(2, 2);

        assert!(mask.set_bit(1, 0, true));
        assert!(mask.set_bit(0, 1, true));
        assert!(bitmap.paint_bitonal_mask(&mask, [0, 0, 0]));

        assert_eq!(&bitmap.pixels[0..3], &[0xff, 0xff, 0xff]);
        assert_eq!(&bitmap.pixels[3..6], &[0, 0, 0]);
        assert_eq!(&bitmap.pixels[6..9], &[0, 0, 0]);
        assert_eq!(&bitmap.pixels[9..12], &[0xff, 0xff, 0xff]);
    }

    #[test]
    fn page_bitmap_paints_scaled_iw44_rgb_layer() {
        let mut bitmap = PageBitmap::new_rgb8(4, 4, 300, [0xff, 0xff, 0xff]);
        let image = Iw44RgbImage {
            width: 2,
            height: 2,
            pixels: vec![
                0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0x22, 0x33, 0x44,
            ],
        };
        let mapping = Iw44PageMapping {
            page_width: 4,
            page_height: 4,
            layer_width: 2,
            layer_height: 2,
            subsample: 2,
            scaled_width: 4,
            scaled_height: 4,
            horizontal_overscan: 0,
            vertical_overscan: 0,
        };

        assert!(bitmap.paint_iw44_rgb_layer(&image, &mapping));

        assert_eq!(&bitmap.pixels[0..3], &[0xff, 0x00, 0x00]);
        assert_eq!(&bitmap.pixels[6..9], &[0x40, 0xbf, 0x00]);
        assert_eq!(&bitmap.pixels[24..27], &[0x40, 0x00, 0xbf]);
        assert_eq!(&bitmap.pixels[45..48], &[0x22, 0x33, 0x44]);
    }

    #[test]
    fn page_bitmap_crops_iw44_overscan_from_top_left() {
        let mut bitmap = PageBitmap::new_rgb8(2, 4, 300, [0xff, 0xff, 0xff]);
        let image = Iw44RgbImage {
            width: 1,
            height: 3,
            pixels: vec![
                0x10, 0x00, 0x00, // overscanned top row
                0x20, 0x00, 0x00, //
                0x30, 0x00, 0x00, //
            ],
        };
        let mapping = Iw44PageMapping {
            page_width: 2,
            page_height: 4,
            layer_width: 1,
            layer_height: 3,
            subsample: 2,
            scaled_width: 2,
            scaled_height: 6,
            horizontal_overscan: 0,
            vertical_overscan: 2,
        };

        assert!(bitmap.paint_iw44_rgb_layer(&image, &mapping));

        assert_eq!(
            bitmap.pixels,
            vec![
                0x1c, 0x00, 0x00, 0x1c, 0x00, 0x00, //
                0x24, 0x00, 0x00, 0x24, 0x00, 0x00, //
                0x2c, 0x00, 0x00, 0x2c, 0x00, 0x00, //
                0x30, 0x00, 0x00, 0x30, 0x00, 0x00, //
            ]
        );
    }

    #[test]
    fn page_bitmap_paints_scaled_iw44_rgb_layer_through_mask() {
        let mut bitmap = PageBitmap::new_rgb8(4, 4, 300, [0xff, 0xff, 0xff]);
        let mut mask = BitonalBitmap::new(4, 4);
        assert!(mask.set_bit(1, 0, true));
        assert!(mask.set_bit(3, 3, true));
        let image = Iw44RgbImage {
            width: 2,
            height: 2,
            pixels: vec![
                0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0x22, 0x33, 0x44,
            ],
        };
        let mapping = Iw44PageMapping {
            page_width: 4,
            page_height: 4,
            layer_width: 2,
            layer_height: 2,
            subsample: 2,
            scaled_width: 4,
            scaled_height: 4,
            horizontal_overscan: 0,
            vertical_overscan: 0,
        };

        assert!(bitmap.paint_iw44_rgb_layer_through_mask(&image, &mapping, &mask));

        assert_eq!(&bitmap.pixels[0..3], &[0xff, 0xff, 0xff]);
        assert_eq!(&bitmap.pixels[3..6], &[0xff, 0x00, 0x00]);
        assert_eq!(&bitmap.pixels[42..45], &[0xff, 0xff, 0xff]);
        assert_eq!(&bitmap.pixels[45..48], &[0x22, 0x33, 0x44]);
    }

    #[test]
    fn page_bitmap_rejects_mask_with_mismatched_dimensions() {
        let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0xff, 0xff, 0xff]);
        let mask = BitonalBitmap::new(1, 2);

        assert!(!bitmap.paint_bitonal_mask(&mask, [0, 0, 0]));
    }

    #[test]
    fn page_bitmap_can_be_created_from_page_info() {
        let bitmap = PageBitmap::white_rgb8(&page_info());

        assert_eq!(bitmap.width, 100);
        assert_eq!(bitmap.height, 200);
        assert_eq!(bitmap.dpi, 300);
        assert!(bitmap.pixels.iter().all(|byte| *byte == 0xff));
    }

    #[test]
    fn page_bitmap_writes_binary_ppm_bytes() {
        let mut bitmap = PageBitmap::new_rgb8(1, 2, 300, [0xff, 0xff, 0xff]);
        assert!(bitmap.set_rgb(0, 1, [0, 0, 0]));

        assert_eq!(
            bitmap.to_ppm_bytes(),
            b"P6\n1 2\n255\n\xff\xff\xff\0\0\0".to_vec()
        );
    }

    #[test]
    fn page_bitmap_reads_binary_ppm_bytes() {
        let ppm = b"P6\n# fixture\n2 1\n255\n\x00\x01\x02\xfd\xfe\xff";

        let bitmap = PageBitmap::from_ppm_bytes(ppm, 144).expect("PPM should parse");

        assert_eq!(bitmap.width, 2);
        assert_eq!(bitmap.height, 1);
        assert_eq!(bitmap.dpi, 144);
        assert_eq!(bitmap.pixels, [0, 1, 2, 253, 254, 255]);
    }

    #[test]
    fn page_bitmap_rejects_unsupported_ppm() {
        assert_eq!(
            PageBitmap::from_ppm_bytes(b"P3\n1 1\n255\n0 0 0", 300)
                .expect_err("plain-text PPM should fail")
                .to_string(),
            "PPM oracle must use binary P6 format"
        );
        assert_eq!(
            PageBitmap::from_ppm_bytes(b"P6\n1 1\n15\n\x00\x00\x00", 300)
                .expect_err("non-8-bit PPM should fail")
                .to_string(),
            "PPM oracle max value 15 is not supported"
        );
    }

    #[test]
    fn page_bitmap_diff_reports_component_deltas() {
        let mut actual = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);
        let mut expected = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);
        assert!(actual.set_rgb(1, 0, [10, 20, 30]));
        assert!(expected.set_rgb(1, 0, [7, 25, 40]));

        let diff = actual.diff(&expected).expect("matching dimensions");

        assert_eq!(diff.width, 2);
        assert_eq!(diff.height, 1);
        assert_eq!(diff.compared_pixels, 2);
        assert_eq!(diff.exact_pixels, 1);
        assert_eq!(diff.differing_pixels, 1);
        assert_eq!(diff.total_abs_delta, 18);
        assert_eq!(diff.max_abs_delta, 10);
        assert_eq!(diff.max_delta_pixels, 1);
        assert!((diff.mean_abs_delta - 3.0).abs() < f64::EPSILON);
        assert_eq!(
            diff.channels,
            [
                PageBitmapChannelDiff {
                    total_abs_delta: 3,
                    signed_delta: 3,
                    max_abs_delta: 3,
                    mean_abs_delta: 1.5,
                    mean_signed_delta: 1.5,
                },
                PageBitmapChannelDiff {
                    total_abs_delta: 5,
                    signed_delta: -5,
                    max_abs_delta: 5,
                    mean_abs_delta: 2.5,
                    mean_signed_delta: -2.5,
                },
                PageBitmapChannelDiff {
                    total_abs_delta: 10,
                    signed_delta: -10,
                    max_abs_delta: 10,
                    mean_abs_delta: 5.0,
                    mean_signed_delta: -5.0,
                },
            ]
        );
        assert_eq!(
            diff.bounds,
            Some(PageBitmapDiffBounds {
                min_x: 1,
                min_y: 0,
                max_x: 1,
                max_y: 0,
            })
        );
        assert_eq!(
            diff.first_difference,
            Some(PageBitmapDiffPixel {
                x: 1,
                y: 0,
                actual: [10, 20, 30],
                expected: [7, 25, 40],
                abs_delta_sum: 18,
                max_abs_delta: 10,
            })
        );
        assert_eq!(diff.max_difference, diff.first_difference);
    }

    #[test]
    fn page_bitmap_diff_counts_pixels_at_max_delta() {
        let mut actual = PageBitmap::new_rgb8(3, 1, 300, [0, 0, 0]);
        let expected = PageBitmap::new_rgb8(3, 1, 300, [0, 0, 0]);
        assert!(actual.set_rgb(0, 0, [9, 0, 0]));
        assert!(actual.set_rgb(1, 0, [0, 9, 0]));
        assert!(actual.set_rgb(2, 0, [0, 0, 3]));

        let diff = actual.diff(&expected).expect("matching dimensions");

        assert_eq!(diff.differing_pixels, 3);
        assert_eq!(diff.max_abs_delta, 9);
        assert_eq!(diff.max_delta_pixels, 2);
    }

    #[test]
    fn bitmap_diff_failures_reports_exceeded_compare_limits() {
        let mut actual = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);
        let expected = PageBitmap::new_rgb8(2, 1, 300, [0xff, 0xff, 0xff]);
        assert!(actual.set_rgb(1, 0, [0xff, 0xff, 0xfe]));
        let diff = actual.diff(&expected).expect("matching dimensions");

        assert_eq!(
            bitmap_diff_failures(&diff, RenderCompareLimits::new(1, 1, Some(0), 1.0)),
            [
                "render differs in 2 pixels; allowed 1",
                "render max absolute delta is 255; allowed 1",
                "render has 1 pixels at max absolute delta; allowed 0",
                "render mean absolute delta is 127.666667; allowed 1.000000",
            ]
        );
    }

    #[test]
    fn bitmap_diff_region_summary_counts_local_difference_characteristics() {
        let mut actual = PageBitmap::new_rgb8(3, 2, 300, [0xff, 0xff, 0xff]);
        let mut expected = actual.clone();
        assert!(actual.set_rgb(1, 0, [0, 0, 0]));
        assert!(actual.set_rgb(2, 0, [0x10, 0x20, 0x30]));
        assert!(expected.set_rgb(2, 0, [0x10, 0x20, 0x31]));

        let summary = bitmap_diff_region_summary(
            &actual,
            &expected,
            PageBitmapDiffBounds {
                min_x: 1,
                min_y: 0,
                max_x: 2,
                max_y: 0,
            },
        )
        .expect("region should be in bounds");

        assert_eq!(
            summary,
            PageBitmapDiffRegionSummary {
                pixels: 2,
                differing_pixels: 2,
                total_abs_delta: 766,
                max_abs_delta: 255,
                max_delta_pixels: 1,
                actual_black_pixels: 1,
                expected_black_pixels: 0,
                actual_white_pixels: 0,
                expected_white_pixels: 1,
            }
        );
    }

    #[test]
    fn bitmap_diff_tile_summaries_reports_only_differing_tiles() {
        let mut actual = PageBitmap::new_rgb8(5, 3, 300, [0, 0, 0]);
        let expected = actual.clone();
        assert!(actual.set_rgb(4, 2, [0xff, 0, 0]));

        let tiles = bitmap_diff_tile_summaries(&actual, &expected, 2, 2)
            .expect("matching dimensions should produce tile summaries");

        assert_eq!(
            tiles,
            [PageBitmapDiffTileSummary {
                bounds: PageBitmapDiffBounds {
                    min_x: 4,
                    min_y: 2,
                    max_x: 4,
                    max_y: 2,
                },
                summary: PageBitmapDiffRegionSummary {
                    pixels: 1,
                    differing_pixels: 1,
                    total_abs_delta: 255,
                    max_abs_delta: 255,
                    max_delta_pixels: 1,
                    actual_black_pixels: 0,
                    expected_black_pixels: 1,
                    actual_white_pixels: 0,
                    expected_white_pixels: 0,
                },
            }]
        );
    }

    #[test]
    fn bitmap_diff_tile_summaries_rejects_invalid_inputs() {
        let actual = PageBitmap::new_rgb8(1, 1, 300, [0, 0, 0]);
        let expected = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);

        assert!(bitmap_diff_tile_summaries(&actual, &actual, 0, 1).is_none());
        assert!(bitmap_diff_tile_summaries(&actual, &actual, 1, 0).is_none());
        assert!(bitmap_diff_tile_summaries(&actual, &expected, 1, 1).is_none());
    }

    #[test]
    fn page_bitmap_stats_describe_rgb_pixels() {
        let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0xff, 0xff, 0xff]);
        assert!(bitmap.set_rgb(0, 0, [0, 0, 0]));
        assert!(bitmap.set_rgb(1, 0, [10, 20, 30]));
        assert!(bitmap.set_rgb(0, 1, [7, 7, 7]));

        let stats = bitmap.stats();

        assert_eq!(stats.width, 2);
        assert_eq!(stats.height, 2);
        assert_eq!(stats.pixel_count, 4);
        assert_eq!(stats.black_pixels, 1);
        assert_eq!(stats.white_pixels, 1);
        assert_eq!(stats.non_gray_pixels, 1);
        assert_eq!(stats.component_sum, 846);
        assert_ne!(stats.fingerprint, 0);
    }

    #[test]
    fn render_plan_creates_base_bitmap_from_info() {
        let plan = PageRenderPlan::new(page_info(), Vec::new());
        let bitmap = plan.render_base_bitmap();

        assert_eq!(bitmap.width, 100);
        assert_eq!(bitmap.height, 200);
        assert_eq!(bitmap.dpi, 300);
    }

    fn resolved_chunk(kind: PageChunkKind, id: &'static str) -> ResolvedPageChunk<'static> {
        ResolvedPageChunk {
            source: PageChunkSource::Page,
            chunk: PageChunk {
                chunk: Chunk {
                    id,
                    size: 0,
                    data_start: 0,
                    data_end: 0,
                    next_start: 0,
                },
                kind,
                payload: PageChunkPayload::Raw,
            },
        }
    }

    #[test]
    fn render_plan_classifies_effective_page_chunks() {
        let plan = PageRenderPlan::new(
            page_info(),
            vec![
                resolved_chunk(PageChunkKind::Info, "INFO"),
                resolved_chunk(PageChunkKind::Djbz, "Djbz"),
                resolved_chunk(PageChunkKind::Sjbz, "Sjbz"),
                resolved_chunk(PageChunkKind::Fg44, "FG44"),
                resolved_chunk(PageChunkKind::Bg44, "BG44"),
                resolved_chunk(PageChunkKind::Txtz, "TXTz"),
                resolved_chunk(PageChunkKind::Unknown, "ZZZZ"),
            ],
        );

        assert!(plan.has_image_data());
        assert!(plan.has_text());
        assert_eq!(plan.bitonal_dictionaries, [1]);
        assert_eq!(plan.bitonal_images, [2]);
        assert_eq!(plan.foreground_layers, [3]);
        assert_eq!(plan.background_layers, [4]);
        assert_eq!(plan.text_chunks, [5]);
        assert_eq!(plan.unknown_chunks, [6]);
        assert_eq!(
            plan.chunk(2).map(|chunk| chunk.chunk.chunk.id),
            Some("Sjbz")
        );
    }

    #[test]
    fn render_plan_extracts_bitonal_payloads_by_chunk_index() {
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![
                resolved_chunk(PageChunkKind::Djbz, "Djbz"),
                resolved_chunk(PageChunkKind::Sjbz, "Sjbz"),
                resolved_chunk(PageChunkKind::Txtz, "TXTz"),
            ],
        );
        plan.chunks[0].chunk.chunk.data_start = 1;
        plan.chunks[0].chunk.chunk.data_end = 4;
        plan.chunks[1].chunk.chunk.data_start = 4;
        plan.chunks[1].chunk.chunk.data_end = 7;
        let bytes = b"Xdictimg";

        let dictionaries = plan.bitonal_dictionary_payloads(bytes);
        let images = plan.bitonal_image_payloads(bytes);

        assert_eq!(
            dictionaries,
            [RenderChunkPayload {
                index: 0,
                bytes: b"dic"
            }]
        );
        assert_eq!(
            images,
            [RenderChunkPayload {
                index: 1,
                bytes: b"tim"
            }]
        );
    }

    #[test]
    fn render_plan_extracts_iw44_layer_payloads_by_chunk_index() {
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![
                resolved_chunk(PageChunkKind::Fg44, "FG44"),
                resolved_chunk(PageChunkKind::Bg44, "BG44"),
                resolved_chunk(PageChunkKind::Bg44, "BG44"),
                resolved_chunk(PageChunkKind::Sjbz, "Sjbz"),
            ],
        );
        plan.chunks[0].chunk.chunk.data_start = 1;
        plan.chunks[0].chunk.chunk.data_end = 4;
        plan.chunks[1].chunk.chunk.data_start = 4;
        plan.chunks[1].chunk.chunk.data_end = 7;
        plan.chunks[2].chunk.chunk.data_start = 7;
        plan.chunks[2].chunk.chunk.data_end = 9;
        let bytes = b"Xfgbg0bg1";

        let foreground = plan.foreground_layer_payloads(bytes);
        let background = plan.background_layer_payloads(bytes);

        assert_eq!(
            foreground,
            [RenderChunkPayload {
                index: 0,
                bytes: b"fgb"
            }]
        );
        assert_eq!(
            background,
            [
                RenderChunkPayload {
                    index: 1,
                    bytes: b"g0b"
                },
                RenderChunkPayload {
                    index: 2,
                    bytes: b"g1"
                }
            ]
        );
    }

    #[test]
    fn render_plan_reads_iw44_layer_summaries() {
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![
                resolved_chunk(PageChunkKind::Fg44, "FG44"),
                resolved_chunk(PageChunkKind::Bg44, "BG44"),
                resolved_chunk(PageChunkKind::Bg44, "BG44"),
            ],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = 10;
        plan.chunks[1].chunk.chunk.data_start = 10;
        plan.chunks[1].chunk.chunk.data_end = 20;
        plan.chunks[2].chunk.chunk.data_start = 20;
        plan.chunks[2].chunk.chunk.data_end = 23;
        let bytes = [
            0x00, 0x05, 0x01, 0x02, 0x00, 0x20, 0x00, 0x10, 0x80, 0xaa, 0x00, 0x07, 0x01, 0x02,
            0x00, 0x40, 0x00, 0x20, 0x00, 0xbb, 0x01, 0x03, 0xcc,
        ];

        let foreground = plan
            .foreground_layer_summary(&bytes)
            .expect("foreground summary should parse")
            .expect("foreground layer should exist");
        let background = plan
            .background_layer_summary(&bytes)
            .expect("background summary should parse")
            .expect("background layer should exist");

        assert_eq!(foreground.image.width, 32);
        assert_eq!(foreground.image.height, 16);
        assert_eq!(foreground.total_slices, 5);
        assert_eq!(foreground.total_payload_bytes, 1);
        assert_eq!(background.image.width, 64);
        assert_eq!(background.image.height, 32);
        assert_eq!(background.total_slices, 10);
        assert_eq!(background.total_payload_bytes, 2);
    }

    #[test]
    fn render_plan_maps_iw44_layers_to_page_space() {
        let mut plan = PageRenderPlan::new(
            PageInfo {
                width: 1560,
                height: 1633,
                version: 25,
                dpi: 200,
                gamma: 2.2,
                rotation: 1,
            },
            vec![
                resolved_chunk(PageChunkKind::Fg44, "FG44"),
                resolved_chunk(PageChunkKind::Bg44, "BG44"),
            ],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = 10;
        plan.chunks[1].chunk.chunk.data_start = 10;
        plan.chunks[1].chunk.chunk.data_end = 20;
        let bytes = [
            0x00, 0x64, 0x01, 0x02, 0x00, 0xc3, 0x00, 0xcd, 0x80, 0xaa, 0x00, 0x4a, 0x01, 0x02,
            0x03, 0x0c, 0x03, 0x31, 0x8a, 0xbb,
        ];

        let foreground = plan
            .foreground_layer_geometry(&bytes)
            .expect("foreground geometry should parse")
            .expect("foreground layer should exist");
        let background = plan
            .background_layer_geometry(&bytes)
            .expect("background geometry should parse")
            .expect("background layer should exist");

        assert_eq!(foreground.mapping.subsample, 8);
        assert_eq!(foreground.mapping.scaled_width, 1560);
        assert_eq!(foreground.mapping.scaled_height, 1640);
        assert_eq!(foreground.mapping.vertical_overscan, 7);
        assert_eq!(background.mapping.subsample, 2);
        assert_eq!(background.mapping.scaled_width, 1560);
        assert_eq!(background.mapping.scaled_height, 1634);
        assert_eq!(background.mapping.vertical_overscan, 1);
    }

    #[test]
    fn render_plan_returns_sjbz_payloads_without_bzz_decoding() {
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = 3;

        let images = plan.bitonal_image_payloads(b"jb2");

        assert_eq!(
            images,
            [RenderChunkPayload {
                index: 0,
                bytes: b"jb2"
            }]
        );
    }

    #[test]
    fn render_plan_reads_bitonal_image_headers() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let headers = plan
            .bitonal_image_headers(RYPKA_PAGE_1_SJBZ)
            .expect("JB2 header should parse");

        assert_eq!(
            headers,
            [BitonalImageHeader {
                index: 0,
                header: Jb2ImageHeader {
                    width: 1560,
                    height: 1633,
                    inherited_dictionary_symbols: 0,
                },
            }]
        );
    }

    #[test]
    fn render_plan_reads_bitonal_record_prefixes() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let prefixes = plan
            .bitonal_record_prefixes(RYPKA_PAGE_1_SJBZ, 8)
            .expect("JB2 record prefix should parse");

        assert_eq!(prefixes.len(), 1);
        assert_eq!(prefixes[0].0, 0);
        assert_eq!(prefixes[0].1.header.width, 1560);
        assert!(!prefixes[0].1.records.is_empty());
    }

    #[test]
    fn render_plan_reads_partial_bitonal_masks() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let masks = plan
            .partial_bitonal_masks(RYPKA_PAGE_1_SJBZ, 8)
            .expect("partial JB2 mask should render");

        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].1.mask.width, 1560);
        assert!(!masks[0].1.reached_end_of_data);
        assert!(masks[0].1.mask.black_pixel_count() > 0);
    }

    #[test]
    fn render_plan_reads_complete_bitonal_masks() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let masks = plan
            .bitonal_masks(RYPKA_PAGE_1_SJBZ)
            .expect("complete JB2 mask should render");

        assert_eq!(masks.len(), 1);
        assert!(masks[0].1.reached_end_of_data);
        assert_eq!(masks[0].1.mask.black_pixel_count(), 167_493);
    }

    #[test]
    fn render_plan_paints_bitonal_masks_into_rgb_bitmap() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            PageInfo {
                width: 1560,
                height: 1633,
                version: 25,
                dpi: 200,
                gamma: 2.2,
                rotation: 1,
            },
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let render = plan
            .render_partial_bitmap(RYPKA_PAGE_1_SJBZ)
            .expect("partial bitmap should render");
        let black_pixels = render
            .bitmap
            .pixels
            .chunks_exact(3)
            .filter(|pixel| **pixel == [0, 0, 0])
            .count();

        assert_eq!(render.bitonal_masks.len(), 1);
        assert!(render.bitonal_masks[0].1.reached_end_of_data);
        assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
        assert_eq!(black_pixels, 167_493);
    }

    #[test]
    fn render_plan_foreground_mode_paints_bitonal_masks_without_iw44_foreground() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            PageInfo {
                width: 1560,
                height: 1633,
                version: 25,
                dpi: 200,
                gamma: 2.2,
                rotation: 1,
            },
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let render = plan
            .render_bitmap_with_mode(RYPKA_PAGE_1_SJBZ, PageRenderMode::Foreground)
            .expect("foreground mode should render");
        let black_pixels = render
            .bitmap
            .pixels
            .chunks_exact(3)
            .filter(|pixel| **pixel == [0, 0, 0])
            .count();

        assert!(render.iw44_layers.is_empty());
        assert_eq!(render.bitonal_masks.len(), 1);
        assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
        assert_eq!(black_pixels, 167_493);
    }

    #[test]
    fn render_document_page_rejects_invalid_page_numbers() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");

        assert!(matches!(
            render_document_page(RYPKA, 0, PageRenderMode::Full)
                .expect_err("zero page should fail"),
            DjvuRenderError::ZeroPage
        ));
        let missing = render_document_page(RYPKA, 10_000, PageRenderMode::Full)
            .expect_err("page should fail");
        let DjvuRenderError::PageOutOfRange { page, page_count } = missing else {
            panic!("expected page range error, got {missing}");
        };
        assert_eq!(page, 10_000);
        assert!(page_count > 0);
    }

    #[test]
    fn render_document_page_renders_fixture_page_layer_modes() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");

        let foreground = render_document_page(RYPKA, 1, PageRenderMode::Foreground)
            .expect("foreground page should render");
        let mask =
            render_document_page(RYPKA, 1, PageRenderMode::Mask).expect("mask page should render");

        assert_eq!(
            (foreground.bitmap.width, foreground.bitmap.height),
            (1560, 1633)
        );
        assert_eq!(foreground.iw44_layers.len(), 1);
        assert_eq!(foreground.bitonal_masks.len(), 1);
        assert!(foreground.bitmap.stats().black_pixels < 167_493);
        assert!(mask.iw44_layers.is_empty());
        assert_eq!(mask.bitonal_masks.len(), 1);
        assert_eq!(mask.bitmap.stats().black_pixels, 167_493);
    }

    #[test]
    fn render_document_pages_rejects_invalid_page_ranges() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");

        assert!(matches!(
            render_document_pages(RYPKA, 0, None, PageRenderMode::Full)
                .expect_err("zero from page should fail"),
            DjvuRenderError::ZeroFromPage
        ));
        assert!(matches!(
            render_document_pages(RYPKA, 2, Some(1), PageRenderMode::Full)
                .expect_err("reversed range should fail"),
            DjvuRenderError::ReversedPageRange
        ));
    }

    #[test]
    fn render_document_pages_with_events_renders_fixture_range() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
        let mut events = Vec::new();

        let renders =
            render_document_pages_with_events(RYPKA, 68, Some(68), PageRenderMode::Full, |event| {
                match event {
                    DjvuPageRenderEvent::PageStarted {
                        page_number,
                        end_page,
                    } => events.push((page_number, end_page, false, 0, 0)),
                    DjvuPageRenderEvent::PageRendered {
                        page_number,
                        render,
                    } => events.push((
                        page_number,
                        page_number,
                        true,
                        render.bitmap.width,
                        render.bitmap.height,
                    )),
                }
            })
            .expect("fixture range should render");

        assert_eq!(renders.len(), 1);
        assert_eq!(events, [(68, 68, false, 0, 0), (68, 68, true, 3423, 5075),]);
        assert_eq!(renders[0].page_number, 68);
        assert_eq!(
            (
                renders[0].render.bitmap.width,
                renders[0].render.bitmap.height
            ),
            (3423, 5075)
        );
    }

    #[test]
    fn render_plan_paints_iw44_background_before_bitonal_masks() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
        let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
        let page = document
            .pages(RYPKA)
            .next()
            .expect("fixture should have a first page")
            .expect("first page should parse");
        let mut decoded_tail = Vec::new();
        let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
            decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
            parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
        });
        let plan = document
            .page_render_plan(RYPKA, &page, &tail_entries)
            .expect("render plan should parse");

        let render = plan
            .render_partial_bitmap(RYPKA)
            .expect("partial bitmap should render");
        let stats = render.bitmap.stats();
        let black_pixels = render
            .bitmap
            .pixels
            .chunks_exact(3)
            .filter(|pixel| **pixel == [0, 0, 0])
            .count();
        let white_pixels = render
            .bitmap
            .pixels
            .chunks_exact(3)
            .filter(|pixel| **pixel == [0xff, 0xff, 0xff])
            .count();
        let foreground_colored_mask_pixels = (0..render.bitmap.height)
            .flat_map(|y| (0..render.bitmap.width).map(move |x| (x, y)))
            .filter(|(x, y)| render.bitonal_masks[0].1.mask.bit(*x, *y).unwrap_or(false))
            .filter(|(x, y)| {
                let offset = render
                    .bitmap
                    .pixel_offset(*x, *y)
                    .expect("mask coordinate should be in bitmap");
                render.bitmap.pixels[offset..offset + 3] != [0, 0, 0]
            })
            .count();

        assert_eq!(render.iw44_layers.len(), 2);
        assert_eq!(render.iw44_layers[0].role, Iw44LayerRole::Background);
        assert_eq!(render.iw44_layers[0].chunk_indices, [3, 4, 5, 6]);
        assert_eq!(render.iw44_layers[0].image.width, 780);
        assert_eq!(render.iw44_layers[0].image.height, 817);
        assert_eq!(render.iw44_layers[1].role, Iw44LayerRole::Foreground);
        assert_eq!(render.iw44_layers[1].chunk_indices, [2]);
        assert_eq!(render.bitonal_masks.len(), 1);
        assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
        assert!(black_pixels < 167_493);
        assert!(foreground_colored_mask_pixels > 0);
        assert!(white_pixels < 100_000);
        assert_eq!(stats.width, 1560);
        assert_eq!(stats.height, 1633);
        assert_eq!(stats.pixel_count, 1560 * 1633);
        assert_eq!(
            (
                stats.black_pixels,
                stats.white_pixels,
                stats.non_gray_pixels,
                stats.component_sum,
                stats.fingerprint,
            ),
            (0, 0, 2_547_446, 621_545_909, 14_319_312_755_699_683_013)
        );

        let pdf = write_bitmap_pdf(std::slice::from_ref(&render.bitmap))
            .expect("rendered bitmap should serialize as PDF");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Type /Page"));
        assert!(text.contains("/MediaBox [0 0 561.6000 587.8800]"));
        assert!(text.contains("/Subtype /Image /Width 1560 /Height 1633"));
        assert!(text.contains("/Length 7642440"));
        assert!(text.contains("xref\n0 6\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn render_plan_paints_rypka_page_961_background_without_iw44_artifact() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
        let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
        let page = document
            .pages(RYPKA)
            .nth(960)
            .expect("fixture should have page 961")
            .expect("page 961 should parse");
        let mut decoded_tail = Vec::new();
        let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
            decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
            parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
        });
        let plan = document
            .page_render_plan(RYPKA, &page, &tail_entries)
            .expect("render plan should parse");
        let background = plan
            .background_iw44_layer(RYPKA)
            .expect("background IW44 should decode")
            .expect("page should have background IW44");
        let mut bitmap = plan.render_base_bitmap();

        assert!(bitmap.paint_iw44_rgb_layer(&background.image, &background.geometry.mapping));

        let offset = bitmap
            .pixel_offset(1167, 834)
            .expect("target pixel should be in page");
        assert_eq!(&bitmap.pixels[offset..offset + 3], &[0xff, 0xff, 0xff]);
        assert_eq!(background.role, Iw44LayerRole::Background);
        assert_eq!(background.chunk_indices, [1, 2, 3, 4]);
        assert_eq!(background.image.width, 3486);
        assert_eq!(background.image.height, 2783);
        assert_eq!(background.geometry.mapping.subsample, 1);

        let stats = bitmap.stats();
        assert_eq!(stats.width, 3486);
        assert_eq!(stats.height, 2783);
        assert_eq!(
            (
                stats.black_pixels,
                stats.white_pixels,
                stats.non_gray_pixels,
                stats.component_sum,
                stats.fingerprint,
            ),
            (
                4_577,
                4_757_199,
                2_831_750,
                6_781_848_240,
                0xb902_dd6e_3b4e_270d
            )
        );
    }

    #[test]
    fn render_plan_uses_inherited_jb2_dictionary() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
        let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
        let page = document
            .pages(RYPKA)
            .nth(2)
            .expect("fixture should have a third page")
            .expect("third page should parse");
        let mut decoded_tail = Vec::new();
        let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
            decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
            parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
        });
        let plan = document
            .page_render_plan(RYPKA, &page, &tail_entries)
            .expect("render plan should parse");

        let render = plan
            .render_partial_bitmap(RYPKA)
            .expect("partial bitmap should render");
        let stats = render.bitmap.stats();

        assert_eq!(render.bitonal_masks.len(), 1);
        assert_eq!(
            render.bitonal_masks[0]
                .1
                .header
                .inherited_dictionary_symbols,
            284
        );
        assert!(render.bitonal_masks[0].1.reached_end_of_data);
        assert_eq!(render.bitonal_masks[0].1.dictionary_symbol_count, 293);
        assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 29_084);
        assert_eq!(stats.black_pixels, 29_084);
        assert_eq!(stats.white_pixels, 17_344_292);
        assert_eq!(stats.fingerprint, 0x5004_65d0_2fa7_ab6f);
    }

    #[test]
    fn render_plan_rejects_bitonal_masks_with_mismatched_page_dimensions() {
        const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");
        let mut plan = PageRenderPlan::new(
            page_info(),
            vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
        );
        plan.chunks[0].chunk.chunk.data_start = 0;
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

        let error = plan
            .render_partial_bitmap(RYPKA_PAGE_1_SJBZ)
            .expect_err("mismatched dimensions should fail");

        assert_eq!(
            error.to_string(),
            "bitonal image #0 dimensions 1560x1633 do not match page 100x200"
        );
    }
}
