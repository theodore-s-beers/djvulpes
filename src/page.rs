use crate::chunk::{Chunk, Form, parse_chunks};
use crate::error::Result;
use crate::info::{PageInfo, read_page_info};

#[derive(Debug, Clone)]
pub struct PageDetails<'a> {
    pub form: Form<'a>,
    pub info: Option<PageInfo>,
    pub chunks: Vec<PageChunk<'a>>,
}

#[derive(Debug, Clone)]
pub struct PageChunk<'a> {
    pub chunk: Chunk<'a>,
    pub kind: PageChunkKind,
    pub payload: PageChunkPayload<'a>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PageChunkKind {
    Info,
    Include,
    Djbz,
    Sjbz,
    Fg44,
    Bg44,
    Txta,
    Txtz,
    Unknown,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PageChunkPayload<'a> {
    Include { id: &'a str },
    Raw,
}

/// Reads the typed child chunk structure for a `FORM:DJVU` page.
///
/// # Errors
///
/// Returns an error if the page form's child chunk stream is malformed.
pub fn read_page_details<'a>(bytes: &'a [u8], form: &Form<'a>) -> Result<PageDetails<'a>> {
    let info = read_page_info(bytes, form)?;
    let chunks = parse_chunks(bytes, form.children_start, form.chunk.data_end)?
        .into_iter()
        .map(|chunk| PageChunk {
            kind: PageChunkKind::from_chunk_id(chunk.id),
            payload: PageChunkPayload::from_chunk(bytes, &chunk),
            chunk,
        })
        .collect();

    Ok(PageDetails {
        form: form.clone(),
        info,
        chunks,
    })
}

impl PageChunkKind {
    #[must_use]
    pub fn from_chunk_id(id: &str) -> Self {
        match id {
            "INFO" => Self::Info,
            "INCL" => Self::Include,
            "Djbz" => Self::Djbz,
            "Sjbz" => Self::Sjbz,
            "FG44" => Self::Fg44,
            "BG44" => Self::Bg44,
            "TXTa" => Self::Txta,
            "TXTz" => Self::Txtz,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Include => "include",
            Self::Djbz => "djbz",
            Self::Sjbz => "sjbz",
            Self::Fg44 => "fg44",
            Self::Bg44 => "bg44",
            Self::Txta => "txta",
            Self::Txtz => "txtz",
            Self::Unknown => "unknown",
        }
    }
}

impl<'a> PageChunkPayload<'a> {
    #[must_use]
    pub fn from_chunk(bytes: &'a [u8], chunk: &Chunk<'_>) -> Self {
        if chunk.id != "INCL" {
            return Self::Raw;
        }

        std::str::from_utf8(&bytes[chunk.data_start..chunk.data_end])
            .map_or(Self::Raw, |id| Self::Include { id })
    }
}

impl<'a> PageDetails<'a> {
    pub fn chunks_of_kind(&self, kind: PageChunkKind) -> impl Iterator<Item = &PageChunk<'a>> {
        self.chunks.iter().filter(move |chunk| chunk.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::parse_form_at;

    fn push_chunk(bytes: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) {
        let payload_len = u32::try_from(payload.len()).expect("test payload should fit in u32");

        bytes.extend_from_slice(&id);
        bytes.extend_from_slice(&payload_len.to_be_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            bytes.push(0);
        }
    }

    fn form(kind: [u8; 4], children: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&kind);
        payload.extend_from_slice(children);

        let mut bytes = Vec::new();
        push_chunk(&mut bytes, *b"FORM", &payload);
        bytes
    }

    #[test]
    fn read_page_details_classifies_known_page_chunks() {
        let mut children = Vec::new();
        push_chunk(
            &mut children,
            *b"INFO",
            &[0x06, 0x18, 0x06, 0x61, 25, 0, 200, 0, 22, 1],
        );
        push_chunk(&mut children, *b"INCL", b"shared");
        push_chunk(&mut children, *b"Sjbz", b"bitonal");
        push_chunk(&mut children, *b"ZZZZ", b"unknown");

        let bytes = form(*b"DJVU", &children);
        let form = parse_form_at(&bytes, 0).expect("page form should parse");

        let details = read_page_details(&bytes, &form).expect("page details should parse");

        assert_eq!(details.info.as_ref().map(|info| info.dpi), Some(200));
        assert_eq!(details.chunks.len(), 4);
        assert_eq!(details.chunks[0].kind, PageChunkKind::Info);
        assert_eq!(details.chunks[1].kind, PageChunkKind::Include);
        assert_eq!(details.chunks[2].kind, PageChunkKind::Sjbz);
        assert_eq!(details.chunks[3].kind, PageChunkKind::Unknown);
        assert_eq!(
            details.chunks[1].payload,
            PageChunkPayload::Include { id: "shared" }
        );
        assert_eq!(details.chunks_of_kind(PageChunkKind::Include).count(), 1);
    }
}
