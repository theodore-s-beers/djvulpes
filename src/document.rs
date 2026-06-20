use crate::chunk::{Chunk, Form, parse_chunks, parse_document_root, parse_form_at};
use crate::dirm::{Dirm, DirmTailEntry, parse_dirm};
use crate::error::{ParseError, ParseResult};
use crate::info::{PageInfo, read_page_info};
use crate::page::{PageChunk, PageChunkPayload, PageDetails, read_page_details};

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DirectoryEntry<'a> {
    pub offset: u32,
    pub size: u32,
    pub flags: u8,
    pub name: &'a str,
    pub form_index: Option<usize>,
    pub kind: Option<DocumentFormKind>,
}

#[derive(Debug, Clone)]
pub struct Page<'a> {
    pub offset: u32,
    pub form: Form<'a>,
    pub info: Option<PageInfo>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPageChunk<'a> {
    pub source: PageChunkSource<'a>,
    pub chunk: PageChunk<'a>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PageChunkSource<'a> {
    Page,
    Include { id: &'a str, offset: u32 },
}

impl<'a> Page<'a> {
    /// Reads typed structural details for this page.
    ///
    /// # Errors
    ///
    /// Returns an error if the page form's child chunk stream is malformed.
    pub fn details(&self, bytes: &'a [u8]) -> ParseResult<PageDetails<'a>> {
        read_page_details(bytes, &self.form)
    }
}

impl<'a> Document<'a> {
    /// Parses a `DjVu` document into a document-level view.
    ///
    /// # Errors
    ///
    /// Returns an error if the root document form, root chunk stream, or
    /// directory chunk is malformed.
    pub fn parse(bytes: &'a [u8]) -> ParseResult<Self> {
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
    pub fn root_chunk_counts(&self, bytes: &[u8]) -> ParseResult<RootChunkCounts> {
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

    #[must_use]
    pub fn directory_entries<'tail>(
        &self,
        tail_entries: &'tail [DirmTailEntry<'tail>],
    ) -> Vec<DirectoryEntry<'tail>> {
        tail_entries
            .iter()
            .map(|entry| {
                let form_index = self
                    .forms
                    .iter()
                    .position(|form| form.offset == entry.offset);
                let kind = form_index.map(|index| self.forms[index].kind);

                DirectoryEntry {
                    offset: entry.offset,
                    size: entry.size,
                    flags: entry.flags,
                    name: entry.name,
                    form_index,
                    kind,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn find_directory_entry_by_name<'tail>(
        &self,
        tail_entries: &'tail [DirmTailEntry<'tail>],
        name: &str,
    ) -> Option<DirectoryEntry<'tail>> {
        tail_entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| {
                let form_index = self
                    .forms
                    .iter()
                    .position(|form| form.offset == entry.offset);
                let kind = form_index.map(|index| self.forms[index].kind);

                DirectoryEntry {
                    offset: entry.offset,
                    size: entry.size,
                    flags: entry.flags,
                    name: entry.name,
                    form_index,
                    kind,
                }
            })
    }

    /// Reads a page's own chunks with `INCL` references expanded to their shared
    /// form chunks.
    ///
    /// # Errors
    ///
    /// Returns an error if the page chunks are malformed, an include cannot be
    /// resolved through the supplied directory tail entries, or an include points
    /// at a non-shared form.
    pub fn resolved_page_chunks<'tail>(
        &'a self,
        bytes: &'a [u8],
        page: &Page<'a>,
        tail_entries: &'tail [DirmTailEntry<'tail>],
    ) -> ParseResult<Vec<ResolvedPageChunk<'a>>> {
        let details = page.details(bytes)?;
        let directory_entries = self.directory_entries(tail_entries);
        let mut chunks = Vec::new();

        for page_chunk in details.chunks {
            let PageChunkPayload::Include { id } = page_chunk.payload else {
                chunks.push(ResolvedPageChunk {
                    source: PageChunkSource::Page,
                    chunk: page_chunk,
                });
                continue;
            };

            let Some(entry) = directory_entries.iter().find(|entry| entry.name == id) else {
                return Err(ParseError(format!(
                    "page include `{id}` does not match a directory entry"
                )));
            };
            let Some(form_index) = entry.form_index else {
                return Err(ParseError(format!(
                    "page include `{id}` points to unresolved form offset {}",
                    entry.offset
                )));
            };
            let included_form = &self.forms[form_index];
            if included_form.kind != DocumentFormKind::Shared {
                return Err(ParseError(format!(
                    "page include `{id}` points to FORM:{} instead of shared FORM:DJVI",
                    included_form.form.kind
                )));
            }

            for chunk in read_page_details(bytes, &included_form.form)?.chunks {
                chunks.push(ResolvedPageChunk {
                    source: PageChunkSource::Include {
                        id,
                        offset: included_form.offset,
                    },
                    chunk,
                });
            }
        }

        Ok(chunks)
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
    pub fn page(&self, bytes: &'a [u8]) -> ParseResult<Option<Page<'a>>> {
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
    type Item = ParseResult<Page<'a>>;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::DJVU_MAGIC;

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
            .collect::<ParseResult<Vec<_>>>()
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

    #[test]
    fn document_resolves_directory_tail_entries_to_forms() {
        let shared_form = form(*b"DJVI", &[]);
        let shared_offset = 36;
        let page_offset = shared_offset + u32::try_from(shared_form.len()).unwrap();

        let mut forms = shared_form;
        forms.extend_from_slice(&form(*b"DJVU", &info_chunk()));
        let bytes = multipage_document_with_directory(&[shared_offset, page_offset], &forms);
        let document = Document::parse(&bytes).expect("document should parse");
        let tail_entries = vec![
            DirmTailEntry {
                offset: shared_offset,
                size: 20,
                flags: 0,
                name: "shared.djbz",
            },
            DirmTailEntry {
                offset: page_offset,
                size: 30,
                flags: 1,
                name: "page.djvu",
            },
        ];

        let entries = document.directory_entries(&tail_entries);
        let shared = document
            .find_directory_entry_by_name(&tail_entries, "shared.djbz")
            .expect("shared entry should resolve");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].form_index, Some(0));
        assert_eq!(entries[0].kind, Some(DocumentFormKind::Shared));
        assert_eq!(entries[1].form_index, Some(1));
        assert_eq!(entries[1].kind, Some(DocumentFormKind::Page));
        assert_eq!(shared, entries[0]);
        assert_eq!(
            document.find_directory_entry_by_name(&tail_entries, "missing"),
            None
        );
    }

    #[test]
    fn resolved_page_chunks_expands_shared_includes() {
        let mut shared_children = Vec::new();
        shared_children.extend_from_slice(&chunk(*b"Djbz", b"dictionary"));
        let shared_form = form(*b"DJVI", &shared_children);
        let shared_offset = 36;
        let page_offset = shared_offset + u32::try_from(shared_form.len()).unwrap();

        let mut page_children = info_chunk();
        page_children.extend_from_slice(&chunk(*b"INCL", b"shared"));
        page_children.extend_from_slice(&chunk(*b"Sjbz", b"image"));

        let mut forms = shared_form;
        forms.extend_from_slice(&form(*b"DJVU", &page_children));
        let bytes = multipage_document_with_directory(&[shared_offset, page_offset], &forms);
        let document = Document::parse(&bytes).expect("document should parse");
        let page = document
            .pages(&bytes)
            .next()
            .expect("page should exist")
            .expect("page should parse");
        let tail_entries = vec![
            DirmTailEntry {
                offset: shared_offset,
                size: 20,
                flags: 0,
                name: "shared",
            },
            DirmTailEntry {
                offset: page_offset,
                size: 30,
                flags: 1,
                name: "page",
            },
        ];

        let chunks = document
            .resolved_page_chunks(&bytes, &page, &tail_entries)
            .expect("page includes should resolve");

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].source, PageChunkSource::Page);
        assert_eq!(chunks[0].chunk.chunk.id, "INFO");
        assert_eq!(
            chunks[1].source,
            PageChunkSource::Include {
                id: "shared",
                offset: shared_offset
            }
        );
        assert_eq!(chunks[1].chunk.chunk.id, "Djbz");
        assert_eq!(chunks[2].source, PageChunkSource::Page);
        assert_eq!(chunks[2].chunk.chunk.id, "Sjbz");
    }

    #[test]
    fn resolved_page_chunks_rejects_unresolved_include() {
        let page_offset = 32;
        let mut page_children = info_chunk();
        page_children.extend_from_slice(&chunk(*b"INCL", b"missing"));
        let page_form = form(*b"DJVU", &page_children);
        let bytes = multipage_document_with_directory(&[page_offset], &page_form);
        let document = Document::parse(&bytes).expect("document should parse");
        let page = document
            .pages(&bytes)
            .next()
            .expect("page should exist")
            .expect("page should parse");

        let error = document
            .resolved_page_chunks(&bytes, &page, &[])
            .expect_err("missing include should fail");

        assert!(
            error.message().contains("does not match a directory entry"),
            "{error}"
        );
    }
}
