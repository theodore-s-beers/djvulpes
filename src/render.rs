use crate::dirm::DirmTailEntry;
use crate::document::{Document, Page, ResolvedPageChunk};
use crate::error::{ParseError, ParseResult};
use crate::info::PageInfo;
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
    fn page_bitmap_can_be_created_from_page_info() {
        let bitmap = PageBitmap::white_rgb8(&page_info());

        assert_eq!(bitmap.width, 100);
        assert_eq!(bitmap.height, 200);
        assert_eq!(bitmap.dpi, 300);
        assert!(bitmap.pixels.iter().all(|byte| *byte == 0xff));
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
}
