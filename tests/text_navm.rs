use djvulpes::{
    TextError, extract_document_bookmarks, extract_document_text, extract_document_text_pages,
    format_document_text_zones,
};

const RYPKA: &[u8] = include_bytes!("../fixtures/Rypka-HIL.djvu");

#[test]
fn extract_document_text_matches_djvutxt_page_separator_convention() {
    let text = extract_document_text(RYPKA, 3, Some(3)).expect("page 3 text should extract");

    assert_eq!(text, "HISTORY OF IRANIAN LITERATURE \n\n\x0c");
}

#[test]
fn format_document_text_zones_matches_djvused_page_expression() {
    let text = format_document_text_zones(RYPKA, 3, Some(3)).expect("page 3 zones should format");

    assert_eq!(
        text,
        concat!(
            "(page 0 0 3444 5088\n",
            " (line 814 4415 2612 4480\n",
            "  (word 814 4415 1255 4476 \"HISTORY\")\n",
            "  (word 1305 4416 1429 4477 \"OF\")\n",
            "  (word 1480 4416 1922 4480 \"IRANIAN\")\n",
            "  (word 1972 4416 2612 4478 \"LITERATURE\")))\n",
        )
    );
}

#[test]
fn format_document_text_zones_preserves_empty_pages() {
    let text = format_document_text_zones(RYPKA, 1, Some(3)).expect("page range should format");

    assert_eq!(
        text,
        concat!(
            "(page 0 0 0 0 \"\")\n",
            "(page 0 0 0 0 \"\")\n",
            "(page 0 0 3444 5088\n",
            " (line 814 4415 2612 4480\n",
            "  (word 814 4415 1255 4476 \"HISTORY\")\n",
            "  (word 1305 4416 1429 4477 \"OF\")\n",
            "  (word 1480 4416 1922 4480 \"IRANIAN\")\n",
            "  (word 1972 4416 2612 4478 \"LITERATURE\")))\n",
        )
    );
}

#[test]
fn extract_document_text_pages_preserves_empty_pages() {
    let pages = extract_document_text_pages(RYPKA, 1, Some(3)).expect("text range should extract");

    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].page_number, 1);
    assert_eq!(pages[0].text, "");
    assert_eq!(pages[2].page_number, 3);
    assert_eq!(pages[2].text, "HISTORY OF IRANIAN LITERATURE \n");
}

#[test]
fn extract_document_text_rejects_invalid_ranges() {
    assert!(matches!(
        extract_document_text(RYPKA, 0, None),
        Err(TextError::ZeroFromPage)
    ));
    assert!(matches!(
        extract_document_text(RYPKA, 3, Some(2)),
        Err(TextError::ReversedPageRange)
    ));
    assert!(matches!(
        extract_document_text(RYPKA, 962, None),
        Err(TextError::PageOutOfRange {
            page: 962,
            page_count: 961
        })
    ));
}

#[test]
fn extract_document_bookmarks_reads_rypka_outline() {
    let bookmarks = extract_document_bookmarks(RYPKA)
        .expect("bookmark extraction should parse")
        .expect("Rypka should contain bookmarks");

    assert_eq!(count_bookmarks(&bookmarks), 389);
    assert_eq!(bookmarks.len(), 20);
    assert_eq!(bookmarks[0].title, "Cover ");
    assert_eq!(bookmarks[0].url, "#1");
}

fn count_bookmarks(bookmarks: &[djvulpes::Bookmark]) -> usize {
    bookmarks
        .iter()
        .map(|bookmark| 1 + count_bookmarks(&bookmark.children))
        .sum()
}
