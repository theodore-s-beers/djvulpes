use crate::{
    BzzError, DjvuPageRenderEvent, DjvuRenderError, PageRenderMode, ParseError, RenderError,
    render::{PageBitmap, PartialPageRender, PixelFormat},
    render_document_pages_with_events,
};
use std::fmt;

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
    PageRendered {
        page_number: usize,
        render: &'a PartialPageRender,
    },
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
    if page_count == 0 {
        return Err(PdfError::new("cannot write a PDF with no pages").into());
    }
    let catalog_id = 1;
    let pages_id = 2;
    let first_page_id = 3;
    let first_content_id = first_page_id + page_count;
    let first_image_id = first_content_id + page_count;
    let mut pdf = b"%PDF-1.4\n%\xff\xff\xff\xff\n".to_vec();
    let mut offsets = vec![0usize; first_image_id + page_count];

    append_pdf_object(
        &mut pdf,
        &mut offsets,
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").as_bytes(),
    );

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", first_page_id + index))
        .collect::<Vec<_>>()
        .join(" ");
    append_pdf_object(
        &mut pdf,
        &mut offsets,
        pages_id,
        format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>").as_bytes(),
    );

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
        let page = format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {width_points} {height_points}] /Resources << /XObject << /{image_name} {image_id} 0 R >> >> /Contents {content_id} 0 R >>"
        );
        append_pdf_object(&mut pdf, &mut offsets, page_object_id, page.as_bytes());

        let content = if page_image.is_bitonal_mask() {
            format!("q\n0 g\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n")
        } else {
            format!("q\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n")
        };
        let content_stream = stream_object(&[], content.as_bytes());
        append_pdf_object(&mut pdf, &mut offsets, content_id, &content_stream);

        let (dictionary, image_bytes) = page_image.image_stream_parts();
        let image_stream = image_stream_object(&dictionary, image_bytes);
        append_pdf_object(&mut pdf, &mut offsets, image_id, &image_stream);
    }

    if actual_page_count != page_count {
        return Err(PdfError::new(format!(
            "PDF page iterator produced {actual_page_count} pages, expected {page_count}"
        ))
        .into());
    }

    Ok(finish_pdf(pdf, &offsets, catalog_id))
}

/// Writes a PDF from an iterator of rendered pages.
///
/// This preserves the same image embedding choices as [`PdfPageImage::from_render`]:
/// bitonal-only renders can become 1-bit image masks, while renders containing
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
    write_page_image_pdf_iter(
        page_count,
        renders
            .into_iter()
            .map(|render| render.map(|render| PdfPageImage::from_render(&render))),
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
    render_document_pdf_with_events(bytes, from_page, to_page, |_| {})
}

/// Renders a `DjVu` document byte slice to a PDF, reporting page-level events.
///
/// The event callback is invoked before each selected page is rendered and
/// after the page's compositor output is available. The rendered page reference
/// passed to `PageRendered` is valid only for the duration of the callback.
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
    let renders = render_document_pages_with_events(
        bytes,
        from_page,
        to_page,
        PageRenderMode::Full,
        |item| match item {
            DjvuPageRenderEvent::PageStarted {
                page_number,
                end_page,
            } => event(DjvuPdfRenderEvent::PageStarted {
                page_number,
                end_page,
            }),
            DjvuPageRenderEvent::PageRendered {
                page_number,
                render,
            } => event(DjvuPdfRenderEvent::PageRendered {
                page_number,
                render,
            }),
        },
    )?;

    write_rendered_pages_pdf_iter(
        renders.len(),
        renders
            .into_iter()
            .map(|page| Ok::<PartialPageRender, DjvuPdfError>(page.render)),
    )
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

    fn image_stream_parts(&self) -> (String, &[u8]) {
        match self {
            Self::Rgb8 {
                width,
                height,
                pixels,
                ..
            } => (
                format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8"
                ),
                pixels,
            ),
            Self::BitonalMask {
                width,
                height,
                mask,
                ..
            } => (
                format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ImageMask true /BitsPerComponent 1"
                ),
                mask,
            ),
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

fn append_pdf_object(pdf: &mut Vec<u8>, offsets: &mut [usize], id: usize, object: &[u8]) {
    offsets[id] = pdf.len();
    pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
    pdf.extend_from_slice(object);
    pdf.extend_from_slice(b"\nendobj\n");
}

fn finish_pdf(mut pdf: Vec<u8>, offsets: &[usize], root_id: usize) -> Vec<u8> {
    let xref_start = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {root_id} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );

    pdf
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
        assert!(text.contains("/ImageMask true /BitsPerComponent 1"));
        assert!(text.contains("/Filter /RunLengthDecode"));
        assert!(text.contains("/Length 3"));
        assert!(text.contains("0 g\n32.0000 0 0 1.0000 0 0 cm"));
        assert!(pdf.windows(3).any(|window| window == [253, 0, 128]));
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
        assert!(text.contains("/ImageMask true /BitsPerComponent 1"));
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
                events.push((page_number, end_page, false, 0, 0));
            }
            DjvuPdfRenderEvent::PageRendered {
                page_number,
                render,
            } => {
                events.push((
                    page_number,
                    page_number,
                    true,
                    render.bitmap.width,
                    render.bitmap.height,
                ));
            }
        })
        .expect("fixture page should render to PDF");
        let text = String::from_utf8_lossy(&pdf);

        assert_eq!(events, [(68, 68, false, 0, 0), (68, 68, true, 3423, 5075),]);
        assert!(text.contains("/Type /Pages /Count 1"));
        assert!(text.contains("/ImageMask true /BitsPerComponent 1"));
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
