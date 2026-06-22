use anyhow::{Context, bail};
use djvulpes::{
    Bookmark, extract_document_bookmarks, extract_document_text, format_document_text_zones,
};
use djvulpes::{
    Chunk, DirectoryEntry, Document, DocumentFormKind, Form, PageChunk, PageChunkKind,
    PageChunkPayload, PageChunkSource, PageRenderMode, PageRenderPlan, ParseResult,
    PartialPageRender, PdfPageImage, TextZone, parse_chunks, parse_dirm_tail, parse_form_at,
    parse_text_payload, parse_text_zones, read_page_details,
    render_document_pdf_to_writer_with_events_and_timings,
};
use djvulpes::{DjvuPdfPageKind, DjvuPdfTimingEvent, DjvuPdfTimingStage};
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
    stages: Vec<(DjvuPdfTimingStage, Duration)>,
    pages: Vec<PdfPageTiming>,
    direct_bitonal_pages: usize,
    fallback_rgb_pages: usize,
}

impl PdfTimingSummary {
    fn record(&mut self, event: DjvuPdfTimingEvent) {
        *self.stage_mut(event.stage) += event.duration;
        if let Some(page_number) = event.page_number {
            let page = self.page_mut(page_number);
            page.total += event.duration;
            *page.stage_mut(event.stage) += event.duration;
            if let Some(page_kind) = event.page_kind {
                let first_kind = page.kind.is_none();
                page.kind = Some(page_kind);
                if first_kind {
                    match page_kind {
                        DjvuPdfPageKind::DirectBitonal => self.direct_bitonal_pages += 1,
                        DjvuPdfPageKind::FallbackRgb => self.fallback_rgb_pages += 1,
                    }
                }
            }
        }
    }

    fn stage_mut(&mut self, stage: DjvuPdfTimingStage) -> &mut Duration {
        if let Some(index) = self
            .stages
            .iter()
            .position(|(candidate, _)| *candidate == stage)
        {
            return &mut self.stages[index].1;
        }
        self.stages.push((stage, Duration::ZERO));
        &mut self.stages.last_mut().expect("stage was just pushed").1
    }

    fn page_mut(&mut self, page_number: usize) -> &mut PdfPageTiming {
        if let Some(index) = self
            .pages
            .iter()
            .position(|page| page.page_number == page_number)
        {
            return &mut self.pages[index];
        }
        self.pages.push(PdfPageTiming {
            page_number,
            ..PdfPageTiming::default()
        });
        self.pages.last_mut().expect("page was just pushed")
    }

    fn stage(&self, stage: DjvuPdfTimingStage) -> Duration {
        self.stages
            .iter()
            .find_map(|(candidate, duration)| (*candidate == stage).then_some(*duration))
            .unwrap_or(Duration::ZERO)
    }
}

impl std::fmt::Display for PdfTimingSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(formatter, "timings:")?;
        write_pdf_timing_stage_lines(formatter, self)?;
        writeln!(
            formatter,
            "  page kinds: {} direct bitonal, {} fallback RGB",
            self.direct_bitonal_pages, self.fallback_rgb_pages
        )?;
        write_pdf_timing_page_lines(formatter, self)?;

        Ok(())
    }
}

fn write_pdf_timing_stage_lines(
    formatter: &mut std::fmt::Formatter<'_>,
    summary: &PdfTimingSummary,
) -> std::fmt::Result {
    for (label, stage) in [
        ("setup", DjvuPdfTimingStage::Setup),
        ("text extraction", DjvuPdfTimingStage::TextExtraction),
        ("page planning", DjvuPdfTimingStage::PagePlan),
        ("direct bitonal", DjvuPdfTimingStage::DirectBitonal),
        ("fallback render", DjvuPdfTimingStage::FallbackRender),
        ("JB2 decode", DjvuPdfTimingStage::Jb2Decode),
        ("IW44 decode", DjvuPdfTimingStage::Iw44Decode),
        ("IW44 composite", DjvuPdfTimingStage::Iw44Composite),
        ("mask composite", DjvuPdfTimingStage::MaskComposite),
        ("image bytes", DjvuPdfTimingStage::ImageBytes),
        ("CCITT encode", DjvuPdfTimingStage::CcittEncode),
        ("PDF object write", DjvuPdfTimingStage::PdfObjectWrite),
        ("pdf write/encode", DjvuPdfTimingStage::PdfWrite),
    ] {
        writeln!(
            formatter,
            "  {label}: {}",
            format_duration(summary.stage(stage))
        )?;
    }

    Ok(())
}

fn write_pdf_timing_page_lines(
    formatter: &mut std::fmt::Formatter<'_>,
    summary: &PdfTimingSummary,
) -> std::fmt::Result {
    if summary.pages.is_empty() {
        return Ok(());
    }

    let page_count = summary.pages.len();
    let page_total: Duration = summary
        .pages
        .iter()
        .map(|page| page.stage(DjvuPdfTimingStage::PageTotal))
        .sum();
    writeln!(
        formatter,
        "  per-page average: {}",
        format_duration(page_total / u32::try_from(page_count).unwrap_or(u32::MAX))
    )?;
    writeln!(formatter, "  slowest pages:")?;
    let mut pages = summary.pages.clone();
    pages.sort_by_key(|page| std::cmp::Reverse(page.stage(DjvuPdfTimingStage::PageTotal)));
    for page in pages.iter().take(10) {
        writeln!(
            formatter,
            "    page {}: {} ({}; top: {})",
            page.page_number,
            format_duration(page.stage(DjvuPdfTimingStage::PageTotal)),
            page.kind.map_or("unknown", format_pdf_page_kind),
            page.dominant_stage().map_or_else(
                || "unknown".to_string(),
                |(stage, duration)| format!(
                    "{} {}",
                    format_pdf_timing_stage(stage),
                    format_duration(duration)
                )
            )
        )?;
    }

    Ok(())
}

#[derive(Debug, Clone, Default)]
struct PdfPageTiming {
    page_number: usize,
    kind: Option<DjvuPdfPageKind>,
    total: Duration,
    stages: Vec<(DjvuPdfTimingStage, Duration)>,
}

impl PdfPageTiming {
    fn stage_mut(&mut self, stage: DjvuPdfTimingStage) -> &mut Duration {
        if let Some(index) = self
            .stages
            .iter()
            .position(|(candidate, _)| *candidate == stage)
        {
            return &mut self.stages[index].1;
        }
        self.stages.push((stage, Duration::ZERO));
        &mut self.stages.last_mut().expect("stage was just pushed").1
    }

    fn stage(&self, stage: DjvuPdfTimingStage) -> Duration {
        self.stages
            .iter()
            .find_map(|(candidate, duration)| (*candidate == stage).then_some(*duration))
            .unwrap_or(Duration::ZERO)
    }

    fn dominant_stage(&self) -> Option<(DjvuPdfTimingStage, Duration)> {
        self.stages
            .iter()
            .filter(|(stage, _)| {
                !matches!(
                    stage,
                    DjvuPdfTimingStage::PageTotal
                        | DjvuPdfTimingStage::DirectBitonal
                        | DjvuPdfTimingStage::FallbackRender
                )
            })
            .max_by_key(|(_, duration)| *duration)
            .copied()
    }
}

const fn format_pdf_page_kind(kind: DjvuPdfPageKind) -> &'static str {
    match kind {
        DjvuPdfPageKind::DirectBitonal => "direct bitonal",
        DjvuPdfPageKind::FallbackRgb => "fallback RGB",
    }
}

const fn format_pdf_timing_stage(stage: DjvuPdfTimingStage) -> &'static str {
    match stage {
        DjvuPdfTimingStage::Setup => "setup",
        DjvuPdfTimingStage::TextExtraction => "text extraction",
        DjvuPdfTimingStage::PagePlan => "page planning",
        DjvuPdfTimingStage::DirectBitonal => "direct bitonal",
        DjvuPdfTimingStage::FallbackRender => "fallback render",
        DjvuPdfTimingStage::Jb2Decode => "JB2 decode",
        DjvuPdfTimingStage::Iw44Decode => "IW44 decode",
        DjvuPdfTimingStage::Iw44Composite => "IW44 composite",
        DjvuPdfTimingStage::MaskComposite => "mask composite",
        DjvuPdfTimingStage::ImageBytes => "image bytes",
        DjvuPdfTimingStage::CcittEncode => "CCITT encode",
        DjvuPdfTimingStage::PdfObjectWrite => "PDF object write",
        DjvuPdfTimingStage::PdfWrite => "pdf write/encode",
        DjvuPdfTimingStage::PageTotal => "page total",
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
        PdfPageImage::Gray8 {
            width,
            height,
            dpi,
            pixels,
        } => {
            writeln!(
                summary,
                "prepared: {width}x{height} dpi={dpi} format=PDF/DeviceGray8 bytes={}",
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
                print_text_zone(&root, parsed.text, 0);
            }
            None => println!("zones: none"),
        }
    }

    Ok(())
}

fn print_text_zone(zone: &TextZone, text: &str, depth: usize) {
    let indent = "  ".repeat(depth);
    println!(
        "{indent}{} bbox=({}, {}, {}, {}) text=[{}..{}){}",
        zone.kind.as_str(),
        zone.x_min(),
        zone.y_min(),
        zone.x_max(),
        zone.y_max(),
        zone.text_start,
        zone.text_end(),
        format_zone_text(zone, text)
    );

    for child in &zone.children {
        print_text_zone(child, text, depth + 1);
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
    use djvulpes::RenderCompareLimits;

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
}
