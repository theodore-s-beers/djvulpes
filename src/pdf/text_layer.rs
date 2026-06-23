use super::{DjvuPdfResult, PdfError, PdfObjectWriter, PdfResult, points_from_signed_pixels};
use crate::{Bookmark, TextZone, TextZoneKind, extract_document_text_zone_pages};
use std::{collections::BTreeMap, fmt::Write as _, io::Write};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct PdfBookmark {
    pub(super) title: String,
    pub(super) page_index: usize,
    pub(super) children: Vec<Self>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct NumberedPdfBookmark {
    id: usize,
    title: String,
    page_index: usize,
    children: Vec<Self>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct PdfTextPage {
    pub(super) spans: Vec<PdfTextSpan>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct PdfTextSpan {
    text: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct PdfTextEncoding {
    custom_codes: BTreeMap<char, u8>,
}

pub(super) fn pdf_text_pages_from_djvu(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> DjvuPdfResult<Vec<PdfTextPage>> {
    extract_document_text_zone_pages(bytes, from_page, to_page)?
        .iter()
        .map(|page| {
            let mut spans = Vec::new();
            if let Some(zone) = &page.zone {
                collect_pdf_text_spans(zone, &page.text, &mut spans);
            }
            Ok(PdfTextPage { spans })
        })
        .collect()
}

fn collect_pdf_text_spans(zone: &TextZone, text: &str, spans: &mut Vec<PdfTextSpan>) {
    if matches!(zone.kind, TextZoneKind::Line) {
        let mut words = Vec::new();
        collect_pdf_line_words(zone, text, &mut words);
        let line = words.join(" ");
        if !line.is_empty() {
            spans.push(PdfTextSpan {
                text: line,
                x: zone.x_min(),
                y: zone.y_min(),
                width: zone.width,
                height: zone.height,
            });
        }
        return;
    }

    if matches!(zone.kind, TextZoneKind::Word) {
        let word = pdf_text_zone_word(zone, text);
        if !word.is_empty() {
            spans.push(PdfTextSpan {
                text: word,
                x: zone.x_min(),
                y: zone.y_min(),
                width: zone.width,
                height: zone.height,
            });
        }
        return;
    }

    for child in &zone.children {
        collect_pdf_text_spans(child, text, spans);
    }
}

fn collect_pdf_line_words(zone: &TextZone, text: &str, words: &mut Vec<String>) {
    if matches!(zone.kind, TextZoneKind::Word) {
        let word = pdf_text_zone_word(zone, text);
        if !word.is_empty() {
            words.push(word);
        }
        return;
    }

    for child in &zone.children {
        collect_pdf_line_words(child, text, words);
    }
}

fn pdf_text_zone_word(zone: &TextZone, text: &str) -> String {
    text.get(zone.text_start..zone.text_end())
        .unwrap_or("")
        .trim_end()
        .to_string()
}

pub(super) fn pdf_bookmarks_from_djvu(
    bookmarks: &[Bookmark],
    from_page: usize,
    end_page: usize,
) -> Vec<PdfBookmark> {
    let mut pdf_bookmarks = Vec::new();
    for bookmark in bookmarks {
        let children = pdf_bookmarks_from_djvu(&bookmark.children, from_page, end_page);
        match djvu_bookmark_page(&bookmark.url) {
            Some(page) if (from_page..=end_page).contains(&page) => {
                pdf_bookmarks.push(PdfBookmark {
                    title: bookmark.title.clone(),
                    page_index: page - from_page,
                    children,
                });
            }
            _ => pdf_bookmarks.extend(children),
        }
    }

    pdf_bookmarks
}

fn djvu_bookmark_page(url: &str) -> Option<usize> {
    url.strip_prefix('#')?.parse().ok()
}

pub(super) fn append_pdf_text_layer(
    content: &mut String,
    page: &PdfTextPage,
    dpi: u16,
    encoding: &PdfTextEncoding,
) {
    if page.spans.is_empty() {
        return;
    }

    content.push_str("q\nBT\n3 Tr\n/Ftxt 1 Tf\n");
    for span in &page.spans {
        let x = points_from_signed_pixels(span.x, dpi);
        let y = points_from_signed_pixels(span.y, dpi);
        let height = points_from_signed_pixels(span.height.max(1), dpi);
        let encoded_text = encoding.encode(&span.text);
        write!(
            content,
            "/Span << /ActualText {} >> BDC\n{height} 0 0 {height} {x} {y} Tm\n{} Tj\nEMC\n",
            pdf_text_string(&span.text),
            pdf_literal_bytes(&encoded_text),
        )
        .expect("writing to string should not fail");
    }
    content.push_str("ET\nQ\n");
}

fn pdf_literal_bytes(value: &[u8]) -> String {
    let mut escaped = String::from("(");
    for byte in value {
        match *byte {
            b'(' => escaped.push_str("\\("),
            b')' => escaped.push_str("\\)"),
            b'\\' => escaped.push_str("\\\\"),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            0x20..=0x7e => escaped.push(char::from(*byte)),
            _ => write!(&mut escaped, "\\{byte:03o}").expect("writing to string should not fail"),
        }
    }
    escaped.push(')');

    escaped
}

pub(super) fn count_pdf_bookmarks(bookmarks: &[PdfBookmark]) -> usize {
    bookmarks
        .iter()
        .map(|bookmark| 1 + count_pdf_bookmarks(&bookmark.children))
        .sum()
}

pub(super) fn number_pdf_bookmarks(
    bookmarks: &[PdfBookmark],
    next_id: &mut usize,
) -> Vec<NumberedPdfBookmark> {
    bookmarks
        .iter()
        .map(|bookmark| {
            let id = *next_id;
            *next_id += 1;
            let children = number_pdf_bookmarks(&bookmark.children, next_id);
            NumberedPdfBookmark {
                id,
                title: bookmark.title.clone(),
                page_index: bookmark.page_index,
                children,
            }
        })
        .collect()
}

pub(super) fn write_pdf_outlines<W: Write>(
    pdf: &mut PdfObjectWriter<W>,
    root_id: usize,
    first_page_id: usize,
    bookmarks: &[NumberedPdfBookmark],
) -> PdfResult<()> {
    let first_id = bookmarks
        .first()
        .expect("outline root should have at least one bookmark")
        .id;
    let last_id = bookmarks
        .last()
        .expect("outline root should have at least one bookmark")
        .id;
    let count = count_numbered_pdf_bookmarks(bookmarks);
    pdf.write_object(
        root_id,
        format!("<< /Type /Outlines /First {first_id} 0 R /Last {last_id} 0 R /Count {count} >>")
            .as_bytes(),
    )?;
    write_pdf_outline_items(pdf, root_id, first_page_id, bookmarks)
}

fn write_pdf_outline_items<W: Write>(
    pdf: &mut PdfObjectWriter<W>,
    parent_id: usize,
    first_page_id: usize,
    bookmarks: &[NumberedPdfBookmark],
) -> PdfResult<()> {
    for (index, bookmark) in bookmarks.iter().enumerate() {
        let mut dictionary = format!(
            "<< /Title {} /Parent {parent_id} 0 R /Dest [{} 0 R /Fit]",
            pdf_text_string(&bookmark.title),
            first_page_id + bookmark.page_index
        );
        if let Some(previous) = index.checked_sub(1).and_then(|index| bookmarks.get(index)) {
            write!(&mut dictionary, " /Prev {} 0 R", previous.id)
                .expect("writing to string should not fail");
        }
        if let Some(next) = bookmarks.get(index + 1) {
            write!(&mut dictionary, " /Next {} 0 R", next.id)
                .expect("writing to string should not fail");
        }
        if let Some(first_child) = bookmark.children.first() {
            let last_child = bookmark
                .children
                .last()
                .expect("non-empty child list should have last child");
            let child_count = count_numbered_pdf_bookmarks(&bookmark.children);
            write!(
                &mut dictionary,
                " /First {} 0 R /Last {} 0 R /Count {child_count}",
                first_child.id, last_child.id
            )
            .expect("writing to string should not fail");
        }
        dictionary.push_str(" >>");
        pdf.write_object(bookmark.id, dictionary.as_bytes())?;
        write_pdf_outline_items(pdf, bookmark.id, first_page_id, &bookmark.children)?;
    }

    Ok(())
}

fn count_numbered_pdf_bookmarks(bookmarks: &[NumberedPdfBookmark]) -> usize {
    bookmarks
        .iter()
        .map(|bookmark| 1 + count_numbered_pdf_bookmarks(&bookmark.children))
        .sum()
}

fn pdf_text_string(value: &str) -> String {
    let mut hex = String::from("<FEFF");
    for unit in value.encode_utf16() {
        write!(&mut hex, "{unit:04X}").expect("writing to string should not fail");
    }
    hex.push('>');

    hex
}

impl PdfTextEncoding {
    pub(super) fn from_pages(pages: &[PdfTextPage]) -> PdfResult<Self> {
        let mut custom_codes = BTreeMap::new();
        let mut next_code = 0x80u8;
        for span in pages.iter().flat_map(|page| &page.spans) {
            for character in span.text.chars() {
                if character.is_ascii_graphic() || character == ' ' {
                    continue;
                }
                if custom_codes.contains_key(&character) {
                    continue;
                }
                custom_codes.insert(character, next_code);
                next_code = next_code.checked_add(1).ok_or_else(|| {
                    PdfError::new("PDF text layer uses more than 128 non-ASCII characters")
                })?;
            }
        }

        Ok(Self { custom_codes })
    }

    fn encode(&self, value: &str) -> Vec<u8> {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_graphic() || character == ' ' {
                    u8::try_from(u32::from(character)).expect("ASCII character should fit in u8")
                } else {
                    self.custom_codes
                        .get(&character)
                        .copied()
                        .expect("PDF text character should have a custom encoding")
                }
            })
            .collect()
    }

    fn unicode_entries(&self) -> Vec<(u8, char)> {
        let mut entries = (0x20u8..=0x7e)
            .map(|code| (code, char::from(code)))
            .collect::<Vec<_>>();
        entries.extend(
            self.custom_codes
                .iter()
                .map(|(character, code)| (*code, *character)),
        );
        entries.sort_by_key(|(code, _)| *code);
        entries
    }
}

pub(super) fn pdf_to_unicode_cmap(encoding: &PdfTextEncoding) -> String {
    let entries = encoding.unicode_entries();
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /DjvulpesText def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n\
         <20> <FF>\n\
         endcodespacerange\n",
    );

    for chunk in entries.chunks(100) {
        writeln!(&mut cmap, "{} beginbfchar", chunk.len())
            .expect("writing to string should not fail");
        for (code, character) in chunk {
            writeln!(
                &mut cmap,
                "<{code:02X}> <{}>",
                pdf_utf16be_hex_char(*character)
            )
            .expect("writing to string should not fail");
        }
        cmap.push_str("endbfchar\n");
    }

    cmap.push_str(
        "endcmap\n\
         CMapName currentdict /CMap defineresource pop\n\
         end\n\
         end",
    );

    cmap
}

fn pdf_utf16be_hex_char(character: char) -> String {
    let mut hex = String::new();
    let mut units = [0u16; 2];
    for unit in character.encode_utf16(&mut units) {
        write!(&mut hex, "{unit:04X}").expect("writing to string should not fail");
    }

    hex
}
