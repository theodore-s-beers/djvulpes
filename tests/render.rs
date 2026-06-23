use djvulpes::{
    DjvuPageRenderEvent, DjvuRenderError, PageRenderMode, render_document_page,
    render_document_pages, render_document_pages_with_events,
};

const RYPKA: &[u8] = include_bytes!("../fixtures/Rypka-HIL.djvu");

#[test]
fn render_document_page_rejects_invalid_page_numbers() {
    assert!(matches!(
        render_document_page(RYPKA, 0, PageRenderMode::Full).expect_err("zero page should fail"),
        DjvuRenderError::ZeroPage
    ));
    let missing =
        render_document_page(RYPKA, 10_000, PageRenderMode::Full).expect_err("page should fail");
    let DjvuRenderError::PageOutOfRange { page, page_count } = missing else {
        panic!("expected page range error, got {missing}");
    };
    assert_eq!(page, 10_000);
    assert!(page_count > 0);
}

#[test]
fn render_document_page_renders_fixture_page_layer_modes() {
    let foreground = render_document_page(RYPKA, 1, PageRenderMode::Foreground)
        .expect("foreground page should render");
    let mask =
        render_document_page(RYPKA, 1, PageRenderMode::Mask).expect("mask page should render");

    assert_eq!(
        (foreground.bitmap.width, foreground.bitmap.height),
        (1560, 1633)
    );
    assert_eq!(foreground.iw44_layers.len(), 1);
    assert_eq!(foreground.bitonal_masks.len(), 1);
    assert!(foreground.bitmap.stats().black_pixels < 167_493);
    assert!(mask.iw44_layers.is_empty());
    assert_eq!(mask.bitonal_masks.len(), 1);
    assert_eq!(mask.bitmap.stats().black_pixels, 167_493);
}

#[test]
fn render_document_pages_rejects_invalid_page_ranges() {
    assert!(matches!(
        render_document_pages(RYPKA, 0, None, PageRenderMode::Full)
            .expect_err("zero from page should fail"),
        DjvuRenderError::ZeroFromPage
    ));
    assert!(matches!(
        render_document_pages(RYPKA, 2, Some(1), PageRenderMode::Full)
            .expect_err("reversed range should fail"),
        DjvuRenderError::ReversedPageRange
    ));
}

#[test]
fn render_document_pages_with_events_renders_fixture_range() {
    let mut events = Vec::new();

    let renders =
        render_document_pages_with_events(RYPKA, 68, Some(68), PageRenderMode::Full, |event| {
            match event {
                DjvuPageRenderEvent::PageStarted {
                    page_number,
                    end_page,
                } => events.push((page_number, end_page, false, 0, 0)),
                DjvuPageRenderEvent::PageRendered {
                    page_number,
                    render,
                } => events.push((
                    page_number,
                    page_number,
                    true,
                    render.bitmap.width,
                    render.bitmap.height,
                )),
            }
        })
        .expect("fixture range should render");

    assert_eq!(renders.len(), 1);
    assert_eq!(events, [(68, 68, false, 0, 0), (68, 68, true, 3423, 5075),]);
    assert_eq!(renders[0].page_number, 68);
    assert_eq!(
        (
            renders[0].render.bitmap.width,
            renders[0].render.bitmap.height
        ),
        (3423, 5075)
    );
}
