use anyhow::{Context, bail};
use djvulpes::{
    Bookmark, extract_document_bookmarks, extract_document_text, format_document_text_zones,
};
use djvulpes::{
    Chunk, DirectoryEntry, Document, DocumentFormKind, Form, PageChunk, PageChunkKind,
    PageChunkPayload, PageChunkSource, PageRenderMode, PageRenderPlan, ParseResult,
    PartialPageRender, PdfPageImage, TextZone, parse_chunks, parse_dirm_tail, parse_form_at,
    parse_text_payload, parse_text_zones, read_page_details,
    render_document_pdf_to_writer_with_events_and_timings, render_document_pdf_with_events,
};
use djvulpes::{DjvuPdfTimingEvent, DjvuPdfTimingStage};
use djvulpes::{decode_bzz, decode_dirm_tail};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::Duration;

mod compare;
mod dump;
mod iw44_inspect;
pub use compare::{
    run_compare_ppm, run_compare_render_page, run_compare_render_page_layer,
    run_compare_render_pages,
};
pub use dump::{run_dump_bitonal, run_dump_image_layers};
pub use iw44_inspect::run_inspect_iw44_pixel;

const JB2_PLAN_PREFIX_RECORD_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderPdfOptions {
    pub progress: RenderPdfProgress,
    pub verbose: bool,
    pub timings: bool,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum RenderPdfProgress {
    #[default]
    Sparse,
    PerPage,
    Quiet,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44PixelInspectOptions {
    pub x: u32,
    pub y: u32,
    pub radius: u8,
    pub coefficient_limit: usize,
    pub coefficient_indices: Vec<usize>,
    pub traces: Vec<Iw44PixelTrace>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Iw44PixelTrace {
    Coefficients,
    Slices,
    Events,
    Reconstruction,
}

pub fn run_summary(path: &Path) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;

    println!("file: {}", path.display());
    println!("bytes: {}", bytes.len());

    let root = &document.root;
    println!(
        "root: FORM:{} size={} data=[{}..{})",
        root.kind, root.chunk.size, root.chunk.data_start, root.chunk.data_end
    );

    println!();
    println!("root chunks:");
    println!("  total: {}", document.root_chunks.len());
    print_root_chunk_counts(&document, &bytes)?;
    print_root_chunk_sample(&bytes, &document.root_chunks)?;

    if document.directory.is_some() {
        println!();
        print_dirm_summary(&document, &bytes)?;
    }

    Ok(())
}

pub fn run_pages(path: &Path) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;

    println!("file: {}", path.display());
    println!("pages: {}", document.form_kind_counts().pages);
    println!();
    println!("page  offset    size      dimensions  dpi  rotation");

    for (index, page) in document.pages(&bytes).enumerate() {
        let page = page?;
        print!(
            "{:<5} @{:<8} {:<9}",
            index + 1,
            page.offset,
            page.form.chunk.size
        );

        if let Some(info) = page.info {
            println!(
                "{:>5}x{:<5} {:<4} {}",
                info.width, info.height, info.dpi, info.rotation
            );
        } else {
            println!("missing     -    -");
        }
    }

    Ok(())
}

pub fn run_forms(path: &Path) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;

    println!("file: {}", path.display());
    println!("forms: {}", document.forms.len());
    println!(
        "unresolved offsets: {}",
        document.unresolved_directory_offsets
    );
    println!();
    println!("index  offset    kind    size");

    for (index, document_form) in document.forms.iter().enumerate() {
        println!(
            "{:<6} @{:<8} {:<7} {}",
            index + 1,
            document_form.offset,
            display_form_kind(document_form.kind),
            document_form.form.chunk.size
        );
    }

    Ok(())
}

pub fn run_form(path: &Path, offset: usize) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;
    let form = parse_form_at(&bytes, offset)?;

    println!("file: {}", path.display());
    if let Some(dirm) = &document.directory
        && let Ok(decoded_tail) = decode_dirm_tail(&bytes, dirm)
    {
        let entries = parse_dirm_tail(dirm, &decoded_tail)?;
        let directory_entries = document.directory_entries(&entries);
        if let Some(entry) = directory_entries
            .iter()
            .find(|entry| entry.offset as usize == offset)
        {
            println!(
                "directory: name={} size={} flags=0x{:02x} kind={}",
                entry.name,
                entry.size,
                entry.flags,
                entry.kind.map_or("-", display_form_kind)
            );
        }
    }
    print_form_detail(&bytes, &form, offset)?;

    Ok(())
}

pub fn run_dirm(path: &Path) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;
    let Some(dirm) = &document.directory else {
        bail!("document has no DIRM chunk");
    };

    println!("file: {}", path.display());
    print_dirm_summary(&document, &bytes)?;

    let tail = dirm.compressed_tail(&bytes)?;
    println!();
    println!(
        "compressed tail: [{}..{}) len={}",
        dirm.compressed_tail_start,
        dirm.compressed_tail_end(),
        tail.len()
    );

    let decoded_tail = match decode_dirm_tail(&bytes, dirm) {
        Ok(decoded_tail) => decoded_tail,
        Err(error) => {
            println!("decoded tail: unavailable ({error})");
            return Ok(());
        }
    };

    let entries = parse_dirm_tail(dirm, &decoded_tail)?;
    let resolved_entries = document.directory_entries(&entries);
    println!("decoded tail: {} bytes", decoded_tail.len());
    println!();
    println!("first directory entries:");
    println!("index  offset    size      flags  kind    name");

    for (index, entry) in resolved_entries.iter().take(24).enumerate() {
        println!(
            "{:<6} @{:<8} {:<9} 0x{:02x}   {:<7} {}",
            index + 1,
            entry.offset,
            entry.size,
            entry.flags,
            entry.kind.map_or("-", display_form_kind),
            entry.name
        );
    }

    if resolved_entries.len() > 24 {
        println!(
            "... {} more directory entries omitted",
            resolved_entries.len() - 24
        );
    }

    print_include_resolution(&bytes, &document, &resolved_entries)?;

    Ok(())
}

pub fn run_page(path: &Path, number: usize) -> anyhow::Result<()> {
    if number == 0 {
        bail!("page number must be 1 or greater");
    }

    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;
    let Some(document_form) = document
        .forms
        .iter()
        .filter(|form| form.kind == DocumentFormKind::Page)
        .nth(number - 1)
    else {
        bail!(
            "page {number} not found; document has {} pages",
            document.form_kind_counts().pages
        );
    };

    println!("file: {}", path.display());
    println!("page: {number}");

    if let Some(dirm) = &document.directory
        && let Ok(decoded_tail) = decode_dirm_tail(&bytes, dirm)
    {
        let entries = parse_dirm_tail(dirm, &decoded_tail)?;
        let resolved_entries = document.directory_entries(&entries);
        print_page_detail(
            &bytes,
            &document_form.form,
            document_form.offset,
            Some((&document, &resolved_entries)),
        )?;
    } else {
        print_page_detail(&bytes, &document_form.form, document_form.offset, None)?;
    }

    Ok(())
}

pub fn run_render_plan(path: &Path, number: usize) -> anyhow::Result<()> {
    if number == 0 {
        bail!("page number must be 1 or greater");
    }

    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;
    let page = document
        .pages(&bytes)
        .nth(number - 1)
        .transpose()?
        .with_context(|| {
            format!(
                "page {number} not found; document has {} pages",
                document.form_kind_counts().pages
            )
        })?;
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(&bytes, dirm)?;
        parse_dirm_tail(dirm, &decoded_tail)?
    } else {
        Vec::new()
    };
    let plan = document.page_render_plan(&bytes, &page, &tail_entries)?;

    println!("file: {}", path.display());
    println!("page: {number}");
    print_render_plan(&plan, &bytes);

    Ok(())
}

pub fn run_render_page(path: &Path, number: usize, output: &Path) -> anyhow::Result<()> {
    let render = render_page(path, number)?;
    let ppm = render.bitmap.to_ppm_bytes();

    fs::write(output, ppm).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!(
        "rendered: {}x{} dpi={} format=PPM/P6",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    );
    println!("output: {}", output.display());
    print_bitmap_stats(&render.bitmap);
    print_partial_render_summary(&render.bitonal_masks);
    print_iw44_render_summary(&render.iw44_layers);

    Ok(())
}

pub fn run_render_page_layer(
    path: &Path,
    number: usize,
    mode: PageRenderMode,
    output: &Path,
) -> anyhow::Result<()> {
    let render = render_page_layer(path, number, mode)?;
    let ppm = render.bitmap.to_ppm_bytes();

    fs::write(output, ppm).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("mode: {}", mode.as_str());
    println!(
        "rendered: {}x{} dpi={} format=PPM/P6",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    );
    println!("output: {}", output.display());
    print_bitmap_stats(&render.bitmap);
    print_partial_render_summary(&render.bitonal_masks);
    print_iw44_render_summary(&render.iw44_layers);

    Ok(())
}

pub fn run_render_page_pdf(path: &Path, number: usize, output: &Path) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let mut rendered = None;
    let mut prepared_image = None;
    let pdf = render_document_pdf_with_events(&bytes, number, Some(number), |event| match event {
        djvulpes::DjvuPdfRenderEvent::PageImagePrepared { image, .. } => {
            prepared_image = Some(image.clone());
        }
        djvulpes::DjvuPdfRenderEvent::PageRendered { render, .. } => {
            rendered = Some(render.clone());
        }
        djvulpes::DjvuPdfRenderEvent::PageStarted { .. } => {}
    })?;

    fs::write(output, pdf).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    if let Some(render) = rendered {
        println!(
            "rendered: {}x{} dpi={} format=PDF",
            render.bitmap.width, render.bitmap.height, render.bitmap.dpi
        );
        print_bitmap_stats(&render.bitmap);
        print_partial_render_summary(&render.bitonal_masks);
        print_iw44_render_summary(&render.iw44_layers);
    } else if let Some(image) = prepared_image {
        print_pdf_page_image_summary(&image);
    } else {
        bail!("render-page-pdf did not prepare the requested page");
    }
    println!("output: {}", output.display());

    Ok(())
}

pub fn run_render_pdf(
    path: &Path,
    output: &Path,
    from_page: usize,
    to_page: Option<usize>,
    options: RenderPdfOptions,
) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let mut render_count = 0usize;
    let mut summaries = Vec::new();
    let mut timing_summary = PdfTimingSummary::default();
    let selected_page_count = to_page.map_or_else(
        || None,
        |to_page| to_page.checked_sub(from_page).map(|offset| offset + 1),
    );
    let mut output_file = fs::File::create(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let mut timing_callback = |event: DjvuPdfTimingEvent| {
        timing_summary.record(event);
    };
    render_document_pdf_to_writer_with_events_and_timings(
        &mut output_file,
        &bytes,
        from_page,
        to_page,
        |event| match event {
            djvulpes::DjvuPdfRenderEvent::PageStarted {
                page_number,
                end_page,
            } => {
                if options.progress == RenderPdfProgress::PerPage {
                    eprintln!("rendering page {page_number} of {end_page}");
                }
            }
            djvulpes::DjvuPdfRenderEvent::PageRendered {
                page_number,
                render,
            } => {
                if options.progress == RenderPdfProgress::PerPage {
                    eprintln!("rendered page {page_number}");
                }
                render_count += 1;
                print_render_pdf_sparse_progress(
                    render_count,
                    selected_page_count,
                    page_number,
                    options.progress,
                );
                if options.verbose {
                    summaries.push(render_pdf_page_summary(page_number, render));
                }
            }
            djvulpes::DjvuPdfRenderEvent::PageImagePrepared { page_number, image } => {
                if options.progress == RenderPdfProgress::PerPage {
                    eprintln!("prepared page {page_number}");
                }
                render_count += 1;
                print_render_pdf_sparse_progress(
                    render_count,
                    selected_page_count,
                    page_number,
                    options.progress,
                );
                if options.verbose {
                    summaries.push(pdf_page_image_summary(page_number, image));
                }
            }
        },
        options
            .timings
            .then_some(&mut timing_callback as &mut dyn FnMut(DjvuPdfTimingEvent)),
    )?;

    println!("wrote {} pages to {}", render_count, output.display());
    if options.verbose {
        println!("file: {}", path.display());
        println!(
            "range: {}..{}",
            from_page,
            to_page.map_or_else(|| "end".to_string(), |page| page.to_string())
        );
        println!("format: PDF");
    }
    for summary in summaries {
        print!("{summary}");
    }
    if options.timings {
        print!("{timing_summary}");
    }

    Ok(())
}

#[derive(Debug, Default)]
struct PdfTimingSummary {
    setup: Duration,
    text_extraction: Duration,
    page_plan: Duration,
    direct_bitonal: Duration,
    fallback_render: Duration,
    pdf_write: Duration,
}

impl PdfTimingSummary {
    fn record(&mut self, event: DjvuPdfTimingEvent) {
        match event.stage {
            DjvuPdfTimingStage::Setup => self.setup += event.duration,
            DjvuPdfTimingStage::TextExtraction => self.text_extraction += event.duration,
            DjvuPdfTimingStage::PagePlan => self.page_plan += event.duration,
            DjvuPdfTimingStage::DirectBitonal => self.direct_bitonal += event.duration,
            DjvuPdfTimingStage::FallbackRender => self.fallback_render += event.duration,
            DjvuPdfTimingStage::PdfWrite => self.pdf_write += event.duration,
        }
    }
}

impl std::fmt::Display for PdfTimingSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(formatter, "timings:")?;
        writeln!(formatter, "  setup: {}", format_duration(self.setup))?;
        writeln!(
            formatter,
            "  text extraction: {}",
            format_duration(self.text_extraction)
        )?;
        writeln!(
            formatter,
            "  page planning: {}",
            format_duration(self.page_plan)
        )?;
        writeln!(
            formatter,
            "  direct bitonal: {}",
            format_duration(self.direct_bitonal)
        )?;
        writeln!(
            formatter,
            "  fallback render: {}",
            format_duration(self.fallback_render)
        )?;
        writeln!(
            formatter,
            "  pdf write/encode: {}",
            format_duration(self.pdf_write)
        )
    }
}

fn format_duration(duration: Duration) -> String {
    format!("{:.3}s", duration.as_secs_f64())
}

fn print_render_pdf_sparse_progress(
    render_count: usize,
    selected_page_count: Option<usize>,
    page_number: usize,
    progress: RenderPdfProgress,
) {
    if progress != RenderPdfProgress::Sparse {
        return;
    }
    if render_count.is_multiple_of(50) || selected_page_count == Some(render_count) {
        eprintln!("processed {render_count} pages; latest page {page_number}");
    }
}

fn render_page(path: &Path, number: usize) -> anyhow::Result<PartialPageRender> {
    let bytes = read_file(path)?;
    djvulpes::render_document_page(&bytes, number, PageRenderMode::Full).map_err(Into::into)
}

fn render_pdf_page_summary(page_number: usize, render: &PartialPageRender) -> String {
    let mut summary = String::new();
    writeln!(&mut summary, "page: {page_number}").expect("writing to string should not fail");
    writeln!(
        &mut summary,
        "rendered: {}x{} dpi={}",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    )
    .expect("writing to string should not fail");
    write_bitmap_stats(&mut summary, &render.bitmap);
    write_partial_render_summary(&mut summary, &render.bitonal_masks);
    write_iw44_render_summary(&mut summary, &render.iw44_layers);

    summary
}

fn pdf_page_image_summary(page_number: usize, image: &PdfPageImage) -> String {
    let mut summary = String::new();
    writeln!(&mut summary, "page: {page_number}").expect("writing to string should not fail");
    write_pdf_page_image_summary(&mut summary, image);

    summary
}

fn print_pdf_page_image_summary(image: &PdfPageImage) {
    let mut summary = String::new();
    write_pdf_page_image_summary(&mut summary, image);
    print!("{summary}");
}

fn write_pdf_page_image_summary(summary: &mut String, image: &PdfPageImage) {
    match image {
        PdfPageImage::Rgb8 {
            width,
            height,
            dpi,
            pixels,
        } => {
            writeln!(
                summary,
                "prepared: {width}x{height} dpi={dpi} format=PDF/RGB bytes={}",
                pixels.len()
            )
            .expect("writing to string should not fail");
        }
        PdfPageImage::BitonalMask {
            width,
            height,
            dpi,
            mask,
        } => {
            writeln!(
                summary,
                "prepared: {width}x{height} dpi={dpi} format=PDF/DeviceGray1 bytes={}",
                mask.len()
            )
            .expect("writing to string should not fail");
        }
    }
}

fn render_page_layer(
    path: &Path,
    number: usize,
    mode: PageRenderMode,
) -> anyhow::Result<PartialPageRender> {
    let bytes = read_file(path)?;
    djvulpes::render_document_page(&bytes, number, mode).map_err(Into::into)
}

fn with_page_render_plan<T>(
    path: &Path,
    number: usize,
    render: impl for<'a> FnOnce(&'a [u8], PageRenderPlan<'a>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if number == 0 {
        bail!("page number must be 1 or greater");
    }

    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;
    let page = document
        .pages(&bytes)
        .nth(number - 1)
        .transpose()?
        .with_context(|| {
            format!(
                "page {number} not found; document has {} pages",
                document.form_kind_counts().pages
            )
        })?;
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(&bytes, dirm)?;
        parse_dirm_tail(dirm, &decoded_tail)?
    } else {
        Vec::new()
    };
    let plan = document.page_render_plan(&bytes, &page, &tail_entries)?;

    render(&bytes, plan)
}

pub fn run_text(path: &Path, number: usize, show_zones: bool) -> anyhow::Result<()> {
    if number == 0 {
        bail!("page number must be 1 or greater");
    }

    let bytes = read_file(path)?;
    let document = Document::parse(&bytes)?;
    let Some(document_form) = document
        .forms
        .iter()
        .filter(|form| form.kind == DocumentFormKind::Page)
        .nth(number - 1)
    else {
        bail!(
            "page {number} not found; document has {} pages",
            document.form_kind_counts().pages
        );
    };
    let details = read_page_details(&bytes, &document_form.form)?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!(
        "form: @{} FORM:{} size={}",
        document_form.offset, details.form.kind, details.form.chunk.size
    );

    let mut found_text = false;
    for chunk in details
        .chunks
        .iter()
        .filter(|chunk| matches!(chunk.kind, PageChunkKind::Txta | PageChunkKind::Txtz))
    {
        found_text = true;
        print_text_chunk(&bytes, chunk, show_zones)?;
    }

    if !found_text {
        println!();
        println!("text: none");
    }

    Ok(())
}

pub fn run_extract_text(
    path: &Path,
    from_page: usize,
    to_page: Option<usize>,
    structured: bool,
) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let text = if structured {
        format_document_text_zones(&bytes, from_page, to_page)?
    } else {
        extract_document_text(&bytes, from_page, to_page)?
    };
    print!("{text}");

    Ok(())
}

pub fn run_outline(path: &Path) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let Some(bookmarks) = extract_document_bookmarks(&bytes)? else {
        return Ok(());
    };

    println!("(bookmarks");
    for (index, bookmark) in bookmarks.iter().enumerate() {
        let trailing_closes = usize::from(index + 1 == bookmarks.len());
        print_bookmark(bookmark, 1, trailing_closes);
    }

    Ok(())
}

fn read_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn print_bookmark(bookmark: &Bookmark, indent: usize, trailing_closes: usize) {
    print!("{}", " ".repeat(indent));
    println!("(\"{}\"", escape_bookmark_string(&bookmark.title));
    print!("{}", " ".repeat(indent + 1));
    print!("\"{}\"", escape_bookmark_string(&bookmark.url));
    if bookmark.children.is_empty() {
        print!(" )");
        for _ in 0..trailing_closes {
            print!(" )");
        }
        println!();
        return;
    }

    println!();
    for (index, child) in bookmark.children.iter().enumerate() {
        let child_trailing_closes = if index + 1 == bookmark.children.len() {
            trailing_closes + 1
        } else {
            0
        };
        print_bookmark(child, indent + 1, child_trailing_closes);
    }
}

fn escape_bookmark_string(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'"' => escaped.push_str("\\\""),
            b'\\' => escaped.push_str("\\\\"),
            0x20..=0x7e => escaped.push(char::from(*byte)),
            _ => {
                write!(&mut escaped, "\\{byte:03o}").expect("writing to string should not fail");
            }
        }
    }

    escaped
}

fn print_root_chunk_counts(document: &Document<'_>, bytes: &[u8]) -> ParseResult<()> {
    let counts = document.root_chunk_counts(bytes)?;
    println!(
        "  counts: DIRM={}, NAVM={}, FORM:DJVU={}, FORM:DJVI={}, FORM:THUM={}, other={}",
        counts.dirm,
        counts.navm,
        counts.djvu_forms,
        counts.djvi_forms,
        counts.thum_forms,
        counts.other
    );
    Ok(())
}

fn print_root_chunk_sample(bytes: &[u8], chunks: &[Chunk<'_>]) -> ParseResult<()> {
    let sample_len = chunks.len().min(16);
    println!("  first {sample_len}:");

    for chunk in chunks.iter().take(sample_len) {
        print_chunk_line(bytes, chunk)?;
    }

    if chunks.len() > sample_len {
        println!(
            "  ... {} more root chunks omitted",
            chunks.len() - sample_len
        );
    }

    Ok(())
}

fn print_chunk_line(bytes: &[u8], chunk: &Chunk<'_>) -> ParseResult<()> {
    if chunk.id == "FORM" {
        let form = parse_form_at(bytes, chunk.data_start - 8)?;
        println!(
            "    @{:<8} FORM:{:<4} size={:<8} data=[{}..{})",
            chunk.data_start - 8,
            form.kind,
            form.chunk.size,
            form.chunk.data_start,
            form.chunk.data_end
        );
    } else {
        println!(
            "    @{:<8} {:<4}      size={:<8} data=[{}..{})",
            chunk.data_start - 8,
            chunk.id,
            chunk.size,
            chunk.data_start,
            chunk.data_end
        );
    }

    Ok(())
}

fn print_form_detail(bytes: &[u8], form: &Form<'_>, offset: usize) -> ParseResult<()> {
    println!(
        "form: @{offset} FORM:{} size={} data=[{}..{})",
        form.kind, form.chunk.size, form.chunk.data_start, form.chunk.data_end
    );

    println!();
    println!("child chunks:");
    let chunks = parse_chunks(bytes, form.children_start, form.chunk.data_end)?;
    for chunk in chunks {
        print_form_child_chunk_line(bytes, &chunk)?;
    }

    Ok(())
}

fn print_form_child_chunk_line(bytes: &[u8], chunk: &Chunk<'_>) -> ParseResult<()> {
    if chunk.id == "FORM" {
        let form = parse_form_at(bytes, chunk.data_start - 8)?;
        println!(
            "    @{:<8} FORM:{:<4} {:<8} size={:<8} data=[{}..{})",
            chunk.data_start - 8,
            form.kind,
            display_chunk_role(chunk.id, Some(form.kind)),
            form.chunk.size,
            form.chunk.data_start,
            form.chunk.data_end
        );
    } else {
        println!(
            "    @{:<8} {:<4} {:<8} size={:<8} data=[{}..{}){}",
            chunk.data_start - 8,
            chunk.id,
            display_chunk_role(chunk.id, None),
            chunk.size,
            chunk.data_start,
            chunk.data_end,
            format_chunk_payload(bytes, chunk)
        );
    }

    Ok(())
}

fn print_dirm_summary(document: &Document<'_>, bytes: &[u8]) -> ParseResult<()> {
    let Some(dirm) = &document.directory else {
        return Ok(());
    };

    println!("DIRM:");
    println!(
        "  flags: 0x{:02x} bundled={} compressed_tail={}",
        dirm.flags,
        dirm.flags & 0x80 != 0,
        dirm.flags & 0x01 != 0
    );
    println!("  entry count: {}", dirm.entry_count);
    println!(
        "  compressed directory tail bytes: {}",
        dirm.compressed_tail_len
    );

    let counts = document.form_kind_counts();
    println!(
        "  referenced forms: {} DJVU pages, {} DJVI shared, {} THUM thumbnails",
        counts.pages, counts.shared, counts.thumbnails
    );
    println!(
        "  unresolved offsets: {}",
        document.unresolved_directory_offsets
    );

    println!();
    println!("first referenced forms:");
    for (index, document_form) in document.forms.iter().take(12).enumerate() {
        print!(
            "  #{:<4} @{:<8} FORM:{:<4} size={:<8}",
            index + 1,
            document_form.offset,
            document_form.form.kind,
            document_form.form.chunk.size
        );

        if let Some(info) = document_form.page(bytes)?.and_then(|page| page.info) {
            print!(
                " INFO {}x{} dpi={} gamma={:.1} version={} rotation={}",
                info.width, info.height, info.dpi, info.gamma, info.version, info.rotation
            );
        }

        println!();
    }

    Ok(())
}

fn print_page_detail(
    bytes: &[u8],
    form: &Form<'_>,
    offset: u32,
    directory_context: Option<(&Document<'_>, &[DirectoryEntry<'_>])>,
) -> ParseResult<()> {
    let details = read_page_details(bytes, form)?;

    println!(
        "form: @{offset} FORM:{} size={} data=[{}..{})",
        details.form.kind,
        details.form.chunk.size,
        details.form.chunk.data_start,
        details.form.chunk.data_end
    );

    if let Some(info) = details.info {
        println!(
            "INFO: {}x{} dpi={} gamma={:.1} version={} rotation={}",
            info.width, info.height, info.dpi, info.gamma, info.version, info.rotation
        );
    }

    println!();
    println!("child chunks:");
    for page_chunk in details.chunks {
        print_page_chunk_line(bytes, &page_chunk, directory_context)?;
    }

    Ok(())
}

fn print_render_plan(plan: &PageRenderPlan<'_>, bytes: &[u8]) {
    println!(
        "INFO: {}x{} dpi={} gamma={:.1} version={} rotation={}",
        plan.info.width,
        plan.info.height,
        plan.info.dpi,
        plan.info.gamma,
        plan.info.version,
        plan.info.rotation
    );
    println!(
        "layers: bitonal dictionaries={} bitonal images={} foreground={} background={} text={} unknown={}",
        plan.bitonal_dictionaries.len(),
        plan.bitonal_images.len(),
        plan.foreground_layers.len(),
        plan.background_layers.len(),
        plan.text_chunks.len(),
        plan.unknown_chunks.len()
    );
    println!("has image data: {}", plan.has_image_data());
    println!("has text: {}", plan.has_text());
    if !plan.bitonal_images.is_empty() {
        match plan.bitonal_image_headers(bytes) {
            Ok(headers) => {
                for image in headers {
                    println!(
                        "bitonal image #{}: JB2 {}x{} inherited_dict_symbols={}",
                        image.index,
                        image.header.width,
                        image.header.height,
                        image.header.inherited_dictionary_symbols
                    );
                }
            }
            Err(error) => println!("bitonal image headers: unavailable ({error})"),
        }
        match plan.bitonal_record_prefixes(bytes, JB2_PLAN_PREFIX_RECORD_LIMIT) {
            Ok(prefixes) => {
                for (chunk_index, prefix) in prefixes {
                    print!(
                        "bitonal image #{chunk_index}: first {} JB2 records",
                        prefix.records.len()
                    );
                    if let Some(kind) = prefix.stopped_before {
                        print!("; stopped before {}", kind.as_str());
                    }
                    println!();
                    for record in prefix.records.iter().take(JB2_PLAN_PREFIX_RECORD_LIMIT) {
                        println!(
                            "  rec #{:<3} {:<27}{}",
                            record.index,
                            record.kind.as_str(),
                            format_jb2_record_geometry(record)
                        );
                    }
                }
            }
            Err(error) => println!("bitonal record prefix: unavailable ({error})"),
        }
        match plan.bitonal_masks(bytes) {
            Ok(partials) => {
                for (chunk_index, partial) in partials {
                    println!(
                        "bitonal image #{chunk_index}: mask black_pixels={} dictionary_symbols={} end_of_data={} stopped_before={}",
                        partial.mask.black_pixel_count(),
                        partial.dictionary_symbol_count,
                        partial.reached_end_of_data,
                        partial
                            .stopped_before
                            .map_or("none", djvulpes::Jb2RecordKind::as_str)
                    );
                }
            }
            Err(error) => println!("bitonal mask: unavailable ({error})"),
        }
    }
    print_iw44_layer_plan(plan, bytes);
    println!();
    println!("effective chunks:");
    println!("index  source      chunk  role      size      offset");

    for (index, chunk) in plan.chunks.iter().enumerate() {
        println!(
            "{:<6} {:<11} {:<5} {:<9} {:<9} @{}",
            index,
            format_page_chunk_source(chunk.source),
            chunk.chunk.chunk.id,
            chunk.chunk.kind.as_str(),
            chunk.chunk.chunk.size,
            chunk.chunk.chunk.data_start - 8
        );
    }
}

fn print_iw44_layer_plan(plan: &PageRenderPlan<'_>, bytes: &[u8]) {
    let foreground = plan.foreground_layer_payloads(bytes);
    let background = plan.background_layer_payloads(bytes);
    if foreground.is_empty() && background.is_empty() {
        return;
    }

    println!("IW44 layers:");
    print_iw44_layer_summary(
        "foreground",
        "FG44",
        plan.foreground_layer_geometry(bytes),
        &foreground,
    );
    print_iw44_layer_summary(
        "background",
        "BG44",
        plan.background_layer_geometry(bytes),
        &background,
    );
}

fn print_iw44_layer_summary(
    role: &str,
    chunk_id: &str,
    geometry: Result<Option<djvulpes::Iw44LayerGeometry>, djvulpes::Iw44Error>,
    payloads: &[djvulpes::RenderChunkPayload<'_>],
) {
    if payloads.is_empty() {
        return;
    }

    match geometry {
        Ok(Some(geometry)) => {
            let summary = geometry.summary;
            let mapping = geometry.mapping;
            println!(
                "  {role}: chunks={} total_slices={} payload_bytes={} image={}x{} subsample={} scaled={}x{} overscan={}x{} grayscale={} chroma_half={} delay={}",
                summary.chunks.len(),
                summary.total_slices,
                summary.total_payload_bytes,
                summary.image.width,
                summary.image.height,
                mapping.subsample,
                mapping.scaled_width,
                mapping.scaled_height,
                mapping.horizontal_overscan,
                mapping.vertical_overscan,
                summary.image.grayscale,
                summary.image.chroma_half,
                summary.image.delay
            );
        }
        Ok(None) => {}
        Err(error) => println!("  {role}: {chunk_id} layer_error={error}"),
    }

    for payload in payloads {
        print_iw44_payload_summary(role, chunk_id, payload.index, payload.bytes);
    }
}

fn print_iw44_payload_summary(role: &str, chunk_id: &str, index: usize, bytes: &[u8]) {
    match djvulpes::read_iw44_chunk_header(bytes) {
        Ok(header) => {
            print!(
                "  {role} chunk #{index}: {chunk_id} bytes={} serial={} slices={} payload_bytes={}",
                bytes.len(),
                header.serial,
                header.slices,
                header.payload_len
            );
            if let Some(image) = header.image {
                print!(
                    " image={}x{} grayscale={} chroma_half={} delay={}",
                    image.width, image.height, image.grayscale, image.chroma_half, image.delay
                );
            }
            println!();
        }
        Err(error) => {
            println!(
                "  {role} chunk #{index}: {chunk_id} bytes={} header_error={error}",
                bytes.len()
            );
        }
    }
}

fn print_decoded_iw44_payload_summary(
    role: &str,
    payloads: &[djvulpes::RenderChunkPayload<'_>],
) -> anyhow::Result<()> {
    if payloads.is_empty() {
        return Ok(());
    }

    let mut decoder = djvulpes::Iw44Decoder::new();
    for payload in payloads {
        decoder
            .decode_chunk(payload.bytes)
            .with_context(|| format!("failed to decode {role} IW44 chunk #{}", payload.index))?;
    }

    println!(
        "decoded IW44 {role}: chunks={} slices={} payload_bytes={}",
        decoder.chunks_decoded(),
        decoder.slices_decoded(),
        decoder.payload_bytes_seen()
    );
    for summary in decoder.plane_coefficient_summaries() {
        println!(
            "  {} coefficients: image={}x{} blocks={} non_zero={} max_abs={} abs_sum={}",
            iw44_plane_name(summary.plane),
            summary.width,
            summary.height,
            summary.block_count,
            summary.non_zero_coefficients,
            summary.max_abs_coefficient,
            summary.coefficient_abs_sum
        );
    }
    for summary in reconstruction_summaries(&decoder) {
        println!(
            "  {} reconstruction: image={}x{} samples={} min={} max={} abs_sum={}",
            iw44_plane_name(summary.plane),
            summary.width,
            summary.height,
            summary.sample_count,
            summary.min_sample,
            summary.max_sample,
            summary.sample_abs_sum
        );
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44ReconstructionSummary {
    plane: djvulpes::Iw44Plane,
    width: usize,
    height: usize,
    sample_count: usize,
    min_sample: i16,
    max_sample: i16,
    sample_abs_sum: u64,
}

fn reconstruction_summaries(decoder: &djvulpes::Iw44Decoder) -> Vec<Iw44ReconstructionSummary> {
    decoder
        .reconstruct_planes()
        .into_iter()
        .map(|plane| {
            let mut min_sample = i16::MAX;
            let mut max_sample = i16::MIN;
            let mut sample_abs_sum = 0u64;

            for sample in &plane.samples {
                min_sample = min_sample.min(*sample);
                max_sample = max_sample.max(*sample);
                sample_abs_sum += u64::from(sample.unsigned_abs());
            }

            Iw44ReconstructionSummary {
                plane: plane.plane,
                width: plane.width,
                height: plane.height,
                sample_count: plane.samples.len(),
                min_sample,
                max_sample,
                sample_abs_sum,
            }
        })
        .collect()
}

const fn iw44_plane_name(plane: djvulpes::Iw44Plane) -> &'static str {
    match plane {
        djvulpes::Iw44Plane::Y => "Y",
        djvulpes::Iw44Plane::Cb => "Cb",
        djvulpes::Iw44Plane::Cr => "Cr",
    }
}

fn print_partial_render_summary(bitonal_masks: &[(usize, djvulpes::Jb2PartialImage)]) {
    let mut summary = String::new();
    write_partial_render_summary(&mut summary, bitonal_masks);
    print!("{summary}");
}

fn write_partial_render_summary(
    output: &mut String,
    bitonal_masks: &[(usize, djvulpes::Jb2PartialImage)],
) {
    for (chunk_index, partial) in bitonal_masks {
        writeln!(
            output,
            "painted bitonal image #{chunk_index}: black_pixels={} dictionary_symbols={} end_of_data={} stopped_before={}",
            partial.mask.black_pixel_count(),
            partial.dictionary_symbol_count,
            partial.reached_end_of_data,
            partial
                .stopped_before
                .map_or("none", djvulpes::Jb2RecordKind::as_str)
        )
        .expect("writing to string should not fail");
    }
}

fn print_bitmap_stats(bitmap: &djvulpes::PageBitmap) {
    let mut summary = String::new();
    write_bitmap_stats(&mut summary, bitmap);
    print!("{summary}");
}

fn write_bitmap_stats(output: &mut String, bitmap: &djvulpes::PageBitmap) {
    let stats = bitmap.stats();
    writeln!(
        output,
        "bitmap stats: pixels={} black={} white={} non_gray={} component_sum={} fingerprint={:016x}",
        stats.pixel_count,
        stats.black_pixels,
        stats.white_pixels,
        stats.non_gray_pixels,
        stats.component_sum,
        stats.fingerprint
    )
    .expect("writing to string should not fail");
}

fn print_iw44_render_summary(iw44_layers: &[djvulpes::RenderedIw44Layer]) {
    let mut summary = String::new();
    write_iw44_render_summary(&mut summary, iw44_layers);
    print!("{summary}");
}

fn write_iw44_render_summary(output: &mut String, iw44_layers: &[djvulpes::RenderedIw44Layer]) {
    if iw44_layers.is_empty() {
        return;
    }

    for layer in iw44_layers {
        let role = match layer.role {
            djvulpes::Iw44LayerRole::Foreground => "foreground",
            djvulpes::Iw44LayerRole::Background => "background",
        };
        let painted = if layer.role == djvulpes::Iw44LayerRole::Background {
            "painted"
        } else {
            "painted-through-mask"
        };
        writeln!(
            output,
            "IW44 {role}: {painted} chunks={:?} image={}x{} subsample={} scaled={}x{} overscan={}x{}",
            layer.chunk_indices,
            layer.image.width,
            layer.image.height,
            layer.geometry.mapping.subsample,
            layer.geometry.mapping.scaled_width,
            layer.geometry.mapping.scaled_height,
            layer.geometry.mapping.horizontal_overscan,
            layer.geometry.mapping.vertical_overscan
        )
        .expect("writing to string should not fail");
    }
}

fn format_jb2_record_geometry(record: &djvulpes::Jb2RecordSummary) -> String {
    let mut detail = String::new();
    if let (Some(width), Some(height)) = (record.symbol_width, record.symbol_height) {
        let _ = write!(detail, " {width}x{height}");
    }
    if let (Some(x), Some(y)) = (record.x, record.y) {
        let _ = write!(detail, " at ({x}, {y})");
    }
    detail
}

fn format_page_chunk_source(source: PageChunkSource<'_>) -> String {
    match source {
        PageChunkSource::Page => "page".to_string(),
        PageChunkSource::Include { id, .. } => format!("include:{id}"),
    }
}

fn print_page_chunk_line(
    bytes: &[u8],
    page_chunk: &PageChunk<'_>,
    directory_context: Option<(&Document<'_>, &[DirectoryEntry<'_>])>,
) -> ParseResult<()> {
    let chunk = &page_chunk.chunk;

    if chunk.id == "FORM" {
        let form = parse_form_at(bytes, chunk.data_start - 8)?;
        println!(
            "    @{:<8} FORM:{:<4} {:<8} size={:<8} data=[{}..{}){}",
            chunk.data_start - 8,
            form.kind,
            page_chunk.kind.as_str(),
            form.chunk.size,
            form.chunk.data_start,
            form.chunk.data_end,
            format_page_chunk_payload(page_chunk, directory_context)
        );
    } else {
        println!(
            "    @{:<8} {:<4} {:<8} size={:<8} data=[{}..{}){}",
            chunk.data_start - 8,
            chunk.id,
            page_chunk.kind.as_str(),
            chunk.size,
            chunk.data_start,
            chunk.data_end,
            format_page_chunk_payload(page_chunk, directory_context)
        );
    }

    Ok(())
}

fn print_text_chunk(
    bytes: &[u8],
    page_chunk: &PageChunk<'_>,
    show_zones: bool,
) -> anyhow::Result<()> {
    let chunk = &page_chunk.chunk;
    let encoded = &bytes[chunk.data_start..chunk.data_end];
    let decoded;
    let payload = match page_chunk.kind {
        PageChunkKind::Txta => encoded,
        PageChunkKind::Txtz => {
            decoded = decode_bzz(encoded)?;
            &decoded
        }
        _ => return Ok(()),
    };
    let parsed = parse_text_payload(payload)?;

    println!();
    println!(
        "{}: @{} size={} decoded_bytes={} text_bytes={} zone_bytes={}",
        chunk.id,
        chunk.data_start - 8,
        chunk.size,
        payload.len(),
        parsed.text_len,
        parsed.zone_data.len()
    );
    println!();
    print!("{}", parsed.text);
    if !parsed.text.ends_with('\n') {
        println!();
    }

    if show_zones {
        println!();
        match parse_text_zones(parsed.zone_data)? {
            Some(root) => {
                println!("zones:");
                print_text_zone(&root, parsed.text, root.height, 0);
            }
            None => println!("zones: none"),
        }
    }

    Ok(())
}

fn print_text_zone(zone: &TextZone, text: &str, page_height: i32, depth: usize) {
    let indent = "  ".repeat(depth);
    println!(
        "{indent}{} bbox=({}, {}, {}, {}) text=[{}..{}){}",
        zone.kind.as_str(),
        zone.x_min(),
        zone.y_min(page_height),
        zone.x_max(),
        zone.y_max(page_height),
        zone.text_start,
        zone.text_end(),
        format_zone_text(zone, text)
    );

    for child in &zone.children {
        print_text_zone(child, text, page_height, depth + 1);
    }
}

fn format_zone_text(zone: &TextZone, text: &str) -> String {
    let Some(slice) = text.get(zone.text_start..zone.text_end()) else {
        return String::new();
    };
    if slice.is_empty() {
        return String::new();
    }

    let mut excerpt = String::new();
    for (index, character) in slice.chars().enumerate() {
        if index == 80 {
            excerpt.push_str("...");
            break;
        }
        excerpt.push(character);
    }

    let escaped = excerpt.escape_debug().to_string();
    format!(" \"{escaped}\"")
}

fn format_page_chunk_payload(
    page_chunk: &PageChunk<'_>,
    directory_context: Option<(&Document<'_>, &[DirectoryEntry<'_>])>,
) -> String {
    match page_chunk.payload {
        PageChunkPayload::Include { id } => format_include_payload(id, directory_context),
        PageChunkPayload::Raw => String::new(),
    }
}

fn format_include_payload(
    id: &str,
    directory_context: Option<(&Document<'_>, &[DirectoryEntry<'_>])>,
) -> String {
    let Some((document, entries)) = directory_context else {
        return format!(" id={id}");
    };
    let Some(entry) = entries.iter().find(|entry| entry.name == id) else {
        return format!(" id={id} -> unresolved");
    };
    let Some(form_index) = entry.form_index else {
        return format!(" id={id} -> @{} unresolved-form", entry.offset);
    };
    let form = &document.forms[form_index];

    format!(
        " id={id} -> @{} FORM:{} size={}",
        entry.offset, form.form.kind, form.form.chunk.size
    )
}

fn print_include_resolution(
    bytes: &[u8],
    document: &Document<'_>,
    entries: &[DirectoryEntry<'_>],
) -> ParseResult<()> {
    println!();
    println!("first page includes:");

    let mut printed = 0usize;
    for (page_index, page) in document.pages(bytes).enumerate() {
        let page = page?;
        for chunk in page.details(bytes)?.chunks {
            let PageChunkPayload::Include { id } = chunk.payload else {
                continue;
            };
            match entries.iter().find(|entry| entry.name == id) {
                Some(target) => println!(
                    "  page {:<4} {} -> @{} size={} flags=0x{:02x}",
                    page_index + 1,
                    id,
                    target.offset,
                    target.size,
                    target.flags
                ),
                None => println!("  page {:<4} {} -> unresolved", page_index + 1, id),
            }

            printed += 1;
            if printed == 12 {
                return Ok(());
            }
        }
    }

    if printed == 0 {
        println!("  none found");
    }

    Ok(())
}

const fn display_form_kind(kind: DocumentFormKind) -> &'static str {
    match kind {
        DocumentFormKind::Page => "page",
        DocumentFormKind::Shared => "shared",
        DocumentFormKind::Thumbnails => "thumbs",
        DocumentFormKind::Other => "other",
    }
}

fn display_chunk_role(id: &str, form_kind: Option<&str>) -> &'static str {
    match (id, form_kind) {
        ("FORM", Some("DJVU")) => "page",
        ("FORM", Some("DJVI")) => "shared",
        ("FORM", Some("THUM")) => "thumbs",
        ("FORM", _) => "form",
        ("DIRM", _) => "directory",
        ("NAVM", _) => "nav",
        ("INFO", _) => "info",
        ("INCL", _) => "include",
        ("Djbz", _) => "djbz",
        ("Sjbz", _) => "sjbz",
        ("FG44", _) => "fg44",
        ("BG44", _) => "bg44",
        ("TXTa", _) => "txta",
        ("TXTz", _) => "txtz",
        _ => "unknown",
    }
}

fn format_chunk_payload(bytes: &[u8], chunk: &Chunk<'_>) -> String {
    if chunk.id != "INCL" {
        return String::new();
    }

    std::str::from_utf8(&bytes[chunk.data_start..chunk.data_end])
        .map_or_else(|_| String::new(), |id| format!(" id={id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use djvulpes::ResolvedPageChunk;
    use djvulpes::{
        PdfPageImage, RenderCompareLimits, bitmap_diff_region_summary, write_page_image_pdf_iter,
    };

    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");

    fn bitonal_page_plan() -> PageRenderPlan<'static> {
        let mut plan = PageRenderPlan::new(
            djvulpes::PageInfo {
                width: 1560,
                height: 1633,
                version: 25,
                dpi: 200,
                gamma: 2.2,
                rotation: 1,
            },
            vec![ResolvedPageChunk {
                source: PageChunkSource::Page,
                chunk: PageChunk {
                    chunk: Chunk {
                        id: "Sjbz",
                        size: 0,
                        data_start: 0,
                        data_end: 0,
                        next_start: 0,
                    },
                    kind: PageChunkKind::Sjbz,
                    payload: PageChunkPayload::Raw,
                },
            }],
        );
        plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();
        plan
    }

    #[test]
    fn foreground_layer_mode_paints_bitonal_masks_black_without_iw44_foreground() {
        let plan = bitonal_page_plan();

        let render = plan
            .render_bitmap_with_mode(RYPKA_PAGE_1_SJBZ, PageRenderMode::Foreground)
            .expect("foreground contribution should render");
        let black_pixels = render
            .bitmap
            .pixels
            .chunks_exact(3)
            .filter(|pixel| **pixel == [0, 0, 0])
            .count();

        assert!(render.iw44_layers.is_empty());
        assert_eq!(render.bitonal_masks.len(), 1);
        assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
        assert_eq!(black_pixels, 167_493);
    }

    #[test]
    fn pdf_page_image_serializes_mixed_bitonal_and_iw44_renders() {
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
        let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
        let mut decoded_tail = Vec::new();
        let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
            decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
            parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
        });
        let pages = document
            .pages(RYPKA)
            .collect::<Result<Vec<_>, _>>()
            .expect("fixture pages should parse");
        let bitonal_page = pages.get(67).expect("fixture should have page 68");
        let iw44_page = pages.get(960).expect("fixture should have page 961");
        let bitonal_plan = document
            .page_render_plan(RYPKA, bitonal_page, &tail_entries)
            .expect("bitonal render plan should parse");
        let plan = document
            .page_render_plan(RYPKA, iw44_page, &tail_entries)
            .expect("IW44 render plan should parse");
        let bitonal_render = bitonal_plan
            .render_partial_bitmap(RYPKA)
            .expect("bitonal page should render");
        let render = plan
            .render_partial_bitmap(RYPKA)
            .expect("IW44 page should render");

        let bitonal_image = PdfPageImage::from_render(&bitonal_render);
        let page_image = PdfPageImage::from_render(&render);

        let PdfPageImage::BitonalMask {
            width,
            height,
            dpi,
            mask,
        } = &bitonal_image
        else {
            panic!("bitonal-only page should be embedded as a 1-bit image");
        };
        assert_eq!((*width, *height, *dpi), (3423, 5075, 600));
        assert_eq!(mask.len(), 428 * 5075);
        assert!(bitonal_render.iw44_layers.is_empty());

        let PdfPageImage::Rgb8 {
            width,
            height,
            dpi,
            pixels,
        } = &page_image
        else {
            panic!("IW44 page should be embedded as RGB");
        };
        assert_eq!((*width, *height, *dpi), (3486, 2783, 301));
        assert_eq!(pixels.len(), 3486 * 2783 * 3);
        assert!(!render.iw44_layers.is_empty());

        let pdf = write_page_image_pdf_iter(
            2,
            [
                Ok::<PdfPageImage, djvulpes::PdfError>(bitonal_image),
                Ok::<PdfPageImage, djvulpes::PdfError>(page_image),
            ],
        )
        .expect("PDF should serialize");
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.contains("/Type /Pages /Count 2"));
        assert!(text.contains(
            "/Subtype /Image /Width 3423 /Height 5075 /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"
        ));
        assert!(text.contains("/Subtype /Image /Width 3486 /Height 2783"));
        assert!(text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
    }

    #[test]
    fn run_render_pdf_writes_rypka_page_961_rgb_pdf() {
        let output =
            std::env::temp_dir().join(format!("djvulpes-page-961-{}.pdf", std::process::id()));

        run_render_pdf(
            Path::new("Rypka-HIL.djvu"),
            &output,
            961,
            Some(961),
            RenderPdfOptions {
                progress: RenderPdfProgress::Quiet,
                ..RenderPdfOptions::default()
            },
        )
        .expect("render-pdf should write page 961");
        let pdf = fs::read(&output).expect("rendered PDF should be readable");
        let _ = fs::remove_file(&output);
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Pages /Count 1"));
        assert!(text.contains("/Subtype /Image /Width 3486 /Height 2783"));
        assert!(text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
        assert!(!text.contains("/ImageMask true"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn run_render_pdf_writes_rypka_page_68_bitonal_mask_pdf() {
        let output =
            std::env::temp_dir().join(format!("djvulpes-page-68-{}.pdf", std::process::id()));

        run_render_pdf(
            Path::new("Rypka-HIL.djvu"),
            &output,
            68,
            Some(68),
            RenderPdfOptions {
                progress: RenderPdfProgress::Quiet,
                ..RenderPdfOptions::default()
            },
        )
        .expect("render-pdf should write page 68");
        let pdf = fs::read(&output).expect("rendered PDF should be readable");
        let _ = fs::remove_file(&output);
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Pages /Count 1"));
        assert!(text.contains(
            "/Subtype /Image /Width 3423 /Height 5075 /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"
        ));
        assert!(text.contains("/BitsPerComponent 1"));
        assert!(!text.contains("/ColorSpace /DeviceRGB"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn run_render_page_pdf_writes_rypka_page_68_bitonal_mask_pdf() {
        let output = std::env::temp_dir().join(format!(
            "djvulpes-single-page-68-{}.pdf",
            std::process::id()
        ));

        run_render_page_pdf(Path::new("Rypka-HIL.djvu"), 68, &output)
            .expect("render-page-pdf should write page 68");
        let pdf = fs::read(&output).expect("rendered PDF should be readable");
        let _ = fs::remove_file(&output);
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Pages /Count 1"));
        assert!(text.contains(
            "/Subtype /Image /Width 3423 /Height 5075 /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]"
        ));
        assert!(text.contains("/BitsPerComponent 1"));
        assert!(!text.contains("/ColorSpace /DeviceRGB"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn run_render_page_pdf_writes_rypka_page_961_rgb_pdf() {
        let output = std::env::temp_dir().join(format!(
            "djvulpes-single-page-961-{}.pdf",
            std::process::id()
        ));

        run_render_page_pdf(Path::new("Rypka-HIL.djvu"), 961, &output)
            .expect("render-page-pdf should write page 961");
        let pdf = fs::read(&output).expect("rendered PDF should be readable");
        let _ = fs::remove_file(&output);
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Pages /Count 1"));
        assert!(text.contains("/Subtype /Image /Width 3486 /Height 2783"));
        assert!(text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
        assert!(!text.contains("/ImageMask true"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn compare_render_page_layer_accepts_exact_rypka_page_961_background_oracle() {
        let oracle = std::env::temp_dir().join(format!(
            "djvulpes-page-961-background-{}.ppm",
            std::process::id()
        ));

        run_render_page_layer(
            Path::new("Rypka-HIL.djvu"),
            961,
            PageRenderMode::Background,
            &oracle,
        )
        .expect("background oracle should render");
        run_compare_render_page_layer(
            Path::new("Rypka-HIL.djvu"),
            961,
            PageRenderMode::Background,
            &oracle,
            RenderCompareLimits::new(0, 0, Some(0), 0.0),
        )
        .expect("render should match generated oracle exactly");
        let _ = fs::remove_file(&oracle);
    }

    #[test]
    fn compare_render_pages_accepts_exact_generated_oracle_directory() {
        let oracle_dir =
            std::env::temp_dir().join(format!("djvulpes-oracles-{}", std::process::id()));
        fs::create_dir_all(&oracle_dir).expect("oracle directory should be created");
        let oracle = oracle_dir.join("page-1.ppm");

        run_render_page(Path::new("Rypka-HIL.djvu"), 1, &oracle)
            .expect("page oracle should render");
        run_compare_render_pages(
            Path::new("Rypka-HIL.djvu"),
            &oracle_dir,
            PageRenderMode::Full,
            1,
            Some(1),
            RenderCompareLimits::new(0, 0, Some(0), 0.0),
        )
        .expect("render should match generated oracle exactly");
        let _ = fs::remove_file(&oracle);
        let _ = fs::remove_dir(&oracle_dir);
    }

    #[test]
    fn compare_ppm_accepts_exact_bitmap_files() {
        let actual_path =
            std::env::temp_dir().join(format!("djvulpes-actual-{}.ppm", std::process::id()));
        let expected_path =
            std::env::temp_dir().join(format!("djvulpes-expected-{}.ppm", std::process::id()));
        let bitmap = djvulpes::PageBitmap::new_rgb8(2, 1, 300, [0x11, 0x22, 0x33]);
        fs::write(&actual_path, bitmap.to_ppm_bytes()).expect("actual PPM should be written");
        fs::write(&expected_path, bitmap.to_ppm_bytes()).expect("expected PPM should be written");

        run_compare_ppm(
            &actual_path,
            &expected_path,
            RenderCompareLimits::new(0, 0, Some(0), 0.0),
        )
        .expect("identical PPM files should compare exactly");

        let _ = fs::remove_file(&actual_path);
        let _ = fs::remove_file(&expected_path);
    }

    #[test]
    fn dump_image_layers_writes_decoded_native_iw44_ppms() {
        let output_dir =
            std::env::temp_dir().join(format!("djvulpes-iw44-dump-{}", std::process::id()));

        run_dump_image_layers(Path::new("Rypka-HIL.djvu"), 1, &output_dir)
            .expect("image layer dump should write native IW44 PPMs");

        let background = fs::read(output_dir.join("page-1-background.ppm"))
            .expect("decoded background PPM should be written");
        let foreground = fs::read(output_dir.join("page-1-foreground.ppm"))
            .expect("decoded foreground PPM should be written");
        assert!(background.starts_with(b"P6\n780 817\n255\n"));
        assert!(foreground.starts_with(b"P6\n195 205\n255\n"));

        let _ = fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn bitmap_diff_region_summary_counts_local_difference_characteristics() {
        let mut actual = djvulpes::PageBitmap::new_rgb8(3, 2, 300, [0xff, 0xff, 0xff]);
        let mut expected = actual.clone();
        actual.set_rgb(1, 0, [0, 0, 0]);
        actual.set_rgb(2, 0, [0x10, 0x20, 0x30]);
        expected.set_rgb(2, 0, [0x10, 0x20, 0x31]);

        let summary = bitmap_diff_region_summary(
            &actual,
            &expected,
            djvulpes::PageBitmapDiffBounds {
                min_x: 1,
                min_y: 0,
                max_x: 2,
                max_y: 0,
            },
        )
        .expect("region should be in bounds");

        assert_eq!(
            summary,
            djvulpes::PageBitmapDiffRegionSummary {
                pixels: 2,
                differing_pixels: 2,
                total_abs_delta: 766,
                max_abs_delta: 255,
                max_delta_pixels: 1,
                actual_black_pixels: 1,
                expected_black_pixels: 0,
                actual_white_pixels: 0,
                expected_white_pixels: 1,
            }
        );
    }
}
