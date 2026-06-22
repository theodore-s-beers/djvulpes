use crate::{
    Bookmark, BzzError, DjvuRenderError, Document, PageRenderMode, ParseError, RenderError,
    TextError, TextZone, TextZoneKind, decode_dirm_tail, extract_document_bookmarks,
    extract_document_text_zone_pages, parse_dirm_tail,
    render::{PageBitmap, PartialPageRender, PixelFormat},
};
mod ccitt;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::{self, Write as _},
    io::{self, Cursor, Write},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PdfError(String);

pub type PdfResult<T> = Result<T, PdfError>;

pub type DjvuPdfResult<T> = Result<T, DjvuPdfError>;

#[derive(Debug, thiserror::Error)]
pub enum DjvuPdfError {
    #[error("{0}")]
    Parse(#[from] ParseError),
    #[error("{0}")]
    Bzz(#[from] BzzError),
    #[error("{0}")]
    Text(#[from] TextError),
    #[error("{0}")]
    Render(#[from] RenderError),
    #[error("{0}")]
    Pdf(#[from] PdfError),
    #[error("from page must be 1 or greater")]
    ZeroFromPage,
    #[error("to page must be greater than or equal to from page")]
    ReversedPageRange,
    #[error("page {page} not found; document has {page_count} pages")]
    PageOutOfRange { page: usize, page_count: usize },
}

impl From<DjvuRenderError> for DjvuPdfError {
    fn from(error: DjvuRenderError) -> Self {
        match error {
            DjvuRenderError::Parse(error) => Self::Parse(error),
            DjvuRenderError::Bzz(error) => Self::Bzz(error),
            DjvuRenderError::Render(error) => Self::Render(error),
            DjvuRenderError::ZeroPage | DjvuRenderError::ZeroFromPage => Self::ZeroFromPage,
            DjvuRenderError::ReversedPageRange => Self::ReversedPageRange,
            DjvuRenderError::PageOutOfRange { page, page_count } => {
                Self::PageOutOfRange { page, page_count }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DjvuPdfRenderEvent<'a> {
    PageStarted {
        page_number: usize,
        end_page: usize,
    },
    PageImagePrepared {
        page_number: usize,
        image: &'a PdfPageImage,
    },
    PageRendered {
        page_number: usize,
        render: &'a PartialPageRender,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DjvuPdfTimingStage {
    Setup,
    TextExtraction,
    PagePlan,
    DirectBitonal,
    FallbackRender,
    PdfWrite,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DjvuPdfTimingEvent {
    pub stage: DjvuPdfTimingStage,
    pub duration: Duration,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PdfPageImage {
    Rgb8 {
        width: u32,
        height: u32,
        dpi: u16,
        pixels: Vec<u8>,
    },
    BitonalMask {
        width: u32,
        height: u32,
        dpi: u16,
        mask: Vec<u8>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PdfBookmark {
    pub title: String,
    pub page_index: usize,
    pub children: Vec<Self>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct NumberedPdfBookmark {
    id: usize,
    title: String,
    page_index: usize,
    children: Vec<Self>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PdfTextPage {
    spans: Vec<PdfTextSpan>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PdfTextSpan {
    text: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PdfTextEncoding {
    custom_codes: BTreeMap<char, u8>,
}

impl PdfError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PdfError {}

impl From<io::Error> for PdfError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

/// Writes a PDF containing one full-page RGB image per bitmap.
///
/// # Errors
///
/// Returns an error if a bitmap has an unsupported pixel format or an invalid
/// pixel buffer length.
pub fn write_bitmap_pdf(bitmaps: &[PageBitmap]) -> PdfResult<Vec<u8>> {
    let pages = bitmaps
        .iter()
        .map(|bitmap| {
            validate_bitmap(bitmap)?;
            Ok(PdfPageImage::Rgb8 {
                width: bitmap.width,
                height: bitmap.height,
                dpi: bitmap.dpi,
                pixels: bitmap.pixels.clone(),
            })
        })
        .collect::<PdfResult<Vec<_>>>()?;

    write_page_image_pdf(&pages)
}

/// Writes a PDF containing one full-page image per page.
///
/// # Errors
///
/// Returns an error if no pages are provided or if an image buffer length does
/// not match its dimensions.
pub fn write_page_image_pdf(pages: &[PdfPageImage]) -> PdfResult<Vec<u8>> {
    write_page_image_pdf_iter(pages.len(), pages.iter().cloned().map(Ok))
}

/// Writes a PDF from an iterator of full-page images.
///
/// This avoids retaining a second list of PDF page/image objects while the
/// output is being built.
///
/// # Errors
///
/// Returns an error if `page_count` is zero, if the iterator yields a different
/// number of pages, or if any page image buffer length does not match its
/// dimensions.
pub fn write_page_image_pdf_iter<I, E>(page_count: usize, pages: I) -> Result<Vec<u8>, E>
where
    I: IntoIterator<Item = Result<PdfPageImage, E>>,
    E: From<PdfError>,
{
    write_page_image_pdf_iter_with_bookmarks_and_text(page_count, pages, &[], None)
}

/// Writes a PDF from an iterator of full-page images to an existing writer.
///
/// This is the streaming form of [`write_page_image_pdf_iter`]. It writes image
/// and page objects as each page is produced, retaining only cross-reference
/// offsets and caller-provided text/outline metadata.
///
/// # Errors
///
/// Returns an error if `page_count` is zero, if the iterator yields a different
/// number of pages, if any page image buffer length does not match its
/// dimensions, or if writing to `writer` fails.
pub fn write_page_image_pdf_iter_to_writer<W, I, E>(
    writer: W,
    page_count: usize,
    pages: I,
) -> Result<(), E>
where
    W: Write,
    I: IntoIterator<Item = Result<PdfPageImage, E>>,
    E: From<PdfError>,
{
    write_page_image_pdf_iter_with_bookmarks_and_text_to_writer(
        writer,
        page_count,
        pages,
        &[],
        None,
        None,
    )
}

fn write_page_image_pdf_iter_with_bookmarks<I, E>(
    page_count: usize,
    pages: I,
    bookmarks: &[PdfBookmark],
) -> Result<Vec<u8>, E>
where
    I: IntoIterator<Item = Result<PdfPageImage, E>>,
    E: From<PdfError>,
{
    write_page_image_pdf_iter_with_bookmarks_and_text(page_count, pages, bookmarks, None)
}

fn write_page_image_pdf_iter_with_bookmarks_and_text<I, E>(
    page_count: usize,
    pages: I,
    bookmarks: &[PdfBookmark],
    text_pages: Option<&[PdfTextPage]>,
) -> Result<Vec<u8>, E>
where
    I: IntoIterator<Item = Result<PdfPageImage, E>>,
    E: From<PdfError>,
{
    let mut pdf = Cursor::new(Vec::new());
    write_page_image_pdf_iter_with_bookmarks_and_text_to_writer(
        &mut pdf, page_count, pages, bookmarks, text_pages, None,
    )?;

    Ok(pdf.into_inner())
}

#[expect(
    clippy::too_many_lines,
    reason = "PDF object numbering and serialization are kept together for consistency"
)]
fn write_page_image_pdf_iter_with_bookmarks_and_text_to_writer<W, I, E>(
    writer: W,
    page_count: usize,
    pages: I,
    bookmarks: &[PdfBookmark],
    text_pages: Option<&[PdfTextPage]>,
    mut timing: Option<&mut dyn FnMut(DjvuPdfTimingEvent)>,
) -> Result<(), E>
where
    W: Write,
    I: IntoIterator<Item = Result<PdfPageImage, E>>,
    E: From<PdfError>,
{
    if page_count == 0 {
        return Err(PdfError::new("cannot write a PDF with no pages").into());
    }
    if let Some(text_pages) = text_pages
        && text_pages.len() != page_count
    {
        return Err(PdfError::new(format!(
            "PDF text layer has {} pages, expected {page_count}",
            text_pages.len()
        ))
        .into());
    }
    let catalog_id = 1;
    let pages_id = 2;
    let first_page_id = 3;
    let first_content_id = first_page_id + page_count;
    let first_image_id = first_content_id + page_count;
    let has_text_layer =
        text_pages.is_some_and(|pages| pages.iter().any(|page| !page.spans.is_empty()));
    let text_encoding = if has_text_layer {
        Some(PdfTextEncoding::from_pages(text_pages.expect(
            "text pages should exist when text layer is present",
        ))?)
    } else {
        None
    };
    let font_id = first_image_id + page_count;
    let to_unicode_id = font_id + 1;
    let text_object_count = if has_text_layer { 2 } else { 0 };
    let outline_root_id = first_image_id + page_count + text_object_count;
    let first_outline_item_id = outline_root_id + 1;
    let outline_item_count = count_pdf_bookmarks(bookmarks);
    let has_outline = outline_item_count != 0;
    let numbered_bookmarks = if has_outline {
        let mut next_id = first_outline_item_id;
        number_pdf_bookmarks(bookmarks, &mut next_id)
    } else {
        Vec::new()
    };
    let max_object_id = if has_outline {
        first_outline_item_id + outline_item_count - 1
    } else if has_text_layer {
        to_unicode_id
    } else {
        first_image_id + page_count - 1
    };
    let mut pdf = PdfObjectWriter::new(writer, max_object_id).map_err(E::from)?;

    let catalog = if has_outline {
        format!(
            "<< /Type /Catalog /Pages {pages_id} 0 R /Outlines {outline_root_id} 0 R /PageMode /UseOutlines >>"
        )
    } else {
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>")
    };
    pdf.write_object(catalog_id, catalog.as_bytes())
        .map_err(E::from)?;

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", first_page_id + index))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.write_object(
        pages_id,
        format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>").as_bytes(),
    )
    .map_err(E::from)?;

    let mut actual_page_count = 0usize;
    for (index, page_image) in pages.into_iter().enumerate() {
        if index >= page_count {
            return Err(PdfError::new(format!(
                "PDF page iterator produced more than {page_count} pages"
            ))
            .into());
        }
        let page_image = page_image?;
        validate_page_image(&page_image).map_err(E::from)?;
        actual_page_count += 1;
        let page_object_id = first_page_id + index;
        let content_id = first_content_id + index;
        let image_id = first_image_id + index;
        let image_name = format!("Im{}", index + 1);
        let width_points = points_from_pixels(page_image.width(), page_image.dpi());
        let height_points = points_from_pixels(page_image.height(), page_image.dpi());
        let font_resource = if has_text_layer {
            format!(" /Font << /Ftxt {font_id} 0 R >>")
        } else {
            String::new()
        };
        let page = format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {width_points} {height_points}] /Resources << /XObject << /{image_name} {image_id} 0 R >>{font_resource} >> /Contents {content_id} 0 R >>"
        );
        pdf.write_object(page_object_id, page.as_bytes())
            .map_err(E::from)?;

        let mut content = if page_image.is_bitonal_mask() {
            format!(
                "q\n1 g\n0 0 {width_points} {height_points} re f\n0 g\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n"
            )
        } else {
            format!("q\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n")
        };
        if let (Some(text_page), Some(text_encoding)) = (
            text_pages.and_then(|pages| pages.get(index)),
            text_encoding.as_ref(),
        ) {
            append_pdf_text_layer(&mut content, text_page, page_image.dpi(), text_encoding);
        }
        let content_stream = stream_object(&[], content.as_bytes());
        pdf.write_object(content_id, &content_stream)
            .map_err(E::from)?;

        let image_start = timing.is_some().then(Instant::now);
        let image_stream = page_image.image_stream_object();
        pdf.write_object(image_id, &image_stream).map_err(E::from)?;
        if let Some(image_start) = image_start
            && let Some(timing) = timing.as_deref_mut()
        {
            timing(DjvuPdfTimingEvent {
                stage: DjvuPdfTimingStage::PdfWrite,
                duration: image_start.elapsed(),
            });
        }
    }

    if actual_page_count != page_count {
        return Err(PdfError::new(format!(
            "PDF page iterator produced {actual_page_count} pages, expected {page_count}"
        ))
        .into());
    }

    if has_text_layer {
        pdf.write_object(
            font_id,
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode {to_unicode_id} 0 R >>"
            )
            .as_bytes(),
        )
        .map_err(E::from)?;
        let to_unicode_stream = stream_object(
            &[],
            pdf_to_unicode_cmap(
                text_encoding
                    .as_ref()
                    .expect("text encoding should exist when writing text layer"),
            )
            .as_bytes(),
        );
        pdf.write_object(to_unicode_id, &to_unicode_stream)
            .map_err(E::from)?;
    }

    if has_outline {
        write_pdf_outlines(
            &mut pdf,
            outline_root_id,
            first_page_id,
            &numbered_bookmarks,
        )
        .map_err(E::from)?;
    }

    let finish_start = timing.is_some().then(Instant::now);
    let result = pdf.finish(catalog_id).map_err(E::from);
    if let Some(finish_start) = finish_start
        && let Some(timing) = timing
    {
        timing(DjvuPdfTimingEvent {
            stage: DjvuPdfTimingStage::PdfWrite,
            duration: finish_start.elapsed(),
        });
    }

    result
}

/// Writes a PDF from an iterator of rendered pages.
///
/// This preserves the same image embedding choices as [`PdfPageImage::from_render`]:
/// bitonal-only renders can become 1-bit grayscale images, while renders containing
/// IW44 layers are embedded as RGB images.
///
/// # Errors
///
/// Returns an error if `page_count` is zero, if the iterator yields a different
/// number of pages, if a rendered page cannot be produced, or if a generated
/// page image buffer length does not match its dimensions.
pub fn write_rendered_pages_pdf_iter<I, E>(page_count: usize, renders: I) -> Result<Vec<u8>, E>
where
    I: IntoIterator<Item = Result<PartialPageRender, E>>,
    E: From<PdfError>,
{
    write_rendered_pages_pdf_iter_with_bookmarks(page_count, renders, &[])
}

fn write_rendered_pages_pdf_iter_with_bookmarks<I, E>(
    page_count: usize,
    renders: I,
    bookmarks: &[PdfBookmark],
) -> Result<Vec<u8>, E>
where
    I: IntoIterator<Item = Result<PartialPageRender, E>>,
    E: From<PdfError>,
{
    write_page_image_pdf_iter_with_bookmarks(
        page_count,
        renders
            .into_iter()
            .map(|render| render.map(|render| PdfPageImage::from_render(&render))),
        bookmarks,
    )
}

/// Renders a `DjVu` document byte slice to a PDF.
///
/// Page numbers are 1-based. If `to_page` is `None`, rendering continues
/// through the final page.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page range is invalid, a selected page cannot be
/// rendered, or the resulting PDF cannot be serialized.
pub fn render_document_pdf(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> DjvuPdfResult<Vec<u8>> {
    let mut pdf = Cursor::new(Vec::new());
    render_document_pdf_to_writer(&mut pdf, bytes, from_page, to_page)?;

    Ok(pdf.into_inner())
}

/// Renders a `DjVu` document byte slice to a PDF writer.
///
/// Page numbers are 1-based. If `to_page` is `None`, rendering continues
/// through the final page.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page range is invalid, a selected page cannot be
/// rendered, the resulting PDF cannot be serialized, or writing fails.
pub fn render_document_pdf_to_writer<W: Write>(
    writer: W,
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> DjvuPdfResult<()> {
    render_document_pdf_to_writer_with_events(writer, bytes, from_page, to_page, |_| {})
}

/// Renders a `DjVu` document byte slice to a PDF, reporting page-level events.
///
/// The event callback is invoked before each selected page is prepared. Pages
/// that can be embedded directly as 1-bit images emit `PageImagePrepared`.
/// Fallback pages emit `PageRendered` after compositor output is available. Any
/// borrowed event payload is valid only for the duration of the callback.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page range is invalid, a selected page cannot be
/// rendered, or the resulting PDF cannot be serialized.
pub fn render_document_pdf_with_events(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
    mut event: impl FnMut(DjvuPdfRenderEvent<'_>),
) -> DjvuPdfResult<Vec<u8>> {
    let mut pdf = Cursor::new(Vec::new());
    render_document_pdf_to_writer_with_events(&mut pdf, bytes, from_page, to_page, |event_item| {
        event(event_item);
    })?;

    Ok(pdf.into_inner())
}

/// Renders a `DjVu` document byte slice to a PDF writer, reporting page-level events.
///
/// The event callback is invoked before each selected page is prepared. Pages
/// that can be embedded directly as 1-bit images emit `PageImagePrepared`.
/// Fallback pages emit `PageRendered` after compositor output is available. Any
/// borrowed event payload is valid only for the duration of the callback.
///
/// # Errors
///
/// Returns an error if the document cannot be parsed, its bundled directory
/// cannot be decoded, the page range is invalid, a selected page cannot be
/// rendered, the resulting PDF cannot be serialized, or writing fails.
pub fn render_document_pdf_to_writer_with_events<W: Write>(
    writer: W,
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
    event: impl FnMut(DjvuPdfRenderEvent<'_>),
) -> DjvuPdfResult<()> {
    render_document_pdf_to_writer_with_events_and_timings(
        writer, bytes, from_page, to_page, event, None,
    )
}

/// Renders a `DjVu` document byte slice to a PDF writer, reporting page-level
/// events and optional aggregate timing events.
///
/// Timing callbacks are coarse-grained and opt-in. Passing `None` avoids timing
/// collection in normal conversion paths.
///
/// # Errors
///
/// Returns the same errors as [`render_document_pdf_to_writer_with_events`].
pub fn render_document_pdf_to_writer_with_events_and_timings<W: Write>(
    writer: W,
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
    mut event: impl FnMut(DjvuPdfRenderEvent<'_>),
    timing: Option<&mut dyn FnMut(DjvuPdfTimingEvent)>,
) -> DjvuPdfResult<()> {
    let timing = RefCell::new(timing);
    let setup_start = pdf_timing_start(&timing);
    let document = Document::parse(bytes)?;
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(bytes, dirm)?;
        parse_dirm_tail(dirm, &decoded_tail)?
    } else {
        Vec::new()
    };
    let page_count = document.form_kind_counts().pages;
    let end_page = checked_document_pdf_page_range(page_count, from_page, to_page)?;
    let bookmarks = extract_document_bookmarks(bytes)?.map_or_else(Vec::new, |bookmarks| {
        pdf_bookmarks_from_djvu(&bookmarks, from_page, end_page)
    });
    record_pdf_timing(&timing, DjvuPdfTimingStage::Setup, setup_start);
    let text_start = pdf_timing_start(&timing);
    let text_pages = pdf_text_pages_from_djvu(bytes, from_page, Some(end_page))?;
    record_pdf_timing(&timing, DjvuPdfTimingStage::TextExtraction, text_start);
    let selected_page_count = end_page - from_page + 1;
    let mut pages = document.pages(bytes).enumerate();

    let page_images = std::iter::from_fn(|| {
        for (index, page) in pages.by_ref() {
            let page_number = index + 1;
            if page_number < from_page {
                continue;
            }
            if page_number > end_page {
                return None;
            }

            event(DjvuPdfRenderEvent::PageStarted {
                page_number,
                end_page,
            });
            let image = (|| {
                let page = page?;
                let plan_start = pdf_timing_start(&timing);
                let plan = document.page_render_plan(bytes, &page, &tail_entries)?;
                record_pdf_timing(&timing, DjvuPdfTimingStage::PagePlan, plan_start);
                let direct_start = pdf_timing_start(&timing);
                if let Some(image) = direct_bitonal_pdf_page_image(bytes, &plan)? {
                    record_pdf_timing(&timing, DjvuPdfTimingStage::DirectBitonal, direct_start);
                    event(DjvuPdfRenderEvent::PageImagePrepared {
                        page_number,
                        image: &image,
                    });
                    return Ok(image);
                }

                let render_start = pdf_timing_start(&timing);
                let render = plan.render_bitmap_with_mode(bytes, PageRenderMode::Full)?;
                record_pdf_timing(&timing, DjvuPdfTimingStage::FallbackRender, render_start);
                event(DjvuPdfRenderEvent::PageRendered {
                    page_number,
                    render: &render,
                });

                Ok(PdfPageImage::from_render(&render))
            })();

            return Some(image);
        }

        None
    });

    let mut write_timing = |event| {
        if let Some(timing) = timing.borrow_mut().as_deref_mut() {
            timing(event);
        }
    };
    let has_timing = timing.borrow().is_some();

    write_page_image_pdf_iter_with_bookmarks_and_text_to_writer(
        writer,
        selected_page_count,
        page_images,
        &bookmarks,
        Some(&text_pages),
        has_timing.then_some(&mut write_timing as &mut dyn FnMut(DjvuPdfTimingEvent)),
    )
}

fn pdf_timing_start(
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
) -> Option<Instant> {
    timing.borrow().is_some().then(Instant::now)
}

fn record_pdf_timing(
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
    stage: DjvuPdfTimingStage,
    start: Option<Instant>,
) {
    if let Some(start) = start
        && let Some(timing) = timing.borrow_mut().as_deref_mut()
    {
        timing(DjvuPdfTimingEvent {
            stage,
            duration: start.elapsed(),
        });
    }
}

fn checked_document_pdf_page_range(
    page_count: usize,
    from_page: usize,
    to_page: Option<usize>,
) -> DjvuPdfResult<usize> {
    if from_page == 0 {
        return Err(DjvuPdfError::ZeroFromPage);
    }
    if let Some(to_page) = to_page
        && to_page < from_page
    {
        return Err(DjvuPdfError::ReversedPageRange);
    }

    let end_page = to_page.unwrap_or(page_count);
    if from_page > page_count {
        return Err(DjvuPdfError::PageOutOfRange {
            page: from_page,
            page_count,
        });
    }
    if end_page > page_count {
        return Err(DjvuPdfError::PageOutOfRange {
            page: end_page,
            page_count,
        });
    }

    Ok(end_page)
}

fn direct_bitonal_pdf_page_image(
    bytes: &[u8],
    plan: &crate::PageRenderPlan<'_>,
) -> Result<Option<PdfPageImage>, RenderError> {
    if !plan.background_layers.is_empty() || !plan.foreground_layers.is_empty() {
        return Ok(None);
    }

    let masks = plan.bitonal_masks(bytes).map_err(RenderError::from)?;
    let [(.., partial)] = masks.as_slice() else {
        return Ok(None);
    };
    if partial.mask.width != u32::from(plan.info.width)
        || partial.mask.height != u32::from(plan.info.height)
    {
        return Ok(None);
    }

    Ok(Some(PdfPageImage::BitonalMask {
        width: partial.mask.width,
        height: partial.mask.height,
        dpi: plan.info.dpi,
        mask: partial.mask.to_image_mask_bytes(),
    }))
}

fn pdf_text_pages_from_djvu(
    bytes: &[u8],
    from_page: usize,
    to_page: Option<usize>,
) -> DjvuPdfResult<Vec<PdfTextPage>> {
    extract_document_text_zone_pages(bytes, from_page, to_page)?
        .iter()
        .map(|page| {
            let mut spans = Vec::new();
            if let Some(zone) = &page.zone {
                collect_pdf_text_spans(zone, &page.text, zone.height, &mut spans);
            }
            Ok(PdfTextPage { spans })
        })
        .collect()
}

fn collect_pdf_text_spans(
    zone: &TextZone,
    text: &str,
    page_height: i32,
    spans: &mut Vec<PdfTextSpan>,
) {
    if matches!(zone.kind, TextZoneKind::Line) {
        let mut words = Vec::new();
        collect_pdf_line_words(zone, text, &mut words);
        let line = words.join(" ");
        if !line.is_empty() {
            spans.push(PdfTextSpan {
                text: line,
                x: zone.x_min(),
                y: zone.y_min(page_height),
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
                y: zone.y_min(page_height),
                width: zone.width,
                height: zone.height,
            });
        }
        return;
    }

    for child in &zone.children {
        collect_pdf_text_spans(child, text, page_height, spans);
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

fn pdf_bookmarks_from_djvu(
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

fn append_pdf_text_layer(
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

fn count_pdf_bookmarks(bookmarks: &[PdfBookmark]) -> usize {
    bookmarks
        .iter()
        .map(|bookmark| 1 + count_pdf_bookmarks(&bookmark.children))
        .sum()
}

fn number_pdf_bookmarks(
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

fn write_pdf_outlines<W: Write>(
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
    fn from_pages(pages: &[PdfTextPage]) -> PdfResult<Self> {
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

fn pdf_to_unicode_cmap(encoding: &PdfTextEncoding) -> String {
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

impl PdfPageImage {
    #[must_use]
    pub fn from_render(render: &PartialPageRender) -> Self {
        if render.iw44_layers.is_empty() {
            if let Some((_, bitonal)) = render.bitonal_masks.first()
                && bitonal.mask.width == render.bitmap.width
                && bitonal.mask.height == render.bitmap.height
            {
                return Self::BitonalMask {
                    width: render.bitmap.width,
                    height: render.bitmap.height,
                    dpi: render.bitmap.dpi,
                    mask: bitonal.mask.to_image_mask_bytes(),
                };
            }

            if render.bitonal_masks.is_empty() && render.bitmap.stats().black_pixels == 0 {
                let row_bytes = (render.bitmap.width as usize).div_ceil(8);
                return Self::BitonalMask {
                    width: render.bitmap.width,
                    height: render.bitmap.height,
                    dpi: render.bitmap.dpi,
                    mask: vec![0; row_bytes.saturating_mul(render.bitmap.height as usize)],
                };
            }
        }

        Self::Rgb8 {
            width: render.bitmap.width,
            height: render.bitmap.height,
            dpi: render.bitmap.dpi,
            pixels: render.bitmap.pixels.clone(),
        }
    }

    const fn width(&self) -> u32 {
        match self {
            Self::Rgb8 { width, .. } | Self::BitonalMask { width, .. } => *width,
        }
    }

    const fn height(&self) -> u32 {
        match self {
            Self::Rgb8 { height, .. } | Self::BitonalMask { height, .. } => *height,
        }
    }

    const fn dpi(&self) -> u16 {
        match self {
            Self::Rgb8 { dpi, .. } | Self::BitonalMask { dpi, .. } => *dpi,
        }
    }

    const fn is_bitonal_mask(&self) -> bool {
        matches!(self, Self::BitonalMask { .. })
    }

    fn image_stream_object(&self) -> Vec<u8> {
        match self {
            Self::Rgb8 {
                width,
                height,
                pixels,
                ..
            } => image_stream_object(
                &format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8"
                ),
                pixels,
            ),
            Self::BitonalMask {
                width,
                height,
                mask,
                ..
            } => {
                let dictionary = format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"
                );
                ccitt_group4_image_stream_object(&dictionary, *width, *height, mask)
            }
        }
    }
}

fn validate_bitmap(bitmap: &PageBitmap) -> PdfResult<()> {
    if bitmap.format != PixelFormat::Rgb8 {
        return Err(PdfError::new("only RGB8 bitmaps can be written to PDF"));
    }

    let expected_len = rgb_len(bitmap.width, bitmap.height)?;
    if bitmap.pixels.len() != expected_len {
        return Err(PdfError::new(format!(
            "bitmap has {} bytes, expected {expected_len}",
            bitmap.pixels.len()
        )));
    }

    Ok(())
}

fn validate_page_image(page: &PdfPageImage) -> PdfResult<()> {
    match page {
        PdfPageImage::Rgb8 {
            width,
            height,
            pixels,
            ..
        } => {
            let expected_len = rgb_len(*width, *height)?;
            if pixels.len() != expected_len {
                return Err(PdfError::new(format!(
                    "RGB page has {} bytes, expected {expected_len}",
                    pixels.len()
                )));
            }
        }
        PdfPageImage::BitonalMask {
            width,
            height,
            mask,
            ..
        } => {
            let expected_len = bitonal_len(*width, *height)?;
            if mask.len() != expected_len {
                return Err(PdfError::new(format!(
                    "bitonal page has {} bytes, expected {expected_len}",
                    mask.len()
                )));
            }
        }
    }

    Ok(())
}

fn rgb_len(width: u32, height: u32) -> PdfResult<usize> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| PdfError::new("bitmap dimensions overflow"))
}

fn bitonal_len(width: u32, height: u32) -> PdfResult<usize> {
    let width = usize::try_from(width).map_err(|_| PdfError::new("bitmap dimensions overflow"))?;
    let height =
        usize::try_from(height).map_err(|_| PdfError::new("bitmap dimensions overflow"))?;
    width
        .div_ceil(8)
        .checked_mul(height)
        .ok_or_else(|| PdfError::new("bitmap dimensions overflow"))
}

fn points_from_pixels(pixels: u32, dpi: u16) -> String {
    let dpi = u32::from(dpi.max(1));
    let points = f64::from(pixels) * 72.0 / f64::from(dpi);
    format!("{points:.4}")
}

fn points_from_signed_pixels(pixels: i32, dpi: u16) -> String {
    let points = f64::from(pixels) * 72.0 / f64::from(dpi.max(1));
    format!("{points:.4}")
}

fn stream_object(dictionary: &[u8], stream: &[u8]) -> Vec<u8> {
    let mut object = Vec::new();
    object.extend_from_slice(b"<< ");
    if !dictionary.is_empty() {
        object.extend_from_slice(dictionary);
        object.push(b' ');
    }
    object.extend_from_slice(format!("/Length {} >>\nstream\n", stream.len()).as_bytes());
    object.extend_from_slice(stream);
    object.extend_from_slice(b"\nendstream");
    object
}

fn image_stream_object(dictionary: &str, bytes: &[u8]) -> Vec<u8> {
    let encoded = pdf_run_length_encode(bytes);
    if encoded.len() < bytes.len() {
        let dictionary = format!("{dictionary} /Filter /RunLengthDecode");
        stream_object(dictionary.as_bytes(), &encoded)
    } else {
        stream_object(dictionary.as_bytes(), bytes)
    }
}

fn ccitt_group4_image_stream_object(
    dictionary: &str,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> Vec<u8> {
    let encoded = ccitt::group4_encode(width, height, bytes);
    let dictionary = format!(
        "{dictionary} /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns {width} /Rows {height} /BlackIs1 true >>"
    );

    stream_object(dictionary.as_bytes(), &encoded)
}

fn pdf_run_length_encode(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(bytes.len().saturating_add(1));
    let mut index = 0usize;

    while index < bytes.len() {
        let repeat_len = repeated_run_len(bytes, index);
        if repeat_len >= 2 {
            encoded.push(
                u8::try_from(257usize - repeat_len)
                    .expect("PDF repeat run header should fit in one byte"),
            );
            encoded.push(bytes[index]);
            index += repeat_len;
            continue;
        }

        let literal_start = index;
        index += 1;
        while index < bytes.len() {
            if repeated_run_len(bytes, index) >= 2 || index - literal_start == 128 {
                break;
            }
            index += 1;
        }
        let literal_len = index - literal_start;
        encoded.push(
            u8::try_from(literal_len - 1).expect("PDF literal run header should fit in one byte"),
        );
        encoded.extend_from_slice(&bytes[literal_start..index]);
    }

    encoded.push(128);
    encoded
}

const fn repeated_run_len(bytes: &[u8], start: usize) -> usize {
    let value = bytes[start];
    let mut len = 1usize;
    while start + len < bytes.len() && len < 128 && bytes[start + len] == value {
        len += 1;
    }

    len
}

struct PdfObjectWriter<W> {
    inner: W,
    offsets: Vec<usize>,
    position: usize,
}

impl<W: Write> PdfObjectWriter<W> {
    fn new(inner: W, max_object_id: usize) -> PdfResult<Self> {
        let mut writer = Self {
            inner,
            offsets: vec![0; max_object_id + 1],
            position: 0,
        };
        writer.write_all_counted(b"%PDF-1.4\n%\xff\xff\xff\xff\n")?;

        Ok(writer)
    }

    fn write_object(&mut self, id: usize, object: &[u8]) -> PdfResult<()> {
        let offset = self
            .offsets
            .get_mut(id)
            .ok_or_else(|| PdfError::new(format!("PDF object id {id} exceeds xref size")))?;
        *offset = self.position;
        self.write_all_counted(format!("{id} 0 obj\n").as_bytes())?;
        self.write_all_counted(object)?;
        self.write_all_counted(b"\nendobj\n")
    }

    fn finish(mut self, root_id: usize) -> PdfResult<()> {
        let xref_start = self.position;
        self.write_all_counted(format!("xref\n0 {}\n", self.offsets.len()).as_bytes())?;
        self.write_all_counted(b"0000000000 65535 f \n")?;
        for offset in self.offsets.clone().iter().skip(1) {
            self.write_all_counted(format!("{offset:010} 00000 n \n").as_bytes())?;
        }
        self.write_all_counted(
            format!(
                "trailer\n<< /Size {} /Root {root_id} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
                self.offsets.len()
            )
            .as_bytes(),
        )
    }

    fn write_all_counted(&mut self, bytes: &[u8]) -> PdfResult<()> {
        self.inner.write_all(bytes)?;
        self.position = self
            .position
            .checked_add(bytes.len())
            .ok_or_else(|| PdfError::new("PDF byte offset overflow"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::PageBitmap;

    const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");

    #[test]
    fn write_bitmap_pdf_embeds_rgb_image_page() {
        let mut bitmap = PageBitmap::new_rgb8(1, 2, 144, [0xff, 0xff, 0xff]);
        assert!(bitmap.set_rgb(0, 1, [0, 0, 0]));

        let pdf = write_bitmap_pdf(&[bitmap]).expect("PDF should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Type /Page"));
        assert!(text.contains("/MediaBox [0 0 0.5000 1.0000]"));
        assert!(text.contains("/Subtype /Image /Width 1 /Height 2"));
        assert!(text.contains("/Filter /RunLengthDecode"));
        assert!(text.contains("/Length 5"));
        assert!(text.contains("xref\n0 6\n"));
        assert!(text.contains("startxref\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn write_bitmap_pdf_embeds_multiple_rgb_image_pages() {
        let first = PageBitmap::new_rgb8(1, 1, 72, [0, 0, 0]);
        let second = PageBitmap::new_rgb8(2, 1, 144, [0xff, 0xff, 0xff]);

        let pdf = write_bitmap_pdf(&[first, second]).expect("PDF should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/Type /Pages /Count 2 /Kids [3 0 R 4 0 R]"));
        assert!(text.contains("/MediaBox [0 0 1.0000 1.0000]"));
        assert!(text.contains("/MediaBox [0 0 1.0000 0.5000]"));
        assert!(text.contains("/Im1 7 0 R"));
        assert!(text.contains("/Im2 8 0 R"));
        assert!(text.contains("/Subtype /Image /Width 1 /Height 1"));
        assert!(text.contains("/Subtype /Image /Width 2 /Height 1"));
        assert!(text.contains("/Filter /RunLengthDecode"));
        assert!(text.contains("xref\n0 9\n"));
    }

    #[test]
    fn write_page_image_pdf_embeds_bitonal_image_mask_page() {
        let page = PdfPageImage::BitonalMask {
            width: 32,
            height: 1,
            dpi: 72,
            mask: vec![0, 0, 0, 0],
        };

        let pdf = write_page_image_pdf(&[page]).expect("PDF should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/MediaBox [0 0 32.0000 1.0000]"));
        assert!(text.contains("/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"));
        assert!(text.contains("/Filter /CCITTFaxDecode"));
        assert!(text.contains("/DecodeParms << /K -1 /Columns 32 /Rows 1 /BlackIs1 true >>"));
        assert!(text.contains("1 g\n0 0 32.0000 1.0000 re f\n0 g"));
        assert!(text.contains("0 g\n32.0000 0 0 1.0000 0 0 cm"));
    }

    #[test]
    fn write_rendered_pages_pdf_iter_uses_render_image_embedding_choices() {
        let rgb_render = PartialPageRender {
            bitmap: PageBitmap::new_rgb8(1, 1, 72, [0, 0, 0]),
            iw44_layers: Vec::new(),
            bitonal_masks: Vec::new(),
        };
        let white_bitonal_render = PartialPageRender {
            bitmap: PageBitmap::new_rgb8(8, 1, 72, [0xff, 0xff, 0xff]),
            iw44_layers: Vec::new(),
            bitonal_masks: Vec::new(),
        };

        let pdf = write_rendered_pages_pdf_iter(
            2,
            [
                Ok::<PartialPageRender, PdfError>(rgb_render),
                Ok::<PartialPageRender, PdfError>(white_bitonal_render),
            ],
        )
        .expect("rendered pages should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/Type /Pages /Count 2 /Kids [3 0 R 4 0 R]"));
        assert!(text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
        assert!(text.contains("/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"));
    }

    #[test]
    fn write_page_image_pdf_iter_embeds_bookmark_outline() {
        let page = PdfPageImage::Rgb8 {
            width: 1,
            height: 1,
            dpi: 72,
            pixels: vec![0, 0, 0],
        };
        let bookmarks = [PdfBookmark {
            title: "Root".to_string(),
            page_index: 0,
            children: vec![PdfBookmark {
                title: "Child".to_string(),
                page_index: 0,
                children: Vec::new(),
            }],
        }];

        let pdf = write_page_image_pdf_iter_with_bookmarks(
            1,
            [Ok::<PdfPageImage, PdfError>(page)],
            &bookmarks,
        )
        .expect("PDF with outline should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(
            text.contains("/Type /Catalog /Pages 2 0 R /Outlines 6 0 R /PageMode /UseOutlines")
        );
        assert!(text.contains("6 0 obj\n<< /Type /Outlines /First 7 0 R /Last 7 0 R /Count 2 >>"));
        assert!(text.contains("7 0 obj\n<< /Title <FEFF0052006F006F0074> /Parent 6 0 R /Dest [3 0 R /Fit] /First 8 0 R /Last 8 0 R /Count 1 >>"));
        assert!(text.contains(
            "8 0 obj\n<< /Title <FEFF004300680069006C0064> /Parent 7 0 R /Dest [3 0 R /Fit] >>"
        ));
        assert!(text.contains("xref\n0 9\n"));
    }

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
                events.push((
                    page_number,
                    page_number,
                    "prepared",
                    image.width(),
                    image.height(),
                ));
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
        let pdf =
            render_document_pdf(RYPKA, 33, Some(33)).expect("unicode outline page should render");
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
        assert!(text.contains(
            "/Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode "
        ));
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

    #[test]
    fn write_page_image_pdf_leaves_incompressible_image_unfiltered() {
        let page = PdfPageImage::Rgb8 {
            width: 2,
            height: 1,
            dpi: 72,
            pixels: vec![1, 2, 3, 4, 5, 6],
        };

        let pdf = write_page_image_pdf(&[page]).expect("PDF should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(!text.contains("/Filter /RunLengthDecode"));
        assert!(text.contains("/Length 6"));
        assert!(pdf.windows(6).any(|window| window == [1, 2, 3, 4, 5, 6]));
    }

    #[test]
    fn pdf_run_length_encode_encodes_literal_and_repeat_runs() {
        assert_eq!(pdf_run_length_encode(&[]), [128]);
        assert_eq!(
            pdf_run_length_encode(&[1, 2, 3, 4, 4, 4, 5]),
            [2, 1, 2, 3, 254, 4, 0, 5, 128]
        );
    }

    #[test]
    fn pdf_run_length_encode_splits_long_runs() {
        let bytes = vec![7; 130];

        assert_eq!(pdf_run_length_encode(&bytes), [129, 7, 255, 7, 128]);
    }

    #[test]
    fn write_page_image_pdf_rejects_invalid_bitonal_buffer_length() {
        let page = PdfPageImage::BitonalMask {
            width: 9,
            height: 2,
            dpi: 72,
            mask: vec![0; 3],
        };

        assert_eq!(
            write_page_image_pdf(&[page]).expect_err("bad buffer should fail"),
            PdfError::new("bitonal page has 3 bytes, expected 4")
        );
    }

    #[test]
    fn write_page_image_pdf_iter_rejects_short_iterator() {
        let page = PdfPageImage::Rgb8 {
            width: 1,
            height: 1,
            dpi: 72,
            pixels: vec![0, 0, 0],
        };

        assert_eq!(
            write_page_image_pdf_iter(2, [Ok::<PdfPageImage, PdfError>(page)])
                .expect_err("short iterator should fail"),
            PdfError::new("PDF page iterator produced 1 pages, expected 2")
        );
    }

    #[test]
    fn write_page_image_pdf_iter_rejects_long_iterator() {
        let page = PdfPageImage::Rgb8 {
            width: 1,
            height: 1,
            dpi: 72,
            pixels: vec![0, 0, 0],
        };

        assert_eq!(
            write_page_image_pdf_iter(
                1,
                [
                    Ok::<PdfPageImage, PdfError>(page.clone()),
                    Ok::<PdfPageImage, PdfError>(page),
                ],
            )
            .expect_err("long iterator should fail"),
            PdfError::new("PDF page iterator produced more than 1 pages")
        );
    }

    #[test]
    fn write_bitmap_pdf_rejects_empty_page_list() {
        assert_eq!(
            write_bitmap_pdf(&[]).expect_err("empty input should fail"),
            PdfError::new("cannot write a PDF with no pages")
        );
    }

    #[test]
    fn write_bitmap_pdf_rejects_invalid_pixel_buffer_length() {
        let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0, 0, 0]);
        bitmap.pixels.pop();

        assert_eq!(
            write_bitmap_pdf(&[bitmap]).expect_err("bad buffer should fail"),
            PdfError::new("bitmap has 11 bytes, expected 12")
        );
    }
}
