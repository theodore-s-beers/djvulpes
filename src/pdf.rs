use crate::{
    BzzError, DjvuRenderError, Document, Jb2Dictionary, Jb2Error, Jb2PartialImage, ParseError,
    RenderError, TextError, decode_dirm_tail, decode_jb2_dictionary, extract_document_bookmarks,
    parse_dirm_tail,
    render::{PageBitmap, PartialPageRender, PixelFormat},
    render_jb2_image, render_jb2_image_with_dictionary,
};
mod ccitt;
mod text_layer;

use text_layer::{
    PdfBookmark, PdfTextEncoding, PdfTextPage, append_pdf_text_layer, count_pdf_bookmarks,
    number_pdf_bookmarks, pdf_bookmarks_from_djvu, pdf_text_pages_from_djvu, pdf_to_unicode_cmap,
    write_pdf_outlines,
};

type Jb2DictionaryCache = BTreeMap<(usize, usize), Jb2Dictionary>;

use flate2::{Compression, write::ZlibEncoder};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt,
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
    Jb2Decode,
    Iw44Decode,
    Iw44Composite,
    MaskComposite,
    ImageBytes,
    CcittEncode,
    PdfObjectWrite,
    PdfWrite,
    PageTotal,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DjvuPdfPageKind {
    DirectBitonal,
    FallbackRgb,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DjvuPdfTimingEvent {
    pub stage: DjvuPdfTimingStage,
    pub duration: Duration,
    pub page_number: Option<usize>,
    pub page_kind: Option<DjvuPdfPageKind>,
}

impl DjvuPdfTimingEvent {
    const fn aggregate(stage: DjvuPdfTimingStage, duration: Duration) -> Self {
        Self {
            stage,
            duration,
            page_number: None,
            page_kind: None,
        }
    }

    const fn page(stage: DjvuPdfTimingStage, page_number: usize, duration: Duration) -> Self {
        Self {
            stage,
            duration,
            page_number: Some(page_number),
            page_kind: None,
        }
    }

    const fn page_kind(
        stage: DjvuPdfTimingStage,
        page_number: usize,
        page_kind: DjvuPdfPageKind,
        duration: Duration,
    ) -> Self {
        Self {
            stage,
            duration,
            page_number: Some(page_number),
            page_kind: Some(page_kind),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PdfPageImage {
    Rgb8 {
        width: u32,
        height: u32,
        dpi: u16,
        pixels: Vec<u8>,
    },
    Gray8 {
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
    let has_text_layer =
        text_pages.is_some_and(|pages| pages.iter().any(|page| !page.spans.is_empty()));
    let text_encoding = if has_text_layer {
        Some(PdfTextEncoding::from_pages(text_pages.expect(
            "text pages should exist when text layer is present",
        ))?)
    } else {
        None
    };
    let outline_item_count = count_pdf_bookmarks(bookmarks);
    let has_outline = outline_item_count != 0;
    let ids = PdfObjectIds::new(page_count, has_text_layer, outline_item_count);
    let numbered_bookmarks = if has_outline {
        let mut next_id = ids.first_outline_item;
        number_pdf_bookmarks(bookmarks, &mut next_id)
    } else {
        Vec::new()
    };
    let mut pdf = PdfObjectWriter::new(writer, ids.max_object).map_err(E::from)?;

    write_pdf_catalog_and_page_tree(&mut pdf, ids, page_count, has_outline).map_err(E::from)?;
    let actual_page_count = write_pdf_page_objects(
        &mut pdf,
        ids,
        page_count,
        pages,
        text_pages,
        text_encoding.as_ref(),
        &mut timing,
    )?;
    if actual_page_count != page_count {
        return Err(PdfError::new(format!(
            "PDF page iterator produced {actual_page_count} pages, expected {page_count}"
        ))
        .into());
    }

    write_pdf_text_objects(&mut pdf, ids, text_encoding.as_ref()).map_err(E::from)?;

    if has_outline {
        write_pdf_outlines(
            &mut pdf,
            ids.outline_root,
            ids.first_page,
            &numbered_bookmarks,
        )
        .map_err(E::from)?;
    }

    let finish_start = timing.is_some().then(Instant::now);
    let result = pdf.finish(ids.catalog).map_err(E::from);
    if let Some(finish_start) = finish_start
        && let Some(timing) = timing
    {
        timing(DjvuPdfTimingEvent::aggregate(
            DjvuPdfTimingStage::PdfWrite,
            finish_start.elapsed(),
        ));
    }

    result
}

#[derive(Clone, Copy)]
struct PdfObjectIds {
    catalog: usize,
    pages: usize,
    first_page: usize,
    first_content: usize,
    first_image: usize,
    font: usize,
    to_unicode: usize,
    outline_root: usize,
    first_outline_item: usize,
    max_object: usize,
}

impl PdfObjectIds {
    const fn new(page_count: usize, has_text_layer: bool, outline_item_count: usize) -> Self {
        let catalog = 1;
        let pages = 2;
        let first_page = 3;
        let first_content = first_page + page_count;
        let first_image = first_content + page_count;
        let font = first_image + page_count;
        let to_unicode = font + 1;
        let text_object_count = if has_text_layer { 2 } else { 0 };
        let outline_root = first_image + page_count + text_object_count;
        let first_outline_item = outline_root + 1;
        let max_object = if outline_item_count != 0 {
            first_outline_item + outline_item_count - 1
        } else if has_text_layer {
            to_unicode
        } else {
            first_image + page_count - 1
        };

        Self {
            catalog,
            pages,
            first_page,
            first_content,
            first_image,
            font,
            to_unicode,
            outline_root,
            first_outline_item,
            max_object,
        }
    }
}

fn write_pdf_catalog_and_page_tree<W: Write>(
    pdf: &mut PdfObjectWriter<W>,
    ids: PdfObjectIds,
    page_count: usize,
    has_outline: bool,
) -> PdfResult<()> {
    let catalog = if has_outline {
        format!(
            "<< /Type /Catalog /Pages {} 0 R /Outlines {} 0 R /PageMode /UseOutlines >>",
            ids.pages, ids.outline_root
        )
    } else {
        format!("<< /Type /Catalog /Pages {} 0 R >>", ids.pages)
    };
    pdf.write_object(ids.catalog, catalog.as_bytes())?;

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", ids.first_page + index))
        .collect::<Vec<_>>()
        .join(" ");
    pdf.write_object(
        ids.pages,
        format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>").as_bytes(),
    )
}

fn write_pdf_page_objects<W, I, E>(
    pdf: &mut PdfObjectWriter<W>,
    ids: PdfObjectIds,
    page_count: usize,
    pages: I,
    text_pages: Option<&[PdfTextPage]>,
    text_encoding: Option<&PdfTextEncoding>,
    timing: &mut Option<&mut dyn FnMut(DjvuPdfTimingEvent)>,
) -> Result<usize, E>
where
    W: Write,
    I: IntoIterator<Item = Result<PdfPageImage, E>>,
    E: From<PdfError>,
{
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
        write_pdf_single_page_objects(
            pdf,
            ids,
            index,
            &page_image,
            text_pages.and_then(|pages| pages.get(index)),
            text_encoding,
            timing,
        )?;
    }

    Ok(actual_page_count)
}

fn write_pdf_single_page_objects<W, E>(
    pdf: &mut PdfObjectWriter<W>,
    ids: PdfObjectIds,
    index: usize,
    page_image: &PdfPageImage,
    text_page: Option<&PdfTextPage>,
    text_encoding: Option<&PdfTextEncoding>,
    timing: &mut Option<&mut dyn FnMut(DjvuPdfTimingEvent)>,
) -> Result<(), E>
where
    W: Write,
    E: From<PdfError>,
{
    let page_object_id = ids.first_page + index;
    let content_id = ids.first_content + index;
    let image_id = ids.first_image + index;
    let image_name = format!("Im{}", index + 1);
    let width_points = points_from_pixels(page_image.width(), page_image.dpi());
    let height_points = points_from_pixels(page_image.height(), page_image.dpi());
    let font_resource = if text_encoding.is_some() {
        format!(" /Font << /Ftxt {} 0 R >>", ids.font)
    } else {
        String::new()
    };
    let page = format!(
        "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {width_points} {height_points}] /Resources << /XObject << /{image_name} {image_id} 0 R >>{font_resource} >> /Contents {content_id} 0 R >>",
        ids.pages
    );
    pdf.write_object(page_object_id, page.as_bytes())
        .map_err(E::from)?;

    write_pdf_page_content(
        pdf,
        content_id,
        PdfPageContent {
            image_name: &image_name,
            page_image,
            text_page,
            text_encoding,
            width_points: &width_points,
            height_points: &height_points,
        },
    )?;
    write_pdf_page_image(pdf, image_id, page_image, timing)
}

#[derive(Clone, Copy)]
struct PdfPageContent<'a> {
    image_name: &'a str,
    page_image: &'a PdfPageImage,
    text_page: Option<&'a PdfTextPage>,
    text_encoding: Option<&'a PdfTextEncoding>,
    width_points: &'a str,
    height_points: &'a str,
}

fn write_pdf_page_content<W, E>(
    pdf: &mut PdfObjectWriter<W>,
    content_id: usize,
    page: PdfPageContent<'_>,
) -> Result<(), E>
where
    W: Write,
    E: From<PdfError>,
{
    let width_points = page.width_points;
    let height_points = page.height_points;
    let image_name = page.image_name;
    let mut content = if page.page_image.is_bitonal_mask() {
        format!(
            "q\n1 g\n0 0 {width_points} {height_points} re f\n0 g\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n"
        )
    } else {
        format!("q\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n")
    };
    if let (Some(text_page), Some(text_encoding)) = (page.text_page, page.text_encoding) {
        append_pdf_text_layer(
            &mut content,
            text_page,
            page.page_image.dpi(),
            text_encoding,
        );
    }
    let content_stream = stream_object(&[], content.as_bytes());
    pdf.write_object(content_id, &content_stream)
        .map_err(E::from)
}

fn write_pdf_page_image<W, E>(
    pdf: &mut PdfObjectWriter<W>,
    image_id: usize,
    page_image: &PdfPageImage,
    timing: &mut Option<&mut dyn FnMut(DjvuPdfTimingEvent)>,
) -> Result<(), E>
where
    W: Write,
    E: From<PdfError>,
{
    let image_start = timing.is_some().then(Instant::now);
    let encode_start = timing.is_some().then(Instant::now);
    let image_stream = page_image.image_stream_object();
    if let Some(encode_start) = encode_start
        && let Some(timing) = timing.as_deref_mut()
    {
        let stage = if page_image.is_bitonal_mask() {
            DjvuPdfTimingStage::CcittEncode
        } else {
            DjvuPdfTimingStage::ImageBytes
        };
        timing(DjvuPdfTimingEvent::aggregate(stage, encode_start.elapsed()));
    }
    let write_start = timing.is_some().then(Instant::now);
    pdf.write_object(image_id, &image_stream).map_err(E::from)?;
    if let Some(write_start) = write_start
        && let Some(timing) = timing.as_deref_mut()
    {
        timing(DjvuPdfTimingEvent::aggregate(
            DjvuPdfTimingStage::PdfObjectWrite,
            write_start.elapsed(),
        ));
    }
    if let Some(image_start) = image_start
        && let Some(timing) = timing.as_deref_mut()
    {
        timing(DjvuPdfTimingEvent::aggregate(
            DjvuPdfTimingStage::PdfWrite,
            image_start.elapsed(),
        ));
    }

    Ok(())
}

fn write_pdf_text_objects<W: Write>(
    pdf: &mut PdfObjectWriter<W>,
    ids: PdfObjectIds,
    text_encoding: Option<&PdfTextEncoding>,
) -> PdfResult<()> {
    let Some(text_encoding) = text_encoding else {
        return Ok(());
    };
    pdf.write_object(
        ids.font,
        format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /ToUnicode {} 0 R >>",
            ids.to_unicode
        )
        .as_bytes(),
    )?;
    let to_unicode_stream = stream_object(&[], pdf_to_unicode_cmap(text_encoding).as_bytes());
    pdf.write_object(ids.to_unicode, &to_unicode_stream)
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
    let mut bitonal_dictionary_cache = Jb2DictionaryCache::new();

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
                let page_total_start = pdf_timing_start(&timing);
                let page = page?;
                let plan_start = pdf_timing_start(&timing);
                let plan = document.page_render_plan(bytes, &page, &tail_entries)?;
                record_pdf_timing(&timing, DjvuPdfTimingStage::PagePlan, plan_start);
                pdf_page_image_from_plan(
                    bytes,
                    &plan,
                    page_number,
                    page_total_start,
                    &timing,
                    &mut bitonal_dictionary_cache,
                    &mut event,
                )
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
        timing(DjvuPdfTimingEvent::aggregate(stage, start.elapsed()));
    }
}

fn record_pdf_page_timing(
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
    stage: DjvuPdfTimingStage,
    page_number: usize,
    start: Option<Instant>,
) {
    if let Some(start) = start
        && let Some(timing) = timing.borrow_mut().as_deref_mut()
    {
        timing(DjvuPdfTimingEvent::page(
            stage,
            page_number,
            start.elapsed(),
        ));
    }
}

fn record_pdf_page_kind_timing(
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
    stage: DjvuPdfTimingStage,
    page_number: usize,
    page_kind: DjvuPdfPageKind,
    start: Option<Instant>,
) {
    if let Some(start) = start
        && let Some(timing) = timing.borrow_mut().as_deref_mut()
    {
        timing(DjvuPdfTimingEvent::page_kind(
            stage,
            page_number,
            page_kind,
            start.elapsed(),
        ));
    }
}

fn pdf_page_image_from_plan(
    bytes: &[u8],
    plan: &crate::PageRenderPlan<'_>,
    page_number: usize,
    page_total_start: Option<Instant>,
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
    bitonal_dictionary_cache: &mut Jb2DictionaryCache,
    event: &mut impl FnMut(DjvuPdfRenderEvent<'_>),
) -> DjvuPdfResult<PdfPageImage> {
    let direct_start = pdf_timing_start(timing);
    if let Some(image) =
        direct_bitonal_pdf_page_image(bytes, plan, page_number, timing, bitonal_dictionary_cache)?
    {
        record_pdf_page_kind_timing(
            timing,
            DjvuPdfTimingStage::DirectBitonal,
            page_number,
            DjvuPdfPageKind::DirectBitonal,
            direct_start,
        );
        record_pdf_page_kind_timing(
            timing,
            DjvuPdfTimingStage::PageTotal,
            page_number,
            DjvuPdfPageKind::DirectBitonal,
            page_total_start,
        );
        event(DjvuPdfRenderEvent::PageImagePrepared {
            page_number,
            image: &image,
        });
        return Ok(image);
    }

    let render_start = pdf_timing_start(timing);
    let render = render_full_page_with_pdf_timings(
        bytes,
        plan,
        page_number,
        timing,
        bitonal_dictionary_cache,
    )?;
    record_pdf_page_kind_timing(
        timing,
        DjvuPdfTimingStage::FallbackRender,
        page_number,
        DjvuPdfPageKind::FallbackRgb,
        render_start,
    );
    event(DjvuPdfRenderEvent::PageRendered {
        page_number,
        render: &render,
    });

    let image_start = pdf_timing_start(timing);
    let image = PdfPageImage::from_render(&render);
    record_pdf_page_timing(
        timing,
        DjvuPdfTimingStage::ImageBytes,
        page_number,
        image_start,
    );
    record_pdf_page_kind_timing(
        timing,
        DjvuPdfTimingStage::PageTotal,
        page_number,
        DjvuPdfPageKind::FallbackRgb,
        page_total_start,
    );

    Ok(image)
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
    page_number: usize,
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
    bitonal_dictionary_cache: &mut Jb2DictionaryCache,
) -> Result<Option<PdfPageImage>, RenderError> {
    if !plan.background_layers.is_empty() || !plan.foreground_layers.is_empty() {
        return Ok(None);
    }

    let jb2_start = pdf_timing_start(timing);
    let masks = bitonal_masks_with_cache(plan, bytes, bitonal_dictionary_cache)
        .map_err(RenderError::from)?;
    record_pdf_page_timing(
        timing,
        DjvuPdfTimingStage::Jb2Decode,
        page_number,
        jb2_start,
    );
    let [(.., partial)] = masks.as_slice() else {
        return Ok(None);
    };
    if partial.mask.width != u32::from(plan.info.width)
        || partial.mask.height != u32::from(plan.info.height)
    {
        return Ok(None);
    }

    let image_start = pdf_timing_start(timing);
    let mask = partial.mask.to_image_mask_bytes();
    record_pdf_page_timing(
        timing,
        DjvuPdfTimingStage::ImageBytes,
        page_number,
        image_start,
    );

    Ok(Some(PdfPageImage::BitonalMask {
        width: partial.mask.width,
        height: partial.mask.height,
        dpi: plan.info.dpi,
        mask,
    }))
}

fn bitonal_masks_with_cache(
    plan: &crate::PageRenderPlan<'_>,
    bytes: &[u8],
    bitonal_dictionary_cache: &mut Jb2DictionaryCache,
) -> Result<Vec<(usize, Jb2PartialImage)>, Jb2Error> {
    let dictionary = bitonal_dictionary_with_cache(plan, bytes, bitonal_dictionary_cache)?;
    plan.bitonal_image_payloads(bytes)
        .into_iter()
        .map(|payload| {
            let image = dictionary.map_or_else(
                || render_jb2_image(payload.bytes),
                |dictionary| render_jb2_image_with_dictionary(payload.bytes, dictionary),
            )?;
            Ok((payload.index, image))
        })
        .collect()
}

fn bitonal_dictionary_with_cache<'cache>(
    plan: &crate::PageRenderPlan<'_>,
    bytes: &[u8],
    bitonal_dictionary_cache: &'cache mut Jb2DictionaryCache,
) -> Result<Option<&'cache Jb2Dictionary>, Jb2Error> {
    let mut last_key = None;
    for payload in plan.bitonal_dictionary_payloads(bytes) {
        let key = (payload.bytes.as_ptr() as usize, payload.bytes.len());
        if let std::collections::btree_map::Entry::Vacant(entry) =
            bitonal_dictionary_cache.entry(key)
        {
            entry.insert(decode_jb2_dictionary(payload.bytes)?);
        }
        last_key = Some(key);
    }

    Ok(last_key.and_then(|key| bitonal_dictionary_cache.get(&key)))
}

fn render_full_page_with_pdf_timings(
    bytes: &[u8],
    plan: &crate::PageRenderPlan<'_>,
    page_number: usize,
    timing: &RefCell<Option<&mut dyn FnMut(DjvuPdfTimingEvent)>>,
    bitonal_dictionary_cache: &mut Jb2DictionaryCache,
) -> Result<PartialPageRender, RenderError> {
    let mut iw44_layers = Vec::with_capacity(2);
    let mut bitmap = plan.render_base_bitmap();

    let background_start = pdf_timing_start(timing);
    let background = plan.background_iw44_layer(bytes)?;
    record_pdf_page_timing(
        timing,
        DjvuPdfTimingStage::Iw44Decode,
        page_number,
        background_start,
    );
    if let Some(background) = background {
        let composite_start = pdf_timing_start(timing);
        if !bitmap.paint_iw44_rgb_layer(&background.image, &background.geometry.mapping) {
            return Err(RenderError::new(format!(
                "background IW44 layer dimensions {}x{} do not map to page {}x{}",
                background.image.width, background.image.height, bitmap.width, bitmap.height
            )));
        }
        record_pdf_page_timing(
            timing,
            DjvuPdfTimingStage::Iw44Composite,
            page_number,
            composite_start,
        );
        iw44_layers.push(background);
    }

    let foreground_start = pdf_timing_start(timing);
    let foreground = plan.foreground_iw44_layer(bytes)?;
    record_pdf_page_timing(
        timing,
        DjvuPdfTimingStage::Iw44Decode,
        page_number,
        foreground_start,
    );

    let jb2_start = pdf_timing_start(timing);
    let bitonal_masks = bitonal_masks_with_cache(plan, bytes, bitonal_dictionary_cache)?;
    record_pdf_page_timing(
        timing,
        DjvuPdfTimingStage::Jb2Decode,
        page_number,
        jb2_start,
    );

    for (chunk_index, partial) in &bitonal_masks {
        let composite_start = pdf_timing_start(timing);
        let painted = if let Some(foreground) = &foreground {
            bitmap.paint_iw44_rgb_layer_through_mask(
                &foreground.image,
                &foreground.geometry.mapping,
                &partial.mask,
            )
        } else {
            bitmap.paint_bitonal_mask(&partial.mask, [0, 0, 0])
        };
        record_pdf_page_timing(
            timing,
            DjvuPdfTimingStage::MaskComposite,
            page_number,
            composite_start,
        );

        if !painted {
            return Err(RenderError::new(format!(
                "bitonal image #{chunk_index} dimensions {}x{} do not match page {}x{}",
                partial.mask.width, partial.mask.height, bitmap.width, bitmap.height
            )));
        }
    }
    if let Some(foreground) = foreground {
        iw44_layers.push(foreground);
    }

    Ok(PartialPageRender {
        bitmap,
        iw44_layers,
        bitonal_masks,
    })
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

        if let Some(pixels) = rgb_pixels_to_gray8(&render.bitmap.pixels) {
            return Self::Gray8 {
                width: render.bitmap.width,
                height: render.bitmap.height,
                dpi: render.bitmap.dpi,
                pixels,
            };
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
            Self::Rgb8 { width, .. }
            | Self::Gray8 { width, .. }
            | Self::BitonalMask { width, .. } => *width,
        }
    }

    const fn height(&self) -> u32 {
        match self {
            Self::Rgb8 { height, .. }
            | Self::Gray8 { height, .. }
            | Self::BitonalMask { height, .. } => *height,
        }
    }

    const fn dpi(&self) -> u16 {
        match self {
            Self::Rgb8 { dpi, .. } | Self::Gray8 { dpi, .. } | Self::BitonalMask { dpi, .. } => {
                *dpi
            }
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
            Self::Gray8 {
                width,
                height,
                pixels,
                ..
            } => image_stream_object(
                &format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceGray /BitsPerComponent 8"
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
        PdfPageImage::Gray8 {
            width,
            height,
            pixels,
            ..
        } => {
            let expected_len = gray_len(*width, *height)?;
            if pixels.len() != expected_len {
                return Err(PdfError::new(format!(
                    "grayscale page has {} bytes, expected {expected_len}",
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

fn rgb_pixels_to_gray8(pixels: &[u8]) -> Option<Vec<u8>> {
    let mut gray = Vec::with_capacity(pixels.len() / 3);
    for pixel in pixels.chunks_exact(3) {
        if pixel[0] != pixel[1] || pixel[1] != pixel[2] {
            return None;
        }
        gray.push(pixel[0]);
    }

    pixels
        .chunks_exact(3)
        .remainder()
        .is_empty()
        .then_some(gray)
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

fn gray_len(width: u32, height: u32) -> PdfResult<usize> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
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
    let run_length = pdf_run_length_encode(bytes);
    let flate = pdf_flate_encode(bytes);
    let mut best = (bytes, "");
    if run_length.len() < best.0.len() {
        best = (&run_length, " /Filter /RunLengthDecode");
    }
    if flate.len() < best.0.len() {
        best = (&flate, " /Filter /FlateDecode");
    }

    stream_object(format!("{dictionary}{}", best.1).as_bytes(), best.0)
}

fn pdf_flate_encode(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .expect("zlib encoder should write to memory");
    encoder.finish().expect("zlib encoder should finish")
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
        let white_bitonal_render = PartialPageRender {
            bitmap: PageBitmap::new_rgb8(8, 1, 72, [0xff, 0xff, 0xff]),
            iw44_layers: Vec::new(),
            bitonal_masks: Vec::new(),
        };

        let pdf = write_rendered_pages_pdf_iter(
            1,
            [Ok::<PartialPageRender, PdfError>(white_bitonal_render)],
        )
        .expect("rendered pages should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/Type /Pages /Count 1 /Kids [3 0 R]"));
        assert!(text.contains("/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"));
    }

    #[test]
    fn write_page_image_pdf_embeds_gray8_page_as_device_gray() {
        let page = PdfPageImage::Gray8 {
            width: 2,
            height: 1,
            dpi: 72,
            pixels: vec![7, 8],
        };

        let pdf = write_page_image_pdf(&[page]).expect("PDF should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/ColorSpace /DeviceGray /BitsPerComponent 8"));
        assert!(!text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
    }

    #[test]
    fn rgb_pixels_to_gray8_accepts_only_exact_gray_pixels() {
        assert_eq!(rgb_pixels_to_gray8(&[7, 7, 7, 8, 8, 8]), Some(vec![7, 8]));
        assert_eq!(rgb_pixels_to_gray8(&[7, 7, 8]), None);
        assert_eq!(rgb_pixels_to_gray8(&[7, 7]), None);
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
