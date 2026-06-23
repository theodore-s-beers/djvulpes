use djvulpes::{
    Document, Iw44LayerRole, PageChunkKind, PageRenderMode, extract_document_bookmarks,
    format_document_text_zones, render_document_page, render_document_pdf,
};

const JOHNSON: &[u8] = include_bytes!("../fixtures/johnson-persian.djvu");

#[test]
fn johnson_document_structure_is_supported() {
    let document = Document::parse(JOHNSON).expect("Johnson should parse");
    let form_counts = document.form_kind_counts();
    let root_counts = document
        .root_chunk_counts(JOHNSON)
        .expect("Johnson root chunks should parse");

    assert_eq!(form_counts.pages, 272);
    assert_eq!(form_counts.shared, 0);
    assert_eq!(form_counts.thumbnails, 0);
    assert_eq!(form_counts.other, 0);
    assert_eq!(root_counts.navm, 0);
    assert_eq!(root_counts.other, 0);

    let first_page = document
        .pages(JOHNSON)
        .next()
        .expect("Johnson should have a first page")
        .expect("Johnson first page should parse");
    assert_eq!(
        first_page
            .info
            .map(|info| (info.width, info.height, info.dpi)),
        Some((2099, 2853, 400))
    );
}

#[test]
fn johnson_cida_page_chunks_are_classified_as_known_metadata() {
    let document = Document::parse(JOHNSON).expect("Johnson should parse");
    let page = document
        .pages(JOHNSON)
        .nth(1)
        .expect("Johnson should have page 2")
        .expect("Johnson page 2 should parse");
    let details = page
        .details(JOHNSON)
        .expect("Johnson page 2 details should parse");

    assert_eq!(
        details
            .chunks
            .iter()
            .filter(|chunk| chunk.kind == PageChunkKind::Cida)
            .count(),
        1
    );
    assert_eq!(
        details
            .chunks
            .iter()
            .filter(|chunk| chunk.kind == PageChunkKind::Unknown)
            .count(),
        0
    );
}

#[test]
fn johnson_has_no_outline() {
    let bookmarks = extract_document_bookmarks(JOHNSON).expect("bookmark extraction should parse");

    assert!(bookmarks.is_none());
}

#[test]
fn johnson_page_2_structured_text_uses_bottom_left_coordinates() {
    let text =
        format_document_text_zones(JOHNSON, 2, Some(2)).expect("page 2 text zones should format");

    assert!(text.starts_with("(page 761 1574 1127 1699\n"));
    assert!(text.contains("(word 803 1664 1001 1699 \"Copyright,\")"));
    assert!(!text.contains("(page 761 -"));
}

#[test]
fn johnson_representative_text_page_renders() {
    let render = render_document_page(JOHNSON, 2, PageRenderMode::Full)
        .expect("Johnson page 2 should render");
    let stats = render.bitmap.stats();

    assert_eq!((render.bitmap.width, render.bitmap.height), (1882, 2870));
    assert_eq!(render.bitonal_masks.len(), 1);
    assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 6_779);
    assert_eq!(render.iw44_layers.len(), 2);
    assert_eq!(render.iw44_layers[0].role, Iw44LayerRole::Background);
    assert_eq!(render.iw44_layers[0].geometry.mapping.subsample, 3);
    assert_eq!(render.iw44_layers[1].role, Iw44LayerRole::Foreground);
    assert_eq!(render.iw44_layers[1].geometry.mapping.subsample, 12);
    assert_eq!(stats.component_sum, 3_412_755_679);
    assert_eq!(stats.fingerprint, 9_707_102_074_976_462_725);
}

#[test]
fn johnson_last_page_renders() {
    let render = render_document_page(JOHNSON, 272, PageRenderMode::Full)
        .expect("Johnson final page should render");
    let stats = render.bitmap.stats();

    assert_eq!((render.bitmap.width, render.bitmap.height), (2075, 3047));
    assert_eq!(render.bitonal_masks.len(), 1);
    assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 4_217);
    assert_eq!(render.iw44_layers.len(), 2);
    assert_eq!(stats.black_pixels, 12);
    assert_eq!(stats.component_sum, 604_991_883);
    assert_eq!(stats.fingerprint, 7_148_341_397_051_384_248);
}

#[test]
fn johnson_single_page_pdf_renders() {
    let pdf = render_document_pdf(JOHNSON, 2, Some(2)).expect("page 2 should render to PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.starts_with("%PDF-1.4\n"));
    assert!(text.contains("/Type /Pages /Count 1"));
    assert!(text.contains("/Subtype /Image /Width 1882 /Height 2870"));
    assert!(pdf.ends_with(b"%%EOF\n"));
}
