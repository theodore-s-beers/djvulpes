use std::fs;

use djvulpes::{
    Document, Iw44LayerRole, PageRenderMode, extract_document_bookmarks,
    format_document_text_zones, render_document_page, render_document_pdf,
};

const BRINGHURST_PATH: &str = "bringhurst-typography.djvu";

fn bringhurst() -> Option<Vec<u8>> {
    fs::read(BRINGHURST_PATH).ok()
}

#[test]
fn bringhurst_document_structure_is_supported() {
    let Some(bytes) = bringhurst() else {
        return;
    };

    let document = Document::parse(&bytes).expect("Bringhurst should parse");
    let form_counts = document.form_kind_counts();
    let root_counts = document
        .root_chunk_counts(&bytes)
        .expect("Bringhurst root chunks should parse");

    assert_eq!(form_counts.pages, 382);
    assert_eq!(form_counts.shared, 1);
    assert_eq!(form_counts.thumbnails, 0);
    assert_eq!(form_counts.other, 0);
    assert_eq!(root_counts.navm, 0);
    assert_eq!(root_counts.other, 0);

    let first_page = document
        .pages(&bytes)
        .next()
        .expect("Bringhurst should have a first page")
        .expect("Bringhurst first page should parse");
    assert_eq!(
        first_page
            .info
            .map(|info| (info.width, info.height, info.dpi)),
        Some((1587, 2655, 600))
    );
}

#[test]
fn bringhurst_has_no_outline() {
    let Some(bytes) = bringhurst() else {
        return;
    };

    let bookmarks = extract_document_bookmarks(&bytes).expect("bookmark extraction should parse");

    assert!(bookmarks.is_none());
}

#[test]
fn bringhurst_page_1_iw44_layers_are_supported() {
    let Some(bytes) = bringhurst() else {
        return;
    };

    let render = render_document_page(&bytes, 1, PageRenderMode::Full)
        .expect("Bringhurst page 1 should render");

    assert_eq!(render.iw44_layers.len(), 2);
    let background = &render.iw44_layers[0];
    assert_eq!(background.role, Iw44LayerRole::Background);
    assert_eq!(background.chunk_indices, [3, 4, 5, 6]);
    assert_eq!(
        (background.image.width, background.image.height),
        (529, 885)
    );
    assert_eq!(background.geometry.mapping.subsample, 3);
    assert_eq!(background.geometry.mapping.scaled_width, 1587);
    assert_eq!(background.geometry.mapping.scaled_height, 2655);
    assert_eq!(background.geometry.mapping.horizontal_overscan, 0);
    assert_eq!(background.geometry.mapping.vertical_overscan, 0);

    let foreground = &render.iw44_layers[1];
    assert_eq!(foreground.role, Iw44LayerRole::Foreground);
    assert_eq!(foreground.chunk_indices, [2]);
    assert_eq!(
        (foreground.image.width, foreground.image.height),
        (133, 222)
    );
    assert_eq!(foreground.geometry.mapping.subsample, 12);
    assert_eq!(foreground.geometry.mapping.scaled_width, 1596);
    assert_eq!(foreground.geometry.mapping.scaled_height, 2664);
    assert_eq!(foreground.geometry.mapping.horizontal_overscan, 9);
    assert_eq!(foreground.geometry.mapping.vertical_overscan, 9);
}

#[test]
fn bringhurst_structured_text_uses_bottom_left_coordinates() {
    let Some(bytes) = bringhurst() else {
        return;
    };

    let text =
        format_document_text_zones(&bytes, 1, Some(1)).expect("page 1 text zones should format");

    assert!(text.starts_with("(page 167 314 1431 2461\n"));
    assert!(text.contains("  (region 796 314 1431 949\n"));
    assert!(!text.contains("(page 167 -314 1431 1833\n"));
}

#[test]
fn bringhurst_complex_page_mask_decodes_exact_black_pixel_count() {
    let Some(bytes) = bringhurst() else {
        return;
    };

    let render = render_document_page(&bytes, 203, PageRenderMode::Mask)
        .expect("page 203 mask should render");

    assert_eq!((render.bitmap.width, render.bitmap.height), (3220, 4970));
    assert_eq!(render.bitonal_masks.len(), 1);
    assert_eq!(
        render.bitonal_masks[0].1.mask.black_pixel_count(),
        1_282_047
    );
    assert_eq!(render.bitmap.stats().black_pixels, 1_282_047);
}

#[test]
fn bringhurst_single_page_pdf_renders() {
    let Some(bytes) = bringhurst() else {
        return;
    };

    let pdf = render_document_pdf(&bytes, 1, Some(1)).expect("page 1 should render to PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.starts_with("%PDF-1.4\n"));
    assert!(text.contains("/Type /Pages /Count 1"));
    assert!(text.contains("/Subtype /Image /Width 1587 /Height 2655"));
    assert!(pdf.ends_with(b"%%EOF\n"));
}
