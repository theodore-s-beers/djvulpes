use super::{
    print_bitmap_stats, print_iw44_render_summary, print_partial_render_summary, read_file,
    render_page_layer,
};
use anyhow::{Context as _, bail};
use djvulpes::{
    PageRenderMode, RenderCompareLimits, bitmap_diff_failures, bitmap_diff_region_summary,
    bitmap_diff_tile_summaries,
};
use std::path::Path;

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

#[derive(Debug, Clone, Copy)]
pub struct CompareRenderOptions<'a> {
    pub oracle: Option<&'a Path>,
    pub oracle_dir: Option<&'a Path>,
    pub page: Option<usize>,
    pub mode: PageRenderMode,
    pub from_page: usize,
    pub to_page: Option<usize>,
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

fn run_compare_render_oracle_file(
    path: &Path,
    number: usize,
    oracle: &Path,
    mode: PageRenderMode,
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

pub fn run_compare_render(
    path: &Path,
    options: CompareRenderOptions<'_>,
    limits: RenderCompareLimits,
) -> anyhow::Result<()> {
    match (options.oracle, options.oracle_dir) {
        (Some(_), Some(_)) => bail!("use either --oracle or --oracle-dir, not both"),
        (Some(oracle), None) => {
            if options.from_page != 1 || options.to_page.is_some() {
                bail!("--oracle compares one page; use --page instead of --from-page/--to-page");
            }
            let page = options.page.context("--oracle requires --page <number>")?;
            run_compare_render_oracle_file(path, page, oracle, options.mode, limits)
        }
        (None, Some(oracle_dir)) => {
            let (from_page, to_page) = options
                .page
                .map_or((options.from_page, options.to_page), |page| {
                    (page, Some(page))
                });
            run_compare_render_oracle_dir(
                path,
                oracle_dir,
                options.mode,
                from_page,
                to_page,
                limits,
            )
        }
        (None, None) => bail!("compare-render requires --oracle or --oracle-dir"),
    }
}

fn run_compare_render_oracle_dir(
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
