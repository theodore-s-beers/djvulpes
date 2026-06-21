use anyhow::{Context, bail};
use djvulpes::{
    Chunk, DirectoryEntry, Document, DocumentFormKind, Form, PageChunk, PageChunkKind,
    PageChunkPayload, PageChunkSource, PageRenderMode, PageRenderPlan, ParseResult,
    PartialPageRender, RenderCompareLimits, TextZone, bitmap_diff_failures,
    bitmap_diff_region_summary, bitmap_diff_tile_summaries, parse_chunks, parse_dirm_tail,
    parse_form_at, parse_text_payload, parse_text_zones, read_page_details,
    render_document_pdf_with_events,
};
use djvulpes::{decode_bzz, decode_dirm_tail};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const JB2_PLAN_PREFIX_RECORD_LIMIT: usize = 8;

#[derive(Debug, Default)]
struct BatchCompareWorst {
    different_pixels: usize,
    different_pixels_page: usize,
    abs_delta: u8,
    abs_delta_page: usize,
    mean_abs_delta: f64,
    mean_abs_delta_page: usize,
    observed: bool,
}

impl BatchCompareWorst {
    fn observe(&mut self, page_number: usize, diff: &djvulpes::PageBitmapDiff) {
        if !self.observed || diff.differing_pixels > self.different_pixels {
            self.different_pixels = diff.differing_pixels;
            self.different_pixels_page = page_number;
        }
        if !self.observed || diff.max_abs_delta > self.abs_delta {
            self.abs_delta = diff.max_abs_delta;
            self.abs_delta_page = page_number;
        }
        if !self.observed || diff.mean_abs_delta > self.mean_abs_delta {
            self.mean_abs_delta = diff.mean_abs_delta;
            self.mean_abs_delta_page = page_number;
        }
        self.observed = true;
    }

    fn print(&self) {
        println!(
            "worst different pixels: page {page} different={different}",
            page = self.different_pixels_page,
            different = self.different_pixels
        );
        println!(
            "worst max abs delta: page {page} max_abs_delta={delta}",
            page = self.abs_delta_page,
            delta = self.abs_delta
        );
        println!(
            "worst mean abs delta: page {page} mean_abs_delta={delta:.6}",
            page = self.mean_abs_delta_page,
            delta = self.mean_abs_delta
        );
    }
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
    let pdf = render_document_pdf_with_events(&bytes, number, Some(number), |event| {
        if let djvulpes::DjvuPdfRenderEvent::PageRendered { render, .. } = event {
            rendered = Some(render.clone());
        }
    })?;
    let render = rendered.context("render-page-pdf did not render the requested page")?;

    fs::write(output, pdf).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!(
        "rendered: {}x{} dpi={} format=PDF",
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
) -> anyhow::Result<()> {
    let bytes = read_file(path)?;
    let mut render_count = 0usize;
    let mut summaries = Vec::new();
    let pdf = render_document_pdf_with_events(&bytes, from_page, to_page, |event| match event {
        djvulpes::DjvuPdfRenderEvent::PageStarted {
            page_number,
            end_page,
        } => {
            eprintln!("rendering page {page_number} of {end_page}");
        }
        djvulpes::DjvuPdfRenderEvent::PageRendered {
            page_number,
            render,
        } => {
            eprintln!("rendered page {page_number}");
            render_count += 1;
            summaries.push(render_pdf_page_summary(page_number, render));
        }
    })?;

    fs::write(output, pdf).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("pages: {render_count}");
    println!(
        "range: {}..{}",
        from_page,
        to_page.map_or_else(|| "end".to_string(), |page| page.to_string())
    );
    println!("format: PDF");
    println!("output: {}", output.display());
    for summary in summaries {
        print!("{summary}");
    }

    Ok(())
}

pub fn run_compare_render_page(
    path: &Path,
    number: usize,
    oracle: &Path,
    limits: RenderCompareLimits,
) -> anyhow::Result<()> {
    let render = render_page(path, number)?;
    let oracle_bytes = read_file(oracle)?;
    let expected = djvulpes::PageBitmap::from_ppm_bytes(&oracle_bytes, render.bitmap.dpi)?;
    let diff = render.bitmap.diff(&expected)?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("oracle: {}", oracle.display());
    println!(
        "rendered: {}x{} dpi={} format=PPM/P6",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    );
    print_bitmap_comparison(&render.bitmap, &expected, &diff);
    print_partial_render_summary(&render.bitonal_masks);
    print_iw44_render_summary(&render.iw44_layers);
    enforce_bitmap_diff(&diff, limits)
}

pub fn run_compare_ppm(
    actual_path: &Path,
    expected_path: &Path,
    limits: RenderCompareLimits,
) -> anyhow::Result<()> {
    let actual_bytes = read_file(actual_path)?;
    let expected_bytes = read_file(expected_path)?;
    let actual = djvulpes::PageBitmap::from_ppm_bytes(&actual_bytes, 300)
        .with_context(|| format!("failed to read actual bitmap {}", actual_path.display()))?;
    let expected = djvulpes::PageBitmap::from_ppm_bytes(&expected_bytes, actual.dpi)
        .with_context(|| format!("failed to read expected bitmap {}", expected_path.display()))?;
    let diff = actual.diff(&expected)?;

    println!("actual: {}", actual_path.display());
    println!("expected: {}", expected_path.display());
    println!(
        "bitmap: {}x{} dpi={} format=PPM/P6",
        actual.width, actual.height, actual.dpi
    );
    print_bitmap_comparison(&actual, &expected, &diff);
    enforce_bitmap_diff(&diff, limits)
}

fn print_bitmap_comparison(
    actual: &djvulpes::PageBitmap,
    expected: &djvulpes::PageBitmap,
    diff: &djvulpes::PageBitmapDiff,
) {
    println!("actual:");
    print_bitmap_stats(actual);
    println!("expected:");
    print_bitmap_stats(expected);
    println!(
        "diff: pixels={} exact={} different={} total_abs_delta={} max_abs_delta={} max_delta_pixels={} mean_abs_delta={:.6}",
        diff.compared_pixels,
        diff.exact_pixels,
        diff.differing_pixels,
        diff.total_abs_delta,
        diff.max_abs_delta,
        diff.max_delta_pixels,
        diff.mean_abs_delta
    );
    println!(
        "diff channels: red total={} signed={} max={} mean={:.6} bias={:.6} green total={} signed={} max={} mean={:.6} bias={:.6} blue total={} signed={} max={} mean={:.6} bias={:.6}",
        diff.channels[0].total_abs_delta,
        diff.channels[0].signed_delta,
        diff.channels[0].max_abs_delta,
        diff.channels[0].mean_abs_delta,
        diff.channels[0].mean_signed_delta,
        diff.channels[1].total_abs_delta,
        diff.channels[1].signed_delta,
        diff.channels[1].max_abs_delta,
        diff.channels[1].mean_abs_delta,
        diff.channels[1].mean_signed_delta,
        diff.channels[2].total_abs_delta,
        diff.channels[2].signed_delta,
        diff.channels[2].max_abs_delta,
        diff.channels[2].mean_abs_delta,
        diff.channels[2].mean_signed_delta
    );
    if let Some(bounds) = diff.bounds {
        println!(
            "diff bounds: x={}..{} y={}..{} size={}x{}",
            bounds.min_x,
            bounds.max_x,
            bounds.min_y,
            bounds.max_y,
            bounds.width(),
            bounds.height()
        );
        print_bitmap_diff_region_summary(actual, expected, bounds);
        print_worst_bitmap_diff_tiles(actual, expected, 128, 128, 5);
    }
    if let Some(pixel) = diff.first_difference {
        print_diff_pixel("first diff", pixel);
    }
    if let Some(pixel) = diff.max_difference {
        print_diff_pixel("max diff", pixel);
    }
}

fn print_diff_pixel(label: &str, pixel: djvulpes::PageBitmapDiffPixel) {
    println!(
        "{}: x={} y={} actual=#{:02x}{:02x}{:02x} expected=#{:02x}{:02x}{:02x} abs_delta_sum={} max_abs_delta={}",
        label,
        pixel.x,
        pixel.y,
        pixel.actual[0],
        pixel.actual[1],
        pixel.actual[2],
        pixel.expected[0],
        pixel.expected[1],
        pixel.expected[2],
        pixel.abs_delta_sum,
        pixel.max_abs_delta
    );
}

fn print_bitmap_diff_region_summary(
    actual: &djvulpes::PageBitmap,
    expected: &djvulpes::PageBitmap,
    bounds: djvulpes::PageBitmapDiffBounds,
) {
    let Some(summary) = bitmap_diff_region_summary(actual, expected, bounds) else {
        return;
    };

    println!(
        "diff region: pixels={} different={} total_abs_delta={} max_abs_delta={} max_delta_pixels={} actual_black={} expected_black={} actual_white={} expected_white={}",
        summary.pixels,
        summary.differing_pixels,
        summary.total_abs_delta,
        summary.max_abs_delta,
        summary.max_delta_pixels,
        summary.actual_black_pixels,
        summary.expected_black_pixels,
        summary.actual_white_pixels,
        summary.expected_white_pixels
    );
}

fn print_worst_bitmap_diff_tiles(
    actual: &djvulpes::PageBitmap,
    expected: &djvulpes::PageBitmap,
    tile_width: u32,
    tile_height: u32,
    limit: usize,
) {
    let Some(mut tiles) = bitmap_diff_tile_summaries(actual, expected, tile_width, tile_height)
    else {
        return;
    };
    tiles.sort_by(|left, right| {
        right
            .summary
            .total_abs_delta
            .cmp(&left.summary.total_abs_delta)
            .then_with(|| {
                right
                    .summary
                    .differing_pixels
                    .cmp(&left.summary.differing_pixels)
            })
            .then_with(|| right.summary.max_abs_delta.cmp(&left.summary.max_abs_delta))
    });

    for (index, tile) in tiles.into_iter().take(limit).enumerate() {
        println!(
            "diff tile #{}: x={}..{} y={}..{} size={}x{} different={} total_abs_delta={} max_abs_delta={} mean_abs_delta={}",
            index + 1,
            tile.bounds.min_x,
            tile.bounds.max_x,
            tile.bounds.min_y,
            tile.bounds.max_y,
            tile.bounds.width(),
            tile.bounds.height(),
            tile.summary.differing_pixels,
            tile.summary.total_abs_delta,
            tile.summary.max_abs_delta,
            format_milli_delta(tile.summary.total_abs_delta, tile.summary.pixels * 3)
        );
    }
}

fn format_milli_delta(total_abs_delta: u64, component_count: usize) -> String {
    if component_count == 0 {
        return "0.000".to_string();
    }

    let milli = (u128::from(total_abs_delta) * 1_000)
        / u128::try_from(component_count).unwrap_or(u128::MAX);
    format!("{}.{:03}", milli / 1_000, milli % 1_000)
}

fn enforce_bitmap_diff(
    diff: &djvulpes::PageBitmapDiff,
    limits: RenderCompareLimits,
) -> anyhow::Result<()> {
    if let Some(failure) = bitmap_diff_failures(diff, limits).into_iter().next() {
        bail!("{failure}");
    }

    Ok(())
}

pub fn run_compare_render_pages(
    path: &Path,
    oracle_dir: &Path,
    mode: PageRenderMode,
    from_page: usize,
    to_page: Option<usize>,
    limits: RenderCompareLimits,
) -> anyhow::Result<()> {
    let bytes = read_file(path)?;

    println!("file: {}", path.display());
    println!("oracle dir: {}", oracle_dir.display());
    println!("mode: {}", mode.as_str());
    println!(
        "range: {}..{}",
        from_page,
        to_page.map_or_else(|| "end".to_string(), |page| page.to_string())
    );
    println!(
        "limits: max_different_pixels={} max_abs_delta={} max_delta_pixels={} max_mean_abs_delta={:.6}",
        limits.different_pixels,
        limits.abs_delta,
        limits
            .delta_pixels
            .map_or_else(|| "unlimited".to_string(), |value| value.to_string()),
        limits.mean_abs_delta
    );

    let mut checked_pages = 0usize;
    let mut failed_pages = 0usize;
    let mut worst = BatchCompareWorst::default();
    let renders =
        djvulpes::render_document_pages_with_events(&bytes, from_page, to_page, mode, |_| {})
            .context("failed to render page range")?;
    for rendered_page in renders {
        let page_number = rendered_page.page_number;
        let render = rendered_page.render;
        let oracle = oracle_dir.join(format!("page-{page_number}.ppm"));
        let oracle_bytes = read_file(&oracle)?;
        let expected = djvulpes::PageBitmap::from_ppm_bytes(&oracle_bytes, render.bitmap.dpi)
            .with_context(|| format!("failed to read oracle {}", oracle.display()))?;
        let diff = render
            .bitmap
            .diff(&expected)
            .with_context(|| format!("failed to compare page {page_number}"))?;
        let failures = bitmap_diff_failures(&diff, limits);
        checked_pages += 1;
        worst.observe(page_number, &diff);
        if failures.is_empty() {
            println!(
                "page {page_number}: ok different={} max_abs_delta={} max_delta_pixels={} mean_abs_delta={:.6}",
                diff.differing_pixels,
                diff.max_abs_delta,
                diff.max_delta_pixels,
                diff.mean_abs_delta
            );
        } else {
            failed_pages += 1;
            println!(
                "page {page_number}: fail different={} max_abs_delta={} max_delta_pixels={} mean_abs_delta={:.6}",
                diff.differing_pixels,
                diff.max_abs_delta,
                diff.max_delta_pixels,
                diff.mean_abs_delta
            );
            for failure in failures {
                println!("page {page_number}: {failure}");
            }
            if let Some(pixel) = diff.first_difference {
                print_diff_pixel("first diff", pixel);
            }
            if let Some(pixel) = diff.max_difference {
                print_diff_pixel("max diff", pixel);
            }
        }
    }

    println!("checked pages: {checked_pages}");
    println!("failed pages: {failed_pages}");
    worst.print();
    if failed_pages > 0 {
        bail!("{failed_pages} rendered pages exceeded comparison limits");
    }

    Ok(())
}

pub fn run_compare_render_page_layer(
    path: &Path,
    number: usize,
    mode: PageRenderMode,
    oracle: &Path,
    limits: RenderCompareLimits,
) -> anyhow::Result<()> {
    let render = render_page_layer(path, number, mode)?;
    let oracle_bytes = read_file(oracle)?;
    let expected = djvulpes::PageBitmap::from_ppm_bytes(&oracle_bytes, render.bitmap.dpi)?;
    let diff = render.bitmap.diff(&expected)?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("mode: {}", mode.as_str());
    println!("oracle: {}", oracle.display());
    println!(
        "rendered: {}x{} dpi={} format=PPM/P6",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    );
    print_bitmap_comparison(&render.bitmap, &expected, &diff);
    print_partial_render_summary(&render.bitonal_masks);
    print_iw44_render_summary(&render.iw44_layers);
    enforce_bitmap_diff(&diff, limits)
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

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44PixelInspection {
    role: djvulpes::Iw44LayerRole,
    page_x: u32,
    page_y: u32,
    source_x: usize,
    source_y: usize,
    rgb: [u8; 3],
    computed_rgb: [u8; 3],
    y: Iw44PlaneSample,
    cb: Option<Iw44PlaneSample>,
    cr: Option<Iw44PlaneSample>,
    y_neighborhood: Option<Iw44LumaNeighborhood>,
    y_coefficients: Option<Iw44CoefficientBlockSummary>,
    y_coefficient_trace: Option<Vec<Iw44CoefficientTraceStep>>,
    y_coefficient_event_traces: Option<Vec<Iw44CoefficientEventTrace>>,
    y_coefficient_reconstruction: Option<Iw44CoefficientReconstructionSummary>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44PlaneSample {
    plane: djvulpes::Iw44Plane,
    x: usize,
    y: usize,
    raw: i16,
    normalized: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44LumaNeighborhood {
    page_min_x: u32,
    page_min_y: u32,
    source_min_x: usize,
    source_min_y: usize,
    samples: Vec<Vec<Iw44PlaneSample>>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientBlockSummary {
    plane: djvulpes::Iw44Plane,
    plane_width: usize,
    block_x: usize,
    block_y: usize,
    block_col: usize,
    block_row: usize,
    width: usize,
    height: usize,
    non_zero: usize,
    max_abs: u16,
    abs_sum: u64,
    entries: Vec<Iw44CoefficientEntry>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientEntry {
    x: usize,
    y: usize,
    local_x: usize,
    local_y: usize,
    index: usize,
    bucket: usize,
    bucket_offset: usize,
    band: usize,
    value: i16,
    absolute: u16,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientTraceStep {
    chunk_index: usize,
    chunk_serial: u8,
    slice_index: Option<u32>,
    slices_decoded: u32,
    values: Vec<Iw44CoefficientEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientEventTrace {
    entry: Iw44CoefficientEntry,
    events: Vec<Iw44CoefficientTraceEvent>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientTraceEvent {
    chunk_index: usize,
    chunk_serial: u8,
    event: djvulpes::Iw44CoefficientEvent,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientReconstructionSummary {
    entries: Vec<Iw44CoefficientReconstructionTrace>,
    all_zeroed: Iw44CoefficientReconstructionAggregate,
    block_zeroed: Iw44CoefficientReconstructionAggregate,
    rows_then_columns: Iw44TransformOrderTrace,
    padded_extent: Iw44TransformOrderTrace,
    band_zeroed: Vec<Iw44CoefficientReconstructionBandAggregate>,
    bucket_zeroed: Vec<Iw44CoefficientReconstructionBucketAggregate>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44TransformOrderTrace {
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    alternate_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    alternate_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientReconstructionTrace {
    entry: Iw44CoefficientEntry,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientReconstructionAggregate {
    coefficient_count: usize,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientReconstructionBandAggregate {
    band: usize,
    coefficient_count: usize,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientReconstructionBucketAggregate {
    bucket: usize,
    band: usize,
    coefficient_count: usize,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
    contributors: Vec<Iw44CoefficientEntry>,
}

struct Iw44OptionalPixelDiagnostics {
    neighborhood: Option<Iw44LumaNeighborhood>,
    coefficients: Option<Iw44CoefficientBlockSummary>,
    coefficient_trace: Option<Vec<Iw44CoefficientTraceStep>>,
    coefficient_event_traces: Option<Vec<Iw44CoefficientEventTrace>>,
    coefficient_reconstruction: Option<Iw44CoefficientReconstructionSummary>,
}

struct Iw44InspectionLayer<'a> {
    role: djvulpes::Iw44LayerRole,
    payloads: Vec<djvulpes::RenderChunkPayload<'a>>,
    geometry: djvulpes::Iw44LayerGeometry,
}

fn inspect_iw44_pixel(
    plan: &PageRenderPlan<'_>,
    bytes: &[u8],
    mode: PageRenderMode,
    options: &Iw44PixelInspectOptions,
) -> anyhow::Result<Iw44PixelInspection> {
    if options.radius > 10 {
        bail!("IW44 inspection radius must be 10 or less");
    }
    let layer = iw44_inspection_layer(plan, bytes, mode)?;
    if options.x >= layer.geometry.mapping.page_width
        || options.y >= layer.geometry.mapping.page_height
    {
        bail!(
            "page pixel x={} y={} is outside page {}x{}",
            options.x,
            options.y,
            layer.geometry.mapping.page_width,
            layer.geometry.mapping.page_height
        );
    }

    let decoder = decode_iw44_payloads(&layer.payloads)?;
    let image = decoder.to_rgb_image()?;
    let planes = decoder.reconstruct_planes();
    let source_x = iw44_source_coordinate(
        options.x,
        layer.geometry.mapping.horizontal_overscan,
        layer.geometry.mapping.subsample,
        image.width,
    );
    let source_y = iw44_source_coordinate(
        options.y,
        layer.geometry.mapping.vertical_overscan,
        layer.geometry.mapping.subsample,
        image.height,
    );
    let rgb_offset = (source_y * image.width + source_x) * 3;
    let rgb = [
        image.pixels[rgb_offset],
        image.pixels[rgb_offset + 1],
        image.pixels[rgb_offset + 2],
    ];
    let image_header = decoder
        .image()
        .context("IW44 decoder did not produce image metadata")?;
    let y_sample = iw44_plane_sample(&planes, djvulpes::Iw44Plane::Y, source_x, source_y, false)?;
    let blue_chroma_sample = if image_header.grayscale {
        None
    } else {
        Some(iw44_plane_sample(
            &planes,
            djvulpes::Iw44Plane::Cb,
            source_x,
            source_y,
            image_header.chroma_half,
        )?)
    };
    let red_chroma_sample = if image_header.grayscale {
        None
    } else {
        Some(iw44_plane_sample(
            &planes,
            djvulpes::Iw44Plane::Cr,
            source_x,
            source_y,
            image_header.chroma_half,
        )?)
    };
    let computed_rgb = iw44_samples_to_rgb(y_sample, blue_chroma_sample, red_chroma_sample);
    let diagnostics =
        iw44_optional_pixel_diagnostics(&layer, &decoder, &planes, &image, y_sample, options)?;

    Ok(Iw44PixelInspection {
        role: layer.role,
        page_x: options.x,
        page_y: options.y,
        source_x,
        source_y,
        rgb,
        computed_rgb,
        y: y_sample,
        cb: blue_chroma_sample,
        cr: red_chroma_sample,
        y_neighborhood: diagnostics.neighborhood,
        y_coefficients: diagnostics.coefficients,
        y_coefficient_trace: diagnostics.coefficient_trace,
        y_coefficient_event_traces: diagnostics.coefficient_event_traces,
        y_coefficient_reconstruction: diagnostics.coefficient_reconstruction,
    })
}

fn iw44_optional_pixel_diagnostics(
    layer: &Iw44InspectionLayer<'_>,
    decoder: &djvulpes::Iw44Decoder,
    planes: &[djvulpes::Iw44ReconstructionPlane],
    image: &djvulpes::Iw44RgbImage,
    y_sample: Iw44PlaneSample,
    options: &Iw44PixelInspectOptions,
) -> anyhow::Result<Iw44OptionalPixelDiagnostics> {
    let y_neighborhood = if options.radius == 0 {
        None
    } else {
        Some(iw44_luma_neighborhood(
            planes,
            &layer.geometry.mapping,
            image.width,
            image.height,
            options.x,
            options.y,
            options.radius,
        )?)
    };
    let y_coefficients = if options.coefficient_limit == 0 && options.coefficient_indices.is_empty()
    {
        None
    } else {
        Some(iw44_coefficient_block_summary(
            decoder,
            y_sample,
            options.coefficient_limit,
            &options.coefficient_indices,
        )?)
    };
    let y_coefficient_trace = if options.traces.contains(&Iw44PixelTrace::Coefficients) {
        let Some(coefficients) = &y_coefficients else {
            bail!("--trace-coefficients requires --coefficients or --coefficient-index");
        };
        Some(iw44_coefficient_trace(
            &layer.payloads,
            coefficients.plane,
            &coefficients.entries,
            options.traces.contains(&Iw44PixelTrace::Slices),
        )?)
    } else {
        None
    };
    let y_coefficient_event_traces = if options.traces.contains(&Iw44PixelTrace::Events) {
        let Some(coefficients) = &y_coefficients else {
            bail!("--trace-events requires --coefficients or --coefficient-index");
        };
        Some(iw44_coefficient_event_traces(
            &layer.payloads,
            coefficients.plane,
            coefficients.plane_width,
            coefficients.block_col,
            coefficients.block_row,
            &coefficients.entries,
        )?)
    } else {
        None
    };
    let y_coefficient_reconstruction = if options.traces.contains(&Iw44PixelTrace::Reconstruction) {
        let Some(coefficients) = &y_coefficients else {
            bail!("--trace-reconstruction requires --coefficients or --coefficient-index");
        };
        Some(iw44_coefficient_reconstruction_trace(
            decoder,
            coefficients,
            y_sample,
        )?)
    } else {
        None
    };

    Ok(Iw44OptionalPixelDiagnostics {
        neighborhood: y_neighborhood,
        coefficients: y_coefficients,
        coefficient_trace: y_coefficient_trace,
        coefficient_event_traces: y_coefficient_event_traces,
        coefficient_reconstruction: y_coefficient_reconstruction,
    })
}

fn iw44_inspection_layer<'a>(
    plan: &PageRenderPlan<'_>,
    bytes: &'a [u8],
    mode: PageRenderMode,
) -> anyhow::Result<Iw44InspectionLayer<'a>> {
    let (role, payloads, geometry) = match mode {
        PageRenderMode::Background => (
            djvulpes::Iw44LayerRole::Background,
            plan.background_layer_payloads(bytes),
            plan.background_layer_geometry(bytes)?,
        ),
        PageRenderMode::Foreground => (
            djvulpes::Iw44LayerRole::Foreground,
            plan.foreground_layer_payloads(bytes),
            plan.foreground_layer_geometry(bytes)?,
        ),
        PageRenderMode::Full | PageRenderMode::Mask => {
            bail!("IW44 pixel inspection requires background or foreground mode")
        }
    };
    let geometry = geometry.with_context(|| format!("{} IW44 layer not found", mode.as_str()))?;

    Ok(Iw44InspectionLayer {
        role,
        payloads,
        geometry,
    })
}

fn decode_iw44_payloads(
    payloads: &[djvulpes::RenderChunkPayload<'_>],
) -> anyhow::Result<djvulpes::Iw44Decoder> {
    let mut decoder = djvulpes::Iw44Decoder::new();
    for payload in payloads {
        decoder
            .decode_chunk(payload.bytes)
            .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
    }

    Ok(decoder)
}

fn iw44_source_coordinate(
    page_coordinate: u32,
    overscan: u32,
    subsample: u32,
    source_extent: usize,
) -> usize {
    let centered = page_coordinate.saturating_add(overscan / 2);
    let scaled = centered / subsample.max(1);
    (scaled as usize).min(source_extent.saturating_sub(1))
}

fn iw44_plane_sample(
    planes: &[djvulpes::Iw44ReconstructionPlane],
    plane: djvulpes::Iw44Plane,
    source_x: usize,
    source_y: usize,
    chroma_half: bool,
) -> anyhow::Result<Iw44PlaneSample> {
    let plane_data = planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} reconstruction plane", iw44_plane_name(plane)))?;
    let sample_y = plane_data.height - 1 - source_y.min(plane_data.height - 1);
    let sample_x = source_x.min(plane_data.width - 1);
    let (sample_x, sample_y) = if chroma_half {
        (sample_x / 2, sample_y / 2)
    } else {
        (sample_x, sample_y)
    };
    let raw = plane_data.samples[sample_y * plane_data.width + sample_x];

    Ok(Iw44PlaneSample {
        plane,
        x: sample_x,
        y: sample_y,
        raw,
        normalized: normalize_iw44_sample(raw),
    })
}

fn iw44_luma_neighborhood(
    planes: &[djvulpes::Iw44ReconstructionPlane],
    mapping: &djvulpes::Iw44PageMapping,
    image_width: usize,
    image_height: usize,
    x: u32,
    y: u32,
    radius: u8,
) -> anyhow::Result<Iw44LumaNeighborhood> {
    let radius = u32::from(radius);
    let min_page_x = x.saturating_sub(radius);
    let min_page_y = y.saturating_sub(radius);
    let max_page_x = x.saturating_add(radius).min(mapping.page_width - 1);
    let max_page_y = y.saturating_add(radius).min(mapping.page_height - 1);
    let source_min_x = iw44_source_coordinate(
        min_page_x,
        mapping.horizontal_overscan,
        mapping.subsample,
        image_width,
    );
    let source_min_y = iw44_source_coordinate(
        min_page_y,
        mapping.vertical_overscan,
        mapping.subsample,
        image_height,
    );
    let mut rows = Vec::new();

    for page_y in min_page_y..=max_page_y {
        let source_y = iw44_source_coordinate(
            page_y,
            mapping.vertical_overscan,
            mapping.subsample,
            image_height,
        );
        let mut row = Vec::new();
        for page_x in min_page_x..=max_page_x {
            let source_x = iw44_source_coordinate(
                page_x,
                mapping.horizontal_overscan,
                mapping.subsample,
                image_width,
            );
            row.push(iw44_plane_sample(
                planes,
                djvulpes::Iw44Plane::Y,
                source_x,
                source_y,
                false,
            )?);
        }
        rows.push(row);
    }

    Ok(Iw44LumaNeighborhood {
        page_min_x: min_page_x,
        page_min_y: min_page_y,
        source_min_x,
        source_min_y,
        samples: rows,
    })
}

fn iw44_coefficient_block_summary(
    decoder: &djvulpes::Iw44Decoder,
    sample: Iw44PlaneSample,
    entry_limit: usize,
    selected_indices: &[usize],
) -> anyhow::Result<Iw44CoefficientBlockSummary> {
    let coefficient_planes = decoder.coefficient_planes();
    let plane = coefficient_planes
        .iter()
        .find(|plane| plane.plane == sample.plane)
        .with_context(|| {
            format!(
                "missing {} coefficient plane",
                iw44_plane_name(sample.plane)
            )
        })?;
    let block_x = (sample.x / 32) * 32;
    let block_y = (sample.y / 32) * 32;
    let width = 32.min(plane.width - block_x);
    let height = 32.min(plane.height - block_y);
    let mut entries = Vec::new();
    let mut non_zero = 0usize;
    let mut max_abs = 0u16;
    let mut abs_sum = 0u64;

    for y in block_y..block_y + height {
        for x in block_x..block_x + width {
            let value = plane.coefficients[y * plane.width + x];
            let absolute = value.unsigned_abs();
            if absolute == 0 {
                continue;
            }

            non_zero += 1;
            max_abs = max_abs.max(absolute);
            abs_sum += u64::from(absolute);
            entries.push(iw44_coefficient_entry(block_x, block_y, x, y, value));
        }
    }

    entries.sort_by(|left, right| {
        right
            .absolute
            .cmp(&left.absolute)
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.x.cmp(&right.x))
    });
    entries.truncate(entry_limit);
    for &index in selected_indices {
        if index >= 1024 {
            bail!("IW44 coefficient index {index} is outside a 32x32 block");
        }
        if entries.iter().any(|entry| entry.index == index) {
            continue;
        }
        let x = block_x + iw44_zigzag_col(index);
        let y = block_y + iw44_zigzag_row(index);
        if x >= plane.width || y >= plane.height {
            bail!("IW44 coefficient index {index} is outside the edge block");
        }
        let value = plane.coefficients[y * plane.width + x];
        entries.push(iw44_coefficient_entry(block_x, block_y, x, y, value));
    }

    Ok(Iw44CoefficientBlockSummary {
        plane: sample.plane,
        plane_width: plane.width,
        block_x,
        block_y,
        block_col: block_x / 32,
        block_row: block_y / 32,
        width,
        height,
        non_zero,
        max_abs,
        abs_sum,
        entries,
    })
}

fn iw44_coefficient_entry(
    block_x: usize,
    block_y: usize,
    x: usize,
    y: usize,
    value: i16,
) -> Iw44CoefficientEntry {
    let local_x = x - block_x;
    let local_y = y - block_y;
    let index = iw44_inverse_zigzag_index(local_x, local_y);
    let bucket = index / 16;
    let bucket_offset = index % 16;

    Iw44CoefficientEntry {
        x,
        y,
        local_x,
        local_y,
        index,
        bucket,
        bucket_offset,
        band: iw44_bucket_band(bucket),
        value,
        absolute: value.unsigned_abs(),
    }
}

fn iw44_inverse_zigzag_index(local_x: usize, local_y: usize) -> usize {
    let mut index = 0usize;
    for bit in 0..5 {
        index |= ((local_x >> (4 - bit)) & 1) << (bit * 2);
        index |= ((local_y >> (4 - bit)) & 1) << (bit * 2 + 1);
    }
    index
}

fn iw44_zigzag_col(index: usize) -> usize {
    let mut col = 0usize;
    for bit in 0..5 {
        col |= ((index >> (bit * 2)) & 1) << (4 - bit);
    }
    col
}

fn iw44_zigzag_row(index: usize) -> usize {
    let mut row = 0usize;
    for bit in 0..5 {
        row |= ((index >> ((bit * 2) + 1)) & 1) << (4 - bit);
    }
    row
}

fn iw44_coefficient_value(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    x: usize,
    y: usize,
) -> anyhow::Result<i16> {
    let coefficient_planes = decoder.coefficient_planes();
    let plane_data = coefficient_planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} coefficient plane", iw44_plane_name(plane)))?;

    Ok(plane_data.coefficients[y * plane_data.width + x])
}

const fn iw44_bucket_band(bucket: usize) -> usize {
    match bucket {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=7 => 4,
        8..=11 => 5,
        12..=15 => 6,
        16..=31 => 7,
        32..=47 => 8,
        48..=63 => 9,
        _ => 10,
    }
}

fn iw44_coefficient_trace(
    payloads: &[djvulpes::RenderChunkPayload<'_>],
    plane: djvulpes::Iw44Plane,
    entries: &[Iw44CoefficientEntry],
    trace_slices: bool,
) -> anyhow::Result<Vec<Iw44CoefficientTraceStep>> {
    let mut decoder = djvulpes::Iw44Decoder::new();
    let mut trace = Vec::with_capacity(payloads.len());

    for payload in payloads {
        if trace_slices {
            let header = djvulpes::read_iw44_chunk_header(payload.bytes)
                .with_context(|| format!("failed to read IW44 chunk #{} header", payload.index))?;
            let mut trace_error = None;
            decoder
                .decode_chunk_with_slice_observer(payload.bytes, |decoder, slice| {
                    match iw44_trace_values(decoder, plane, entries) {
                        Ok(values) => trace.push(Iw44CoefficientTraceStep {
                            chunk_index: payload.index,
                            chunk_serial: header.serial,
                            slice_index: Some(slice.index),
                            slices_decoded: decoder.slices_decoded(),
                            values,
                        }),
                        Err(error) => trace_error = Some(error),
                    }
                })
                .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
            if let Some(error) = trace_error {
                return Err(error);
            }
        } else {
            let chunk = decoder
                .decode_chunk(payload.bytes)
                .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
            trace.push(Iw44CoefficientTraceStep {
                chunk_index: payload.index,
                chunk_serial: chunk.header.serial,
                slice_index: None,
                slices_decoded: decoder.slices_decoded(),
                values: iw44_trace_values(&decoder, plane, entries)?,
            });
        }
    }

    Ok(trace)
}

fn iw44_coefficient_event_traces(
    payloads: &[djvulpes::RenderChunkPayload<'_>],
    plane: djvulpes::Iw44Plane,
    plane_width: usize,
    block_col: usize,
    block_row: usize,
    entries: &[Iw44CoefficientEntry],
) -> anyhow::Result<Vec<Iw44CoefficientEventTrace>> {
    let block_columns = plane_width.div_ceil(32);
    let block = block_row * block_columns + block_col;
    let mut traces = Vec::with_capacity(entries.len());

    for entry in entries {
        let mut decoder = djvulpes::Iw44Decoder::new();
        let mut events = Vec::new();
        let target = djvulpes::Iw44CoefficientTraceTarget {
            plane,
            block,
            coefficient: entry.index,
        };

        for payload in payloads {
            let header = djvulpes::read_iw44_chunk_header(payload.bytes)
                .with_context(|| format!("failed to read IW44 chunk #{} header", payload.index))?;
            decoder
                .decode_chunk_with_coefficient_observer(payload.bytes, target, |event| {
                    events.push(Iw44CoefficientTraceEvent {
                        chunk_index: payload.index,
                        chunk_serial: header.serial,
                        event,
                    });
                })
                .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
        }

        traces.push(Iw44CoefficientEventTrace {
            entry: *entry,
            events,
        });
    }

    Ok(traces)
}

fn iw44_coefficient_reconstruction_trace(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Iw44CoefficientReconstructionSummary> {
    let block_columns = coefficients.plane_width.div_ceil(32);
    let block = coefficients.block_row * block_columns + coefficients.block_col;
    let overrides = coefficients
        .entries
        .iter()
        .map(|entry| (block, entry.index, 0))
        .collect::<Vec<_>>();
    let block_overrides = (0..1024)
        .map(|coefficient| (block, coefficient, 0))
        .collect::<Vec<_>>();
    let entries =
        iw44_individual_coefficient_reconstruction_traces(decoder, coefficients, block, sample)?;
    let band_zeroed = iw44_band_zeroed_reconstruction(decoder, coefficients, block, sample)?;
    let bucket_zeroed =
        iw44_bucket_zeroed_reconstruction(decoder, coefficients, block, sample, &band_zeroed)?;

    Ok(Iw44CoefficientReconstructionSummary {
        entries,
        all_zeroed: iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &overrides,
            coefficients.entries.len(),
            sample,
            "listed coefficients",
        )?,
        block_zeroed: iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &block_overrides,
            1024,
            sample,
            "containing coefficient block",
        )?,
        rows_then_columns: iw44_transform_order_trace(decoder, coefficients.plane, sample)?,
        padded_extent: iw44_padded_extent_trace(decoder, coefficients.plane, sample)?,
        band_zeroed,
        bucket_zeroed,
    })
}

fn iw44_transform_order_trace(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Iw44TransformOrderTrace> {
    let planes =
        decoder.reconstruct_planes_with_order(djvulpes::Iw44ReconstructionOrder::RowsThenColumns);
    let alternate = planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} reconstruction plane", iw44_plane_name(plane)))?;
    let alternate_raw = alternate.samples[sample.y * alternate.width + sample.x];
    let alternate_normalized = normalize_iw44_sample(alternate_raw);

    Ok(Iw44TransformOrderTrace {
        sample_x: sample.x,
        sample_y: sample.y,
        original_raw: sample.raw,
        alternate_raw,
        raw_delta: i32::from(sample.raw) - i32::from(alternate_raw),
        original_normalized: sample.normalized,
        alternate_normalized,
        normalized_delta: sample.normalized - alternate_normalized,
    })
}

fn iw44_padded_extent_trace(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Iw44TransformOrderTrace> {
    let planes = decoder.reconstruct_planes_with_options(
        djvulpes::Iw44ReconstructionOrder::ColumnsThenRows,
        djvulpes::Iw44ReconstructionExtent::Padded,
    );
    let alternate = planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} reconstruction plane", iw44_plane_name(plane)))?;
    let alternate_raw = alternate.samples[sample.y * alternate.width + sample.x];
    let alternate_normalized = normalize_iw44_sample(alternate_raw);

    Ok(Iw44TransformOrderTrace {
        sample_x: sample.x,
        sample_y: sample.y,
        original_raw: sample.raw,
        alternate_raw,
        raw_delta: i32::from(sample.raw) - i32::from(alternate_raw),
        original_normalized: sample.normalized,
        alternate_normalized,
        normalized_delta: sample.normalized - alternate_normalized,
    })
}

fn iw44_individual_coefficient_reconstruction_traces(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    block: usize,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Vec<Iw44CoefficientReconstructionTrace>> {
    let mut traces = Vec::with_capacity(coefficients.entries.len());
    for entry in &coefficients.entries {
        let aggregate = iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &[(block, entry.index, 0)],
            1,
            sample,
            "coefficient",
        )?;
        traces.push(Iw44CoefficientReconstructionTrace {
            entry: *entry,
            sample_x: aggregate.sample_x,
            sample_y: aggregate.sample_y,
            original_raw: aggregate.original_raw,
            zeroed_raw: aggregate.zeroed_raw,
            raw_delta: aggregate.raw_delta,
            original_normalized: aggregate.original_normalized,
            zeroed_normalized: aggregate.zeroed_normalized,
            normalized_delta: aggregate.normalized_delta,
        });
    }

    Ok(traces)
}

fn iw44_band_zeroed_reconstruction(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    block: usize,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Vec<Iw44CoefficientReconstructionBandAggregate>> {
    let mut bands = Vec::new();
    for band in 0..=9 {
        let overrides = (0..1024)
            .filter(|coefficient| iw44_bucket_band(coefficient / 16) == band)
            .map(|coefficient| (block, coefficient, 0))
            .collect::<Vec<_>>();
        if overrides.is_empty() {
            continue;
        }

        let aggregate = iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &overrides,
            overrides.len(),
            sample,
            "band",
        )?;
        bands.push(Iw44CoefficientReconstructionBandAggregate {
            band,
            coefficient_count: aggregate.coefficient_count,
            sample_x: aggregate.sample_x,
            sample_y: aggregate.sample_y,
            original_raw: aggregate.original_raw,
            zeroed_raw: aggregate.zeroed_raw,
            raw_delta: aggregate.raw_delta,
            original_normalized: aggregate.original_normalized,
            zeroed_normalized: aggregate.zeroed_normalized,
            normalized_delta: aggregate.normalized_delta,
        });
    }

    Ok(bands)
}

fn iw44_bucket_zeroed_reconstruction(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    block: usize,
    sample: Iw44PlaneSample,
    band_zeroed: &[Iw44CoefficientReconstructionBandAggregate],
) -> anyhow::Result<Vec<Iw44CoefficientReconstructionBucketAggregate>> {
    let high_impact_bands = band_zeroed
        .iter()
        .filter(|band| band.normalized_delta.abs() >= 10)
        .map(|band| band.band)
        .collect::<Vec<_>>();
    let mut buckets = Vec::new();
    for bucket in 0..64 {
        if !high_impact_bands.contains(&iw44_bucket_band(bucket)) {
            continue;
        }

        let overrides = (0..16)
            .map(|offset| (block, (bucket * 16) + offset, 0))
            .collect::<Vec<_>>();
        let aggregate = iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &overrides,
            16,
            sample,
            "bucket",
        )?;
        if aggregate.raw_delta == 0 && aggregate.normalized_delta == 0 {
            continue;
        }
        buckets.push(Iw44CoefficientReconstructionBucketAggregate {
            bucket,
            band: iw44_bucket_band(bucket),
            coefficient_count: aggregate.coefficient_count,
            sample_x: aggregate.sample_x,
            sample_y: aggregate.sample_y,
            original_raw: aggregate.original_raw,
            zeroed_raw: aggregate.zeroed_raw,
            raw_delta: aggregate.raw_delta,
            original_normalized: aggregate.original_normalized,
            zeroed_normalized: aggregate.zeroed_normalized,
            normalized_delta: aggregate.normalized_delta,
            contributors: Vec::new(),
        });
    }
    buckets.sort_by_key(|bucket| std::cmp::Reverse(bucket.raw_delta.unsigned_abs()));
    for bucket in buckets.iter_mut().take(3) {
        bucket.contributors =
            iw44_bucket_coefficient_contributors(decoder, coefficients, bucket.bucket)?;
    }

    Ok(buckets)
}

fn iw44_bucket_coefficient_contributors(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    bucket: usize,
) -> anyhow::Result<Vec<Iw44CoefficientEntry>> {
    let mut entries = Vec::new();
    for offset in 0..16 {
        let coefficient = (bucket * 16) + offset;
        let x = coefficients.block_x + iw44_zigzag_col(coefficient);
        let y = coefficients.block_y + iw44_zigzag_row(coefficient);
        let value = iw44_coefficient_value(decoder, coefficients.plane, x, y)?;
        if value == 0 {
            continue;
        }
        entries.push(iw44_coefficient_entry(
            coefficients.block_x,
            coefficients.block_y,
            x,
            y,
            value,
        ));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.absolute));
    entries.truncate(4);

    Ok(entries)
}

fn iw44_reconstruction_aggregate(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    overrides: &[(usize, usize, i16)],
    coefficient_count: usize,
    sample: Iw44PlaneSample,
    label: &str,
) -> anyhow::Result<Iw44CoefficientReconstructionAggregate> {
    let zeroed = decoder
        .reconstruct_plane_with_coefficient_values(plane, overrides)
        .with_context(|| {
            format!(
                "failed to reconstruct {} plane with {label} zeroed",
                iw44_plane_name(plane)
            )
        })?;
    let zeroed_raw = zeroed.samples[sample.y * zeroed.width + sample.x];
    let zeroed_normalized = normalize_iw44_sample(zeroed_raw);

    Ok(Iw44CoefficientReconstructionAggregate {
        coefficient_count,
        sample_x: sample.x,
        sample_y: sample.y,
        original_raw: sample.raw,
        zeroed_raw,
        raw_delta: i32::from(sample.raw) - i32::from(zeroed_raw),
        original_normalized: sample.normalized,
        zeroed_normalized,
        normalized_delta: sample.normalized - zeroed_normalized,
    })
}

fn iw44_trace_values(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    entries: &[Iw44CoefficientEntry],
) -> anyhow::Result<Vec<Iw44CoefficientEntry>> {
    let coefficient_planes = decoder.coefficient_planes();
    let plane_data = coefficient_planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} coefficient plane", iw44_plane_name(plane)))?;

    Ok(entries
        .iter()
        .map(|entry| {
            let value = plane_data.coefficients[entry.y * plane_data.width + entry.x];
            Iw44CoefficientEntry {
                value,
                absolute: value.unsigned_abs(),
                ..*entry
            }
        })
        .collect())
}

fn iw44_samples_to_rgb(
    y: Iw44PlaneSample,
    cb: Option<Iw44PlaneSample>,
    cr: Option<Iw44PlaneSample>,
) -> [u8; 3] {
    let Some(cb) = cb else {
        let value = clamp_u8(127 - y.normalized);
        return [value, value, value];
    };
    let cr = cr.expect("Cb and Cr samples should both exist for color IW44");
    ycbcr_pixel_to_rgb(y.normalized, cb.normalized, cr.normalized)
}

fn normalize_iw44_sample(value: i16) -> i32 {
    ((i32::from(value) + 32) >> 6).clamp(-128, 127)
}

fn ycbcr_pixel_to_rgb(y: i32, cb: i32, cr: i32) -> [u8; 3] {
    let t2 = cr + (cr >> 1);
    let t3 = y + 128 - (cb >> 2);
    [
        clamp_u8(y + 128 + t2),
        clamp_u8(t3 - (t2 >> 1)),
        clamp_u8(t3 + (cb << 1)),
    ]
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).expect("clamped RGB component should fit u8")
}

fn print_iw44_pixel_inspection(inspection: &Iw44PixelInspection) {
    let role = match inspection.role {
        djvulpes::Iw44LayerRole::Foreground => "foreground",
        djvulpes::Iw44LayerRole::Background => "background",
    };
    println!("IW44 role: {role}");
    println!(
        "source pixel: x={} y={}",
        inspection.source_x, inspection.source_y
    );
    println!(
        "rgb: actual=#{:02x}{:02x}{:02x} computed=#{:02x}{:02x}{:02x}",
        inspection.rgb[0],
        inspection.rgb[1],
        inspection.rgb[2],
        inspection.computed_rgb[0],
        inspection.computed_rgb[1],
        inspection.computed_rgb[2]
    );
    print_iw44_plane_sample("Y", inspection.y);
    if let Some(cb) = inspection.cb {
        print_iw44_plane_sample("Cb", cb);
    }
    if let Some(cr) = inspection.cr {
        print_iw44_plane_sample("Cr", cr);
    }
    if let Some(neighborhood) = &inspection.y_neighborhood {
        print_iw44_luma_neighborhood(neighborhood);
    }
    if let Some(coefficients) = &inspection.y_coefficients {
        print_iw44_coefficient_block_summary(coefficients);
    }
    if let Some(trace) = &inspection.y_coefficient_trace {
        print_iw44_coefficient_trace(trace);
    }
    if let Some(event_traces) = &inspection.y_coefficient_event_traces {
        print_iw44_coefficient_event_traces(event_traces);
    }
    if let Some(reconstruction) = &inspection.y_coefficient_reconstruction {
        print_iw44_coefficient_reconstruction_trace(reconstruction);
    }
}

fn print_iw44_plane_sample(label: &str, sample: Iw44PlaneSample) {
    println!(
        "{label}: sample_x={} sample_y={} raw={} normalized={}",
        sample.x, sample.y, sample.raw, sample.normalized
    );
}

fn print_iw44_luma_neighborhood(neighborhood: &Iw44LumaNeighborhood) {
    println!(
        "Y neighborhood: page_origin={}x{} source_origin={}x{} width={} height={}",
        neighborhood.page_min_x,
        neighborhood.page_min_y,
        neighborhood.source_min_x,
        neighborhood.source_min_y,
        neighborhood.samples.first().map_or(0, Vec::len),
        neighborhood.samples.len()
    );
    println!("Y normalized:");
    for row in &neighborhood.samples {
        for sample in row {
            print!("{:>5}", sample.normalized);
        }
        println!();
    }
    println!("Y raw:");
    for row in &neighborhood.samples {
        for sample in row {
            print!("{:>7}", sample.raw);
        }
        println!();
    }
}

fn print_iw44_coefficient_block_summary(summary: &Iw44CoefficientBlockSummary) {
    println!(
        "{} coefficient block: block={}x{} origin={}x{} size={}x{} non_zero={} max_abs={} abs_sum={}",
        iw44_plane_name(summary.plane),
        summary.block_col,
        summary.block_row,
        summary.block_x,
        summary.block_y,
        summary.width,
        summary.height,
        summary.non_zero,
        summary.max_abs,
        summary.abs_sum
    );
    for entry in &summary.entries {
        println!(
            "  coefficient: x={} y={} local={}x{} index={} bucket={} offset={} band={} value={} abs={}",
            entry.x,
            entry.y,
            entry.local_x,
            entry.local_y,
            entry.index,
            entry.bucket,
            entry.bucket_offset,
            entry.band,
            entry.value,
            entry.absolute
        );
    }
}

fn print_iw44_coefficient_trace(trace: &[Iw44CoefficientTraceStep]) {
    println!("Y coefficient trace:");
    for step in trace {
        if let Some(slice_index) = step.slice_index {
            print!(
                "  chunk #{} serial={} slice={} slices={}",
                step.chunk_index, step.chunk_serial, slice_index, step.slices_decoded
            );
        } else {
            print!(
                "  chunk #{} serial={} slices={}",
                step.chunk_index, step.chunk_serial, step.slices_decoded
            );
        }
        for value in &step.values {
            print!(
                " index={} bucket={} band={} value={}",
                value.index, value.bucket, value.band, value.value
            );
        }
        println!();
    }
}

fn print_iw44_coefficient_event_traces(traces: &[Iw44CoefficientEventTrace]) {
    println!("Y coefficient event trace:");
    for trace in traces {
        println!(
            "  index={} bucket={} band={} x={} y={} events={}",
            trace.entry.index,
            trace.entry.bucket,
            trace.entry.band,
            trace.entry.x,
            trace.entry.y,
            trace.events.len()
        );
        for trace_event in &trace.events {
            let event = trace_event.event;
            print!(
                "    chunk #{} serial={} slice={} band={} q={} before={} after={}",
                trace_event.chunk_index,
                trace_event.chunk_serial,
                event.slice_index,
                event.band,
                event.quant,
                event.before,
                event.after
            );
            print_iw44_coefficient_event_kind(event.kind);
            println!();
        }
    }
}

fn print_iw44_coefficient_event_kind(kind: djvulpes::Iw44CoefficientEventKind) {
    match kind {
        djvulpes::Iw44CoefficientEventKind::BucketDecision {
            context,
            decision,
            block_state,
            bucket_state,
        } => print!(
            " bucket context={context} decision={decision} block_state={block_state:#04x} bucket_state={bucket_state:#04x}"
        ),
        djvulpes::Iw44CoefficientEventKind::ActivationDecision {
            context,
            decision,
            unknown_count,
        } => print!(
            " activation context={context} decision={decision} unknown_count={unknown_count}"
        ),
        djvulpes::Iw44CoefficientEventKind::Activated { sign } => {
            print!(" activated sign={sign}");
        }
        djvulpes::Iw44CoefficientEventKind::RefinementDecision {
            context_coded,
            decision,
            magnitude,
        } => print!(
            " refinement context_coded={context_coded} decision={decision} magnitude={magnitude}"
        ),
        djvulpes::Iw44CoefficientEventKind::Refined => print!(" refined"),
    }
}

fn print_iw44_coefficient_reconstruction_trace(summary: &Iw44CoefficientReconstructionSummary) {
    println!("Y coefficient reconstruction trace:");
    print_iw44_reconstruction_aggregate("all_listed_zeroed", summary.all_zeroed);
    print_iw44_reconstruction_aggregate("block_zeroed", summary.block_zeroed);
    print_iw44_transform_variant("rows_then_columns", summary.rows_then_columns);
    print_iw44_transform_variant("padded_extent", summary.padded_extent);
    print_iw44_band_reconstruction_trace(&summary.band_zeroed);
    print_iw44_bucket_reconstruction_trace(&summary.bucket_zeroed);
    print_iw44_individual_reconstruction_trace(&summary.entries);
}

fn print_iw44_reconstruction_aggregate(
    label: &str,
    aggregate: Iw44CoefficientReconstructionAggregate,
) {
    println!(
        "  {label} count={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
        aggregate.coefficient_count,
        aggregate.sample_x,
        aggregate.sample_y,
        aggregate.original_raw,
        aggregate.zeroed_raw,
        aggregate.raw_delta,
        aggregate.original_normalized,
        aggregate.zeroed_normalized,
        aggregate.normalized_delta
    );
}

fn print_iw44_transform_variant(label: &str, trace: Iw44TransformOrderTrace) {
    println!(
        "  {label} sample={}x{} original_raw={} alternate_raw={} raw_delta={} original_norm={} alternate_norm={} norm_delta={}",
        trace.sample_x,
        trace.sample_y,
        trace.original_raw,
        trace.alternate_raw,
        trace.raw_delta,
        trace.original_normalized,
        trace.alternate_normalized,
        trace.normalized_delta
    );
}

fn print_iw44_band_reconstruction_trace(bands: &[Iw44CoefficientReconstructionBandAggregate]) {
    for band in bands {
        println!(
            "  band_zeroed band={} count={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
            band.band,
            band.coefficient_count,
            band.sample_x,
            band.sample_y,
            band.original_raw,
            band.zeroed_raw,
            band.raw_delta,
            band.original_normalized,
            band.zeroed_normalized,
            band.normalized_delta
        );
    }
}

fn print_iw44_bucket_reconstruction_trace(
    buckets: &[Iw44CoefficientReconstructionBucketAggregate],
) {
    let mut buckets = buckets.to_vec();
    buckets.sort_by_key(|bucket| std::cmp::Reverse(bucket.raw_delta.unsigned_abs()));
    for bucket in &buckets {
        println!(
            "  bucket_zeroed bucket={} band={} count={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
            bucket.bucket,
            bucket.band,
            bucket.coefficient_count,
            bucket.sample_x,
            bucket.sample_y,
            bucket.original_raw,
            bucket.zeroed_raw,
            bucket.raw_delta,
            bucket.original_normalized,
            bucket.zeroed_normalized,
            bucket.normalized_delta
        );
        for contributor in &bucket.contributors {
            println!(
                "    contributor index={} offset={} x={} y={} value={} abs={}",
                contributor.index,
                contributor.bucket_offset,
                contributor.x,
                contributor.y,
                contributor.value,
                contributor.absolute
            );
        }
    }
}

fn print_iw44_individual_reconstruction_trace(traces: &[Iw44CoefficientReconstructionTrace]) {
    for trace in traces {
        println!(
            "  index={} bucket={} band={} coefficient={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
            trace.entry.index,
            trace.entry.bucket,
            trace.entry.band,
            trace.entry.value,
            trace.sample_x,
            trace.sample_y,
            trace.original_raw,
            trace.zeroed_raw,
            trace.raw_delta,
            trace.original_normalized,
            trace.zeroed_normalized,
            trace.normalized_delta
        );
    }
}

pub fn run_dump_bitonal(path: &Path, number: usize, output_dir: &Path) -> anyhow::Result<()> {
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
    let dictionaries = plan.bitonal_dictionary_payloads(&bytes);

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("output dir: {}", output_dir.display());
    println!("Sjbz payloads: raw JB2");

    let mut written = 0usize;
    for payload in dictionaries {
        let output = output_dir.join(format!("page-{number}-chunk-{}.djbz", payload.index));
        fs::write(&output, payload.bytes)
            .with_context(|| format!("failed to write {}", output.display()))?;
        println!(
            "wrote {} bytes to {}",
            payload.bytes.len(),
            output.display()
        );
        written += 1;
    }
    for payload in plan.bitonal_image_payloads(&bytes) {
        let output = output_dir.join(format!("page-{number}-chunk-{}.jb2", payload.index));
        fs::write(&output, payload.bytes)
            .with_context(|| format!("failed to write {}", output.display()))?;
        println!(
            "wrote {} bytes to {}",
            payload.bytes.len(),
            output.display()
        );
        written += 1;
    }

    if written == 0 {
        println!("bitonal chunks: none");
    }

    Ok(())
}

pub fn run_dump_image_layers(path: &Path, number: usize, output_dir: &Path) -> anyhow::Result<()> {
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

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("output dir: {}", output_dir.display());
    println!("IW44 payloads: raw FG44/BG44");

    let mut written = 0usize;
    let foreground = plan.foreground_layer_payloads(&bytes);
    let background = plan.background_layer_payloads(&bytes);

    for payload in &foreground {
        let output = output_dir.join(format!("page-{number}-chunk-{}.fg44", payload.index));
        fs::write(&output, payload.bytes)
            .with_context(|| format!("failed to write {}", output.display()))?;
        println!(
            "wrote {} bytes to {}",
            payload.bytes.len(),
            output.display()
        );
        written += 1;
    }
    for payload in &background {
        let output = output_dir.join(format!("page-{number}-chunk-{}.bg44", payload.index));
        fs::write(&output, payload.bytes)
            .with_context(|| format!("failed to write {}", output.display()))?;
        println!(
            "wrote {} bytes to {}",
            payload.bytes.len(),
            output.display()
        );
        written += 1;
    }

    if written == 0 {
        println!("IW44 chunks: none");
    }
    print_decoded_iw44_payload_summary("foreground", &foreground)?;
    print_decoded_iw44_payload_summary("background", &background)?;
    write_decoded_iw44_layer_ppm(
        "foreground",
        number,
        output_dir,
        plan.foreground_iw44_layer(&bytes)?,
    )?;
    write_decoded_iw44_layer_ppm(
        "background",
        number,
        output_dir,
        plan.background_iw44_layer(&bytes)?,
    )?;

    Ok(())
}

fn write_decoded_iw44_layer_ppm(
    role: &str,
    page_number: usize,
    output_dir: &Path,
    layer: Option<djvulpes::RenderedIw44Layer>,
) -> anyhow::Result<()> {
    let Some(layer) = layer else {
        return Ok(());
    };

    let output = output_dir.join(format!("page-{page_number}-{role}.ppm"));
    fs::write(&output, iw44_rgb_image_ppm_bytes(&layer.image))
        .with_context(|| format!("failed to write {}", output.display()))?;
    println!(
        "wrote decoded IW44 {role} RGB {}x{} to {}",
        layer.image.width,
        layer.image.height,
        output.display()
    );

    Ok(())
}

fn iw44_rgb_image_ppm_bytes(image: &djvulpes::Iw44RgbImage) -> Vec<u8> {
    let mut bytes = format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
    bytes.extend_from_slice(&image.pixels);
    bytes
}

pub fn run_inspect_iw44_pixel(
    path: &Path,
    number: usize,
    mode: PageRenderMode,
    options: &Iw44PixelInspectOptions,
) -> anyhow::Result<()> {
    let inspection = with_page_render_plan(path, number, |bytes, plan| {
        inspect_iw44_pixel(&plan, bytes, mode, options)
    })?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("mode: {}", mode.as_str());
    println!("page pixel: x={} y={}", options.x, options.y);
    print_iw44_pixel_inspection(&inspection);

    Ok(())
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

fn read_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
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
    use djvulpes::{PdfPageImage, write_page_image_pdf_iter};

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
            panic!("bitonal-only page should be embedded as an image mask");
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
        assert!(text.contains("/Subtype /Image /Width 3423 /Height 5075 /ImageMask true"));
        assert!(text.contains("/Subtype /Image /Width 3486 /Height 2783"));
        assert!(text.contains("/ColorSpace /DeviceRGB /BitsPerComponent 8"));
    }

    #[test]
    fn run_render_pdf_writes_rypka_page_961_rgb_pdf() {
        let output =
            std::env::temp_dir().join(format!("djvulpes-page-961-{}.pdf", std::process::id()));

        run_render_pdf(Path::new("Rypka-HIL.djvu"), &output, 961, Some(961))
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

        run_render_pdf(Path::new("Rypka-HIL.djvu"), &output, 68, Some(68))
            .expect("render-pdf should write page 68");
        let pdf = fs::read(&output).expect("rendered PDF should be readable");
        let _ = fs::remove_file(&output);
        let text = String::from_utf8_lossy(&pdf);

        assert!(text.starts_with("%PDF-1.4\n"));
        assert!(text.contains("/Type /Pages /Count 1"));
        assert!(text.contains("/Subtype /Image /Width 3423 /Height 5075 /ImageMask true"));
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
        assert!(text.contains("/Subtype /Image /Width 3423 /Height 5075 /ImageMask true"));
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
