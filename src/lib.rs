#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]

use std::fmt;

const DJVU_MAGIC: &[u8; 4] = b"AT&T";

pub type Result<T> = std::result::Result<T, ParseError>;

#[derive(Debug)]
pub struct ParseError(String);

impl ParseError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone)]
pub struct Chunk<'a> {
    pub id: &'a str,
    pub size: u32,
    pub data_start: usize,
    pub data_end: usize,
    pub next_start: usize,
}

#[derive(Debug, Clone)]
pub struct Form<'a> {
    pub chunk: Chunk<'a>,
    pub kind: &'a str,
    pub children_start: usize,
}

#[derive(Debug, Clone)]
pub struct Dirm {
    pub flags: u8,
    pub entry_count: u16,
    pub offsets: Vec<u32>,
    pub compressed_tail_len: usize,
}

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub width: u16,
    pub height: u16,
    pub version: u8,
    pub dpi: u16,
    pub gamma: f32,
    pub rotation: u8,
}

#[derive(Debug, Clone)]
pub struct Document<'a> {
    pub root: Form<'a>,
    pub root_chunks: Vec<Chunk<'a>>,
    pub directory: Option<Dirm>,
    pub forms: Vec<DocumentForm<'a>>,
    pub unresolved_directory_offsets: usize,
}

#[derive(Debug, Clone)]
pub struct DocumentForm<'a> {
    pub offset: u32,
    pub form: Form<'a>,
    pub kind: DocumentFormKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DocumentFormKind {
    Page,
    Shared,
    Thumbnails,
    Other,
}

#[derive(Debug, Clone)]
pub struct Page<'a> {
    pub offset: u32,
    pub form: Form<'a>,
    pub info: Option<PageInfo>,
}

impl<'a> Document<'a> {
    /// Parses a `DjVu` document into a document-level view.
    ///
    /// # Errors
    ///
    /// Returns an error if the root document form, root chunk stream, or
    /// directory chunk is malformed.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let root = parse_document_root(bytes)?;
        let root_chunks = parse_chunks(bytes, root.children_start, root.chunk.data_end)?;
        let directory = root_chunks
            .iter()
            .find(|chunk| chunk.id == "DIRM")
            .map(|chunk| parse_dirm(bytes, chunk))
            .transpose()?;

        let mut forms = Vec::new();
        let mut unresolved_directory_offsets = 0;

        if let Some(directory) = &directory {
            for offset in &directory.offsets {
                let Ok(offset_start) = usize::try_from(*offset) else {
                    unresolved_directory_offsets += 1;
                    continue;
                };

                let Ok(form) = parse_form_at(bytes, offset_start) else {
                    unresolved_directory_offsets += 1;
                    continue;
                };

                forms.push(DocumentForm {
                    offset: *offset,
                    kind: DocumentFormKind::from_form_kind(form.kind),
                    form,
                });
            }
        } else if root.kind == "DJVU" {
            forms.push(DocumentForm {
                offset: 4,
                kind: DocumentFormKind::Page,
                form: root.clone(),
            });
        }

        Ok(Self {
            root,
            root_chunks,
            directory,
            forms,
            unresolved_directory_offsets,
        })
    }

    #[must_use]
    pub fn pages(&'a self, bytes: &'a [u8]) -> Pages<'a> {
        Pages {
            bytes,
            forms: self.forms.iter(),
        }
    }

    #[must_use]
    pub fn form_kind_counts(&self) -> FormKindCounts {
        let mut counts = FormKindCounts::default();

        for form in &self.forms {
            match form.kind {
                DocumentFormKind::Page => counts.pages += 1,
                DocumentFormKind::Shared => counts.shared += 1,
                DocumentFormKind::Thumbnails => counts.thumbnails += 1,
                DocumentFormKind::Other => counts.other += 1,
            }
        }

        counts
    }

    /// Counts the top-level chunks by known `DjVu` role.
    ///
    /// # Errors
    ///
    /// Returns an error if a top-level `FORM` chunk cannot be parsed.
    pub fn root_chunk_counts(&self, bytes: &[u8]) -> Result<RootChunkCounts> {
        let mut counts = RootChunkCounts::default();

        for chunk in &self.root_chunks {
            match chunk.id {
                "DIRM" => counts.dirm += 1,
                "NAVM" => counts.navm += 1,
                "FORM" => match parse_form_at(bytes, chunk.data_start - 8)?.kind {
                    "DJVU" => counts.djvu_forms += 1,
                    "DJVI" => counts.djvi_forms += 1,
                    "THUM" => counts.thum_forms += 1,
                    _ => counts.other += 1,
                },
                _ => counts.other += 1,
            }
        }

        Ok(counts)
    }
}

impl DocumentFormKind {
    #[must_use]
    pub fn from_form_kind(kind: &str) -> Self {
        match kind {
            "DJVU" => Self::Page,
            "DJVI" => Self::Shared,
            "THUM" => Self::Thumbnails,
            _ => Self::Other,
        }
    }
}

impl<'a> DocumentForm<'a> {
    /// Returns this form as a page view when it is a `FORM:DJVU` page.
    ///
    /// # Errors
    ///
    /// Returns an error if the page form's child chunk stream is malformed.
    pub fn page(&self, bytes: &'a [u8]) -> Result<Option<Page<'a>>> {
        if self.kind != DocumentFormKind::Page {
            return Ok(None);
        }

        read_page_info(bytes, &self.form).map(|info| {
            Some(Page {
                offset: self.offset,
                form: self.form.clone(),
                info,
            })
        })
    }
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct FormKindCounts {
    pub pages: usize,
    pub shared: usize,
    pub thumbnails: usize,
    pub other: usize,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct RootChunkCounts {
    pub dirm: usize,
    pub navm: usize,
    pub djvu_forms: usize,
    pub djvi_forms: usize,
    pub thum_forms: usize,
    pub other: usize,
}

pub struct Pages<'a> {
    bytes: &'a [u8],
    forms: std::slice::Iter<'a, DocumentForm<'a>>,
}

impl<'a> Iterator for Pages<'a> {
    type Item = Result<Page<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        for document_form in self.forms.by_ref() {
            if document_form.kind != DocumentFormKind::Page {
                continue;
            }

            return Some(
                read_page_info(self.bytes, &document_form.form).map(|info| Page {
                    offset: document_form.offset,
                    form: document_form.form.clone(),
                    info,
                }),
            );
        }

        None
    }
}

/// Parses the top-level `DjVu` document `FORM`.
///
/// # Errors
///
/// Returns an error if the byte slice is truncated, does not start with the
/// `DjVu` magic bytes, or has an unsupported root form kind.
pub fn parse_document_root(bytes: &[u8]) -> Result<Form<'_>> {
    require_range(bytes, 0, 4)?;
    if &bytes[0..4] != DJVU_MAGIC {
        return Err(ParseError("missing DjVu magic bytes `AT&T`".to_string()));
    }

    let root = parse_form_at(bytes, 4)?;
    match root.kind {
        "DJVU" | "DJVM" => Ok(root),
        other => Err(ParseError(format!(
            "unexpected DjVu root FORM kind `{other}`"
        ))),
    }
}

/// Parses a `FORM` chunk at an absolute byte offset.
///
/// # Errors
///
/// Returns an error if the chunk is truncated, is not a `FORM`, or is too small
/// to contain its four-byte form kind.
pub fn parse_form_at(bytes: &[u8], start: usize) -> Result<Form<'_>> {
    let chunk = parse_chunk_at(bytes, start)?;
    if chunk.id != "FORM" {
        return Err(ParseError(format!(
            "expected FORM at offset {start}, found {}",
            chunk.id
        )));
    }

    if chunk.size < 4 {
        return Err(ParseError(format!(
            "FORM at offset {start} is too small to contain a form kind"
        )));
    }

    let kind = ascii_tag(bytes, chunk.data_start)?;
    Ok(Form {
        children_start: chunk.data_start + 4,
        chunk,
        kind,
    })
}

/// Parses sibling chunks in the half-open byte range `start..end`.
///
/// # Errors
///
/// Returns an error if any child chunk is malformed or extends beyond the
/// supplied parent range.
pub fn parse_chunks(bytes: &[u8], start: usize, end: usize) -> Result<Vec<Chunk<'_>>> {
    let mut chunks = Vec::new();
    let mut cursor = start;

    while cursor < end {
        let chunk = parse_chunk_at(bytes, cursor)?;
        if chunk.data_end > end {
            return Err(ParseError(format!(
                "chunk {} at offset {cursor} extends past parent end {end}",
                chunk.id
            )));
        }

        let next_start = chunk.next_start;
        chunks.push(chunk);
        cursor = next_start;
    }

    Ok(chunks)
}

/// Parses one chunk at an absolute byte offset.
///
/// # Errors
///
/// Returns an error if the chunk header, payload, or required padding byte is
/// outside the available byte slice.
pub fn parse_chunk_at(bytes: &[u8], start: usize) -> Result<Chunk<'_>> {
    require_range(bytes, start, 8)?;

    let id = ascii_tag(bytes, start)?;
    let size = read_u32_be(bytes, start + 4)?;
    let data_start = start + 8;
    let data_end = checked_add(data_start, size as usize)?;
    require_range(bytes, data_start, size as usize)?;

    let next_start = checked_add(data_end, (size & 1) as usize)?;
    if next_start > bytes.len() {
        return Err(ParseError(format!(
            "chunk {id} at offset {start} has invalid padding"
        )));
    }

    Ok(Chunk {
        id,
        size,
        data_start,
        data_end,
        next_start,
    })
}

/// Parses the currently understood, uncompressed prefix of a `DIRM` chunk.
///
/// # Errors
///
/// Returns an error if the chunk is too small for the declared directory offset
/// table.
pub fn parse_dirm(bytes: &[u8], chunk: &Chunk<'_>) -> Result<Dirm> {
    require_range(bytes, chunk.data_start, 3)?;

    let flags = bytes[chunk.data_start];
    let entry_count = read_u16_be(bytes, chunk.data_start + 1)?;
    let offsets_start = chunk.data_start + 3;
    let offsets_len = usize::from(entry_count) * 4;
    let compressed_tail_start = checked_add(offsets_start, offsets_len)?;

    if compressed_tail_start > chunk.data_end {
        return Err(ParseError(format!(
            "DIRM declares {entry_count} entries, but the chunk is too small for their offsets"
        )));
    }

    let mut offsets = Vec::with_capacity(usize::from(entry_count));
    for index in 0..usize::from(entry_count) {
        offsets.push(read_u32_be(bytes, offsets_start + index * 4)?);
    }

    Ok(Dirm {
        flags,
        entry_count,
        offsets,
        compressed_tail_len: chunk.data_end - compressed_tail_start,
    })
}

/// Reads the first `INFO` chunk from a `FORM:DJVU` page, if present.
///
/// # Errors
///
/// Returns an error if the page form's child chunk stream is malformed.
pub fn read_page_info(bytes: &[u8], form: &Form<'_>) -> Result<Option<PageInfo>> {
    if form.kind != "DJVU" {
        return Ok(None);
    }

    let children = parse_chunks(bytes, form.children_start, form.chunk.data_end)?;
    let Some(info_chunk) = children.first().filter(|chunk| chunk.id == "INFO") else {
        return Ok(None);
    };

    if info_chunk.size < 10 {
        return Ok(None);
    }

    let start = info_chunk.data_start;
    Ok(Some(PageInfo {
        width: read_u16_be(bytes, start)?,
        height: read_u16_be(bytes, start + 2)?,
        version: bytes[start + 4],
        dpi: read_u16_be(bytes, start + 5)?,
        gamma: f32::from(bytes[start + 8]) / 10.0,
        rotation: bytes[start + 9],
    }))
}

fn ascii_tag(bytes: &[u8], start: usize) -> Result<&str> {
    require_range(bytes, start, 4)?;
    std::str::from_utf8(&bytes[start..start + 4])
        .map_err(|_| ParseError(format!("non-ASCII chunk tag at offset {start}")))
}

fn read_u16_be(bytes: &[u8], start: usize) -> Result<u16> {
    require_range(bytes, start, 2)?;
    Ok(u16::from_be_bytes([bytes[start], bytes[start + 1]]))
}

fn read_u32_be(bytes: &[u8], start: usize) -> Result<u32> {
    require_range(bytes, start, 4)?;
    Ok(u32::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ]))
}

fn checked_add(lhs: usize, rhs: usize) -> Result<usize> {
    lhs.checked_add(rhs)
        .ok_or_else(|| ParseError("offset overflow".to_string()))
}

fn require_range(bytes: &[u8], start: usize, len: usize) -> Result<()> {
    let end = checked_add(start, len)?;
    if end > bytes.len() {
        return Err(ParseError(format!(
            "need bytes [{start}..{end}), but file only has {} bytes",
            bytes.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_chunk(bytes: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) {
        let payload_len = u32::try_from(payload.len()).expect("test payload should fit in u32");

        bytes.extend_from_slice(&id);
        bytes.extend_from_slice(&payload_len.to_be_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            bytes.push(0);
        }
    }

    fn chunk(id: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_chunk(&mut bytes, id, payload);
        bytes
    }

    fn form(kind: [u8; 4], children: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&kind);
        payload.extend_from_slice(children);
        chunk(*b"FORM", &payload)
    }

    fn info_chunk() -> Vec<u8> {
        chunk(*b"INFO", &[0x06, 0x18, 0x06, 0x61, 25, 0, 200, 0, 22, 1])
    }

    fn multipage_document_with_directory(offsets: &[u32], forms: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(DJVU_MAGIC);

        let mut root_children = Vec::new();
        let mut dirm_payload = vec![0x80, 0, u8::try_from(offsets.len()).unwrap()];
        for offset in offsets {
            dirm_payload.extend_from_slice(&offset.to_be_bytes());
        }
        push_chunk(&mut root_children, *b"DIRM", &dirm_payload);
        root_children.extend_from_slice(forms);

        bytes.extend_from_slice(&form(*b"DJVM", &root_children));
        bytes
    }

    #[test]
    fn parse_document_root_accepts_minimal_multipage_document() {
        let bytes = b"AT&TFORM\0\0\0\x04DJVM";

        let root = parse_document_root(bytes).expect("minimal root should parse");

        assert_eq!(root.kind, "DJVM");
        assert_eq!(root.children_start, 16);
        assert_eq!(root.chunk.data_end, 16);
    }

    #[test]
    fn parse_form_rejects_size_too_small_for_form_kind() {
        let bytes = b"FORM\0\0\0\x03ABC!";

        let error = parse_form_at(bytes, 0).expect_err("undersized FORM should fail");

        assert!(error.message().contains("too small"));
    }

    #[test]
    fn parse_chunks_accepts_final_child_padding_after_parent_data() {
        let bytes = b"ABCD\0\0\0\x01Z\0";

        let chunks = parse_chunks(bytes, 0, 9).expect("final padding may sit after parent data");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data_end, 9);
        assert_eq!(chunks[0].next_start, 10);
    }

    #[test]
    fn parse_chunks_accepts_odd_child_with_padding_inside_parent() {
        let bytes = b"ABCD\0\0\0\x01Z\0";

        let chunks = parse_chunks(bytes, 0, 10).expect("padded odd chunk should parse");

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, "ABCD");
        assert_eq!(chunks[0].data_start, 8);
        assert_eq!(chunks[0].data_end, 9);
        assert_eq!(chunks[0].next_start, 10);
    }

    #[test]
    fn document_parse_resolves_directory_forms_and_pages() {
        let shared_form = form(*b"DJVI", &[]);
        let shared_offset = 36;
        let page_offset = shared_offset + u32::try_from(shared_form.len()).unwrap();

        let mut forms = shared_form;
        forms.extend_from_slice(&form(*b"DJVU", &info_chunk()));
        let bytes = multipage_document_with_directory(&[shared_offset, page_offset], &forms);

        let document = Document::parse(&bytes).expect("document should parse");
        let root_counts = document
            .root_chunk_counts(&bytes)
            .expect("root counts should parse");
        let form_counts = document.form_kind_counts();
        let pages = document
            .pages(&bytes)
            .collect::<Result<Vec<_>>>()
            .expect("pages should parse");

        assert_eq!(root_counts.dirm, 1);
        assert_eq!(root_counts.djvu_forms, 1);
        assert_eq!(root_counts.djvi_forms, 1);
        assert_eq!(form_counts.pages, 1);
        assert_eq!(form_counts.shared, 1);
        assert_eq!(document.unresolved_directory_offsets, 0);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].offset, page_offset);
        assert_eq!(pages[0].info.as_ref().map(|info| info.dpi), Some(200));
    }

    #[test]
    fn document_parse_counts_unresolved_directory_offsets() {
        let bytes = multipage_document_with_directory(&[1_000], &[]);

        let document = Document::parse(&bytes).expect("document should parse");

        assert_eq!(document.forms.len(), 0);
        assert_eq!(document.unresolved_directory_offsets, 1);
    }
}
