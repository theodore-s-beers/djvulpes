use djvulpes::{
    DjvuPdfError, DjvuPdfRenderEvent, PdfPageImage, render_document_pdf,
    render_document_pdf_parallel, render_document_pdf_with_events,
};

const RYPKA: &[u8] = include_bytes!("../fixtures/Rypka-HIL.djvu");
const BRINGHURST: &[u8] = include_bytes!("../fixtures/bringhurst-typography.djvu");

#[test]
fn render_document_pdf_rejects_invalid_page_ranges() {
    assert!(matches!(
        render_document_pdf(RYPKA, 0, None).expect_err("zero page should fail"),
        DjvuPdfError::ZeroFromPage
    ));
    assert!(matches!(
        render_document_pdf(RYPKA, 2, Some(1)).expect_err("reversed range should fail"),
        DjvuPdfError::ReversedPageRange
    ));
    let missing_page =
        render_document_pdf(RYPKA, 10_000, None).expect_err("missing page should fail");
    let DjvuPdfError::PageOutOfRange { page, page_count } = missing_page else {
        panic!("expected page range error, got {missing_page}");
    };
    assert_eq!(page, 10_000);
    assert!(page_count > 0);
}

#[test]
fn render_document_pdf_with_events_renders_fixture_page_range() {
    let mut events = Vec::new();
    let pdf = render_document_pdf_with_events(RYPKA, 68, Some(68), |event| match event {
        DjvuPdfRenderEvent::PageStarted {
            page_number,
            end_page,
        } => {
            events.push((page_number, end_page, "started", 0, 0));
        }
        DjvuPdfRenderEvent::PageImagePrepared { page_number, image } => {
            let (width, height) = match image {
                PdfPageImage::Rgb8 { width, height, .. }
                | PdfPageImage::Gray8 { width, height, .. }
                | PdfPageImage::BitonalMask { width, height, .. } => (*width, *height),
            };
            events.push((page_number, page_number, "prepared", width, height));
        }
        DjvuPdfRenderEvent::PageRendered {
            page_number,
            render,
        } => {
            events.push((
                page_number,
                page_number,
                "rendered",
                render.bitmap.width,
                render.bitmap.height,
            ));
        }
    })
    .expect("fixture page should render to PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert_eq!(
        events,
        [(68, 68, "started", 0, 0), (68, 68, "prepared", 3423, 5075),]
    );
    assert!(text.contains("/Type /Pages /Count 1"));
    assert!(text.contains("/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"));
}

#[test]
fn render_document_pdf_parallel_matches_serial_bitonal_page_range() {
    let serial = render_document_pdf(RYPKA, 68, Some(70)).expect("serial PDF should render");
    let parallel =
        render_document_pdf_parallel(RYPKA, 68, Some(70), 4).expect("parallel PDF should render");

    assert_eq!(parallel, serial);
}

#[test]
fn render_document_pdf_parallel_matches_serial_rgb_page() {
    let serial = render_document_pdf(BRINGHURST, 1, Some(1)).expect("serial PDF should render");
    let parallel = render_document_pdf_parallel(BRINGHURST, 1, Some(1), 4)
        .expect("parallel PDF should render");

    assert_eq!(parallel, serial);
}

#[test]
fn render_document_pdf_embeds_rypka_page_961_iw44_render_as_rgb_image() {
    let pdf = render_document_pdf(RYPKA, 961, Some(961)).expect("IW44 page should render");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.starts_with("%PDF-1.4\n"));
    assert!(text.contains("/Type /Pages /Count 1"));
    assert!(text.contains("/Subtype /Image /Width 3486 /Height 2783"));
    assert!(text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
    assert!(!text.contains("/ImageMask true"));
    assert!(pdf.ends_with(b"%%EOF\n"));
}

#[test]
fn render_document_pdf_embeds_rypka_navm_outline_for_selected_range() {
    let pdf = render_document_pdf(RYPKA, 1, Some(1)).expect("cover page should render to PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.contains("/Outlines "));
    assert!(text.contains("/PageMode /UseOutlines"));
    assert!(text.contains("/Count 1"));
    assert!(text.contains("/Title <FEFF0043006F0076006500720020>"));
    assert!(text.contains("/Dest [3 0 R /Fit]"));
}

#[test]
fn render_document_pdf_embeds_unicode_navm_outline_title() {
    let pdf = render_document_pdf(RYPKA, 33, Some(33)).expect("unicode outline page should render");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.contains("/Outlines "));
    assert!(text.contains(
        "/Title <FEFF004F00540041004B004100520020004B004C00CD004D0041003A0020004100560045005300540041002E00200041004E004300490045004E00540020005000450052005300490041004E00200049004E0053004300520049005000540049004F004E0053002E0020004D004900440044004C00450020005000450052005300490041004E0020004C0049005400450052004100540055005200450020>"
    ));
}

#[test]
fn render_document_pdf_embeds_searchable_text_layer() {
    let pdf = render_document_pdf(RYPKA, 3, Some(3)).expect("text page should render to PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.contains("/Font << /Ftxt "));
    assert!(
        text.contains(
            "/Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode "
        )
    );
    assert!(text.contains("/CMapName /DjvulpesText"));
    assert!(text.contains("<41> <0041>"));
    assert!(text.contains("3 Tr\n/Ftxt 1 Tf"));
    assert!(text.contains(
        "/ActualText <FEFF0048004900530054004F005200590020004F00460020004900520041004E00490041004E0020004C004900540045005200410054005500520045>"
    ));
}

#[test]
fn render_document_pdf_embeds_unicode_actual_text() {
    let pdf = render_document_pdf(RYPKA, 7, Some(7)).expect("unicode text page should render");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.contains(
        "/ActualText <FEFF004F00540041004B004100520020004B004C00CD004D0041002C00200056011A005200410020004B0055004200CD010C004B004F005600C1002C002000460045004C00490058002000540041005500450052002C>"
    ));
}

#[test]
fn render_document_pdf_omits_outline_when_selected_range_has_no_bookmarks() {
    let pdf = render_document_pdf(RYPKA, 2, Some(2)).expect("single page should render to PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert!(!text.contains("/Outlines "));
    assert!(!text.contains("/PageMode /UseOutlines"));
}
