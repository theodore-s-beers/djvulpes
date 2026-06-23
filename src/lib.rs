//! `DjVu` parsing, rendering, and PDF conversion primitives.
//!
//! The high-level rendering entry points are [`render_document_page`],
//! [`render_document_pages`], and [`render_document_pages_with_events`]. Use
//! [`PageRenderMode`] to select full-page, background, foreground, or mask
//! compositor output.
//!
//! The high-level PDF entry points are [`render_document_pdf`],
//! [`render_document_pdf_with_events`], and the writer-based
//! [`render_document_pdf_to_writer`]. They use the same in-house BZZ/ZP, JB2,
//! IW44, text, outline, and compositing paths as the bitmap render APIs.

#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

pub(crate) mod bzz;
pub(crate) mod chunk;
pub(crate) mod dirm;
pub(crate) mod document;
pub(crate) mod error;
pub(crate) mod info;
pub(crate) mod iw44;
pub(crate) mod jb2;
pub(crate) mod navm;
pub(crate) mod page;
pub(crate) mod pdf;
pub(crate) mod render;
pub(crate) mod text;

pub use bzz::{BzzError, BzzResult, decode_bzz, decode_dirm_tail};
pub use chunk::{Chunk, Form, parse_chunk_at, parse_chunks, parse_document_root, parse_form_at};
pub use dirm::{Dirm, DirmTailEntry, parse_dirm, parse_dirm_tail};
pub use document::{
    DirectoryEntry, Document, DocumentForm, DocumentFormKind, FormKindCounts, Page,
    PageChunkSource, Pages, ResolvedPageChunk, RootChunkCounts,
};
pub use error::{ParseError, ParseResult};
pub use info::{PageInfo, read_page_info};
pub use iw44::{
    Iw44Bitstream, Iw44ChunkHeader, Iw44CoefficientEvent, Iw44CoefficientEventKind,
    Iw44CoefficientPlane, Iw44CoefficientTraceTarget, Iw44DecodedChunk, Iw44Decoder, Iw44Error,
    Iw44ImageHeader, Iw44LayerSummary, Iw44PageMapping, Iw44Plane, Iw44PlaneCoefficientSummary,
    Iw44ReconstructionExtent, Iw44ReconstructionOrder, Iw44ReconstructionPlane, Iw44Result,
    Iw44RgbImage, read_iw44_chunk_header, summarize_iw44_layer,
};
pub use jb2::{
    Jb2Dictionary, Jb2Error, Jb2ImageHeader, Jb2PartialImage, Jb2RecordKind, Jb2RecordPrefix,
    Jb2RecordSummary, Jb2Result, decode_jb2_dictionary, read_jb2_image_header,
    read_jb2_record_prefix, render_jb2_image, render_jb2_image_with_dictionary,
    render_jb2_supported_prefix,
};
pub use navm::{Bookmark, extract_document_bookmarks, parse_navm_bookmarks};
pub use page::{PageChunk, PageChunkKind, PageChunkPayload, PageDetails, read_page_details};
pub use pdf::{
    DjvuPdfError, DjvuPdfPageKind, DjvuPdfRenderEvent, DjvuPdfResult, DjvuPdfTimingEvent,
    DjvuPdfTimingStage, PdfError, PdfPageImage, PdfResult, default_pdf_render_jobs,
    render_document_pdf, render_document_pdf_parallel, render_document_pdf_to_writer,
    render_document_pdf_to_writer_parallel, render_document_pdf_to_writer_parallel_with_timings,
    render_document_pdf_to_writer_with_events,
    render_document_pdf_to_writer_with_events_and_timings, render_document_pdf_with_events,
    write_bitmap_pdf, write_page_image_pdf, write_page_image_pdf_iter,
    write_page_image_pdf_iter_to_writer, write_rendered_pages_pdf_iter,
};
pub use render::{
    BitonalBitmap, BitonalImageHeader, DjvuPageRenderEvent, DjvuRenderError, DjvuRenderResult,
    Iw44LayerGeometry, Iw44LayerRole, PageBitmap, PageBitmapChannelDiff, PageBitmapDiff,
    PageBitmapDiffBounds, PageBitmapDiffPixel, PageBitmapDiffRegionSummary,
    PageBitmapDiffTileSummary, PageBitmapStats, PageRenderMode, PageRenderPlan, PartialPageRender,
    PixelFormat, RenderChunkPayload, RenderCompareLimits, RenderError, RenderResult,
    RenderedDocumentPage, RenderedIw44Layer, bitmap_diff_failures, bitmap_diff_region_summary,
    bitmap_diff_tile_summaries, render_document_page, render_document_pages,
    render_document_pages_with_events,
};
pub use text::{
    ExtractedTextPage, ExtractedTextZonePage, TextError, TextPayload, TextResult, TextZone,
    TextZoneKind, extract_document_text, extract_document_text_pages,
    extract_document_text_zone_pages, format_document_text_zones, parse_text_payload,
    parse_text_zones,
};
