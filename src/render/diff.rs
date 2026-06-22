use super::{PageBitmap, RenderError, RenderResult};

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

impl PageBitmap {
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
}

fn mean_delta(total_abs_delta: u64, component_count: usize) -> f64 {
    u64_to_f64(total_abs_delta) / usize_to_f64(component_count)
}

fn mean_signed_delta(signed_delta: i64, component_count: usize) -> f64 {
    i64_to_f64(signed_delta) / usize_to_f64(component_count)
}

fn usize_to_f64(value: usize) -> f64 {
    u64_to_f64(u64::try_from(value).expect("usize should fit u64"))
}

fn i64_to_f64(value: i64) -> f64 {
    if value < 0 {
        -u64_to_f64(value.unsigned_abs())
    } else {
        u64_to_f64(u64::try_from(value).expect("non-negative i64 should fit u64"))
    }
}

fn u64_to_f64(value: u64) -> f64 {
    let [b0, b1, b2, b3, b4, b5, b6, b7] = value.to_le_bytes();
    let low = u32::from_le_bytes([b0, b1, b2, b3]);
    let high = u32::from_le_bytes([b4, b5, b6, b7]);

    f64::from(high).mul_add(4_294_967_296.0, f64::from(low))
}
