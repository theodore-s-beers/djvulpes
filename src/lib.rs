#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

pub(crate) mod bzz;
pub(crate) mod chunk;
pub(crate) mod dirm;
pub(crate) mod document;
pub(crate) mod error;
pub(crate) mod info;
pub(crate) mod page;
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
pub use page::{PageChunk, PageChunkKind, PageChunkPayload, PageDetails, read_page_details};
pub use render::{
    BitonalBitmap, OwnedRenderChunkPayload, PageBitmap, PageRenderPlan, PixelFormat,
    RenderChunkPayload,
};
pub use text::{TextPayload, TextZone, TextZoneKind, parse_text_payload, parse_text_zones};
