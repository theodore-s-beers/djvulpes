use crate::dirm::DirmTailEntry;
use crate::document::{Document, Page, ResolvedPageChunk};
use crate::error::{ParseError, ParseResult};
use crate::info::PageInfo;
use crate::iw44::{Iw44Error, Iw44LayerSummary, Iw44PageMapping, summarize_iw44_layer};
use crate::jb2::{
    Jb2Error, Jb2ImageHeader, Jb2PartialImage, Jb2RecordPrefix, read_jb2_image_header,
    read_jb2_record_prefix, render_jb2_image, render_jb2_supported_prefix,
};
use crate::page::PageChunkKind;

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
    pub fn to_pbm_bytes(&self) -> Vec<u8> {
        let mut bytes = format!("P4\n{} {}\n", self.width, self.height).into_bytes();
        let row_bytes = (self.width as usize).div_ceil(8);

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

    #[must_use]
    pub fn to_ppm_bytes(&self) -> Vec<u8> {
        let mut bytes = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        bytes.extend_from_slice(&self.pixels);
        bytes
    }
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PartialPageRender {
    pub bitmap: PageBitmap,
    pub bitonal_masks: Vec<(usize, Jb2PartialImage)>,
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
        self.bitonal_image_payloads(bytes)
            .into_iter()
            .map(|payload| render_jb2_image(payload.bytes).map(|image| (payload.index, image)))
            .collect()
    }

    #[must_use]
    pub fn render_base_bitmap(&self) -> PageBitmap {
        PageBitmap::white_rgb8(&self.info)
    }

    /// Renders the currently supported page layers into an RGB bitmap.
    ///
    /// This paints complete `Sjbz` JB2 masks over a white page. IW44
    /// foreground/background layers are not decoded yet.
    ///
    /// # Errors
    ///
    /// Returns an error if any supported bitonal layer is malformed or has
    /// dimensions that do not match the page.
    pub fn render_partial_bitmap(&self, bytes: &[u8]) -> Result<PartialPageRender, Jb2Error> {
        let bitonal_masks = self.bitonal_masks(bytes)?;
        let mut bitmap = self.render_base_bitmap();

        for (chunk_index, partial) in &bitonal_masks {
            if !bitmap.paint_bitonal_mask(&partial.mask, [0, 0, 0]) {
                return Err(Jb2Error::new(format!(
                    "bitonal image #{chunk_index} dimensions {}x{} do not match page {}x{}",
                    partial.mask.width, partial.mask.height, bitmap.width, bitmap.height
                )));
            }
        }

        Ok(PartialPageRender {
            bitmap,
            bitonal_masks,
        })
    }
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
        assert_eq!(masks[0].1.mask.black_pixel_count(), 167_028);
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
        assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_028);
        assert_eq!(black_pixels, 167_028);
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
