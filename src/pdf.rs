use crate::render::{PageBitmap, PixelFormat};
use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PdfError(String);

pub type PdfResult<T> = Result<T, PdfError>;

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
    if bitmaps.is_empty() {
        return Err(PdfError::new("cannot write a PDF with no pages"));
    }

    for bitmap in bitmaps {
        validate_bitmap(bitmap)?;
    }

    let page_count = bitmaps.len();
    let catalog_id = 1;
    let pages_id = 2;
    let first_page_id = 3;
    let first_content_id = first_page_id + page_count;
    let first_image_id = first_content_id + page_count;
    let mut objects = Vec::with_capacity(2 + page_count * 3);

    objects.push((
        catalog_id,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    ));

    let kids = (0..page_count)
        .map(|index| format!("{} 0 R", first_page_id + index))
        .collect::<Vec<_>>()
        .join(" ");
    objects.push((
        pages_id,
        format!("<< /Type /Pages /Count {page_count} /Kids [{kids}] >>").into_bytes(),
    ));

    for (index, bitmap) in bitmaps.iter().enumerate() {
        let page_object_id = first_page_id + index;
        let content_id = first_content_id + index;
        let image_id = first_image_id + index;
        let image_name = format!("Im{}", index + 1);
        let width_points = points_from_pixels(bitmap.width, bitmap.dpi);
        let height_points = points_from_pixels(bitmap.height, bitmap.dpi);
        let page = format!(
            "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {width_points} {height_points}] /Resources << /XObject << /{image_name} {image_id} 0 R >> >> /Contents {content_id} 0 R >>"
        );
        objects.push((page_object_id, page.into_bytes()));

        let content =
            format!("q\n{width_points} 0 0 {height_points} 0 0 cm\n/{image_name} Do\nQ\n");
        objects.push((content_id, stream_object(&[], content.as_bytes())));

        let dictionary = format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8",
            bitmap.width, bitmap.height
        );
        objects.push((
            image_id,
            stream_object(dictionary.as_bytes(), &bitmap.pixels),
        ));
    }

    Ok(write_pdf_objects(&objects, catalog_id))
}

fn validate_bitmap(bitmap: &PageBitmap) -> PdfResult<()> {
    if bitmap.format != PixelFormat::Rgb8 {
        return Err(PdfError::new("only RGB8 bitmaps can be written to PDF"));
    }

    let expected_len = usize::try_from(bitmap.width)
        .ok()
        .and_then(|width| {
            usize::try_from(bitmap.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| PdfError::new("bitmap dimensions overflow"))?;
    if bitmap.pixels.len() != expected_len {
        return Err(PdfError::new(format!(
            "bitmap has {} bytes, expected {expected_len}",
            bitmap.pixels.len()
        )));
    }

    Ok(())
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

fn write_pdf_objects(objects: &[(usize, Vec<u8>)], root_id: usize) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n%\xff\xff\xff\xff\n".to_vec();
    let mut offsets = vec![0usize; objects.len() + 1];

    for (id, object) in objects {
        offsets[*id] = pdf.len();
        pdf.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }

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
        assert!(text.contains("/Length 6"));
        assert!(text.contains("xref\n0 6\n"));
        assert!(text.contains("startxref\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
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
