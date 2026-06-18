#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

pub(crate) mod chunk;
pub(crate) mod dirm;
pub(crate) mod document;
pub(crate) mod error;
pub(crate) mod info;
pub(crate) mod page;

pub use chunk::{Chunk, Form, parse_chunk_at, parse_chunks, parse_document_root, parse_form_at};
pub use dirm::{Dirm, parse_dirm};
pub use document::{
    Document, DocumentForm, DocumentFormKind, FormKindCounts, Page, Pages, RootChunkCounts,
};
pub use error::{ParseError, Result};
pub use info::{PageInfo, read_page_info};
pub use page::{PageChunk, PageChunkKind, PageChunkPayload, PageDetails, read_page_details};
