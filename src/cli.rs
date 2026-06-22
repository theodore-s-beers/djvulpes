use crate::commands::{
    Iw44PixelInspectOptions, Iw44PixelTrace, run_compare_ppm, run_compare_render_page,
    run_compare_render_page_layer, run_compare_render_pages, run_dirm, run_dump_bitonal,
    run_dump_image_layers, run_extract_text, run_form, run_forms, run_inspect_iw44_pixel,
    run_outline, run_page, run_pages, run_render_page, run_render_page_layer, run_render_page_pdf,
    run_render_pdf, run_render_plan, run_summary, run_text,
};
use clap::{Parser, Subcommand};
use djvulpes::{PageRenderMode, RenderCompareLimits};
use std::path::PathBuf;

const DEFAULT_FILE: &str = "Rypka-HIL.djvu";

#[derive(Debug, Parser)]
#[command(version, about, long_about = None, args_conflicts_with_subcommands = true)]
struct Cli {
    #[arg(default_value = DEFAULT_FILE)]
    file: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a high-level document summary.
    Summary {
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// List pages with basic page metadata.
    Pages {
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// List forms referenced by the document directory.
    Forms {
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Inspect a FORM at an absolute byte offset.
    Form {
        offset: usize,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Inspect the bundled document directory.
    Dirm {
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Inspect one page by 1-based page number.
    Page {
        number: usize,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Show the renderer-facing chunk plan for one page.
    RenderPlan {
        number: usize,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Render a page-sized RGB PPM image.
    RenderPage {
        number: usize,
        output: PathBuf,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Render one page layer mode to a page-sized RGB PPM image.
    RenderPageLayer {
        number: usize,
        mode: PageRenderMode,
        output: PathBuf,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Render a page-sized RGB PDF image.
    RenderPagePdf {
        number: usize,
        output: PathBuf,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Render all pages into one RGB PDF.
    RenderPdf {
        output: PathBuf,
        #[arg(long, default_value_t = 1)]
        from_page: usize,
        #[arg(long)]
        to_page: Option<usize>,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Compare a rendered page against a binary RGB PPM oracle.
    CompareRenderPage {
        number: usize,
        oracle: PathBuf,
        #[arg(long, default_value_t = 0)]
        max_different_pixels: usize,
        #[arg(long, default_value_t = 0)]
        max_abs_delta: u8,
        #[arg(long)]
        max_delta_pixels: Option<usize>,
        #[arg(long, default_value_t = 0.0)]
        max_mean_abs_delta: f64,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Compare a page range against page-<number>.ppm files in an oracle directory.
    CompareRenderPages {
        oracle_dir: PathBuf,
        #[arg(long, default_value = "full")]
        mode: PageRenderMode,
        #[arg(long, default_value_t = 1)]
        from_page: usize,
        #[arg(long)]
        to_page: Option<usize>,
        #[arg(long, default_value_t = 0)]
        max_different_pixels: usize,
        #[arg(long, default_value_t = 0)]
        max_abs_delta: u8,
        #[arg(long)]
        max_delta_pixels: Option<usize>,
        #[arg(long, default_value_t = 0.0)]
        max_mean_abs_delta: f64,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Compare one rendered page layer mode against a binary RGB PPM oracle.
    CompareRenderPageLayer {
        number: usize,
        mode: PageRenderMode,
        oracle: PathBuf,
        #[arg(long, default_value_t = 0)]
        max_different_pixels: usize,
        #[arg(long, default_value_t = 0)]
        max_abs_delta: u8,
        #[arg(long)]
        max_delta_pixels: Option<usize>,
        #[arg(long, default_value_t = 0.0)]
        max_mean_abs_delta: f64,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Compare two binary RGB PPM images.
    ComparePpm {
        actual: PathBuf,
        expected: PathBuf,
        #[arg(long, default_value_t = 0)]
        max_different_pixels: usize,
        #[arg(long, default_value_t = 0)]
        max_abs_delta: u8,
        #[arg(long)]
        max_delta_pixels: Option<usize>,
        #[arg(long, default_value_t = 0.0)]
        max_mean_abs_delta: f64,
    },
    /// Dump Djbz/Sjbz JB2 bitonal payloads for one page.
    DumpBitonal {
        number: usize,
        output_dir: PathBuf,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Dump FG44/BG44 IW44 payloads for one page.
    DumpImageLayers {
        number: usize,
        output_dir: PathBuf,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Inspect decoded IW44 samples at a page-space pixel.
    InspectIw44Pixel {
        number: usize,
        mode: PageRenderMode,
        x: u32,
        y: u32,
        #[arg(long, default_value_t = 0)]
        radius: u8,
        #[arg(long, default_value_t = 0)]
        coefficients: usize,
        #[arg(long = "coefficient-index")]
        coefficient_indices: Vec<usize>,
        #[arg(long)]
        trace_coefficients: bool,
        #[arg(long)]
        trace_slices: bool,
        #[arg(long)]
        trace_events: bool,
        #[arg(long)]
        trace_reconstruction: bool,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Extract hidden text from one page by 1-based page number.
    Text {
        number: usize,
        #[arg(long)]
        zones: bool,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Extract hidden text as raw djvutxt-compatible or structured djvused-style output.
    ExtractText {
        #[arg(long, default_value_t = 1)]
        from_page: usize,
        #[arg(long)]
        to_page: Option<usize>,
        #[arg(long)]
        structured: bool,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
    /// Print the document outline/bookmarks.
    Outline {
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run_command(cli.command.unwrap_or(Command::Summary { file: cli.file }))
}

#[expect(
    clippy::too_many_lines,
    reason = "CLI command dispatch is intentionally centralized"
)]
fn run_command(command: Command) -> anyhow::Result<()> {
    if run_compare_command(&command)? {
        return Ok(());
    }

    match command {
        Command::Summary { file } => run_summary(&file)?,
        Command::Pages { file } => run_pages(&file)?,
        Command::Forms { file } => run_forms(&file)?,
        Command::Form { offset, file } => run_form(&file, offset)?,
        Command::Dirm { file } => run_dirm(&file)?,
        Command::Page { number, file } => run_page(&file, number)?,
        Command::RenderPlan { number, file } => run_render_plan(&file, number)?,
        Command::RenderPage {
            number,
            output,
            file,
        } => run_render_page(&file, number, &output)?,
        Command::RenderPageLayer {
            number,
            mode,
            output,
            file,
        } => run_render_page_layer(&file, number, mode, &output)?,
        Command::RenderPagePdf {
            number,
            output,
            file,
        } => run_render_page_pdf(&file, number, &output)?,
        Command::RenderPdf {
            output,
            from_page,
            to_page,
            file,
        } => run_render_pdf(&file, &output, from_page, to_page)?,
        Command::CompareRenderPage { .. }
        | Command::CompareRenderPages { .. }
        | Command::CompareRenderPageLayer { .. }
        | Command::ComparePpm { .. } => unreachable!("compare command already handled"),
        Command::DumpBitonal {
            number,
            output_dir,
            file,
        } => run_dump_bitonal(&file, number, &output_dir)?,
        Command::DumpImageLayers {
            number,
            output_dir,
            file,
        } => run_dump_image_layers(&file, number, &output_dir)?,
        Command::InspectIw44Pixel {
            number,
            mode,
            x,
            y,
            radius,
            coefficients,
            coefficient_indices,
            trace_coefficients,
            trace_slices,
            trace_events,
            trace_reconstruction,
            file,
        } => run_inspect_iw44_pixel(
            &file,
            number,
            mode,
            &Iw44PixelInspectOptions {
                x,
                y,
                radius,
                coefficient_limit: coefficients,
                coefficient_indices,
                traces: {
                    let mut traces = Vec::with_capacity(4);
                    if trace_coefficients {
                        traces.push(Iw44PixelTrace::Coefficients);
                    }
                    if trace_slices {
                        traces.push(Iw44PixelTrace::Slices);
                    }
                    if trace_events {
                        traces.push(Iw44PixelTrace::Events);
                    }
                    if trace_reconstruction {
                        traces.push(Iw44PixelTrace::Reconstruction);
                    }
                    traces
                },
            },
        )?,
        Command::Text {
            number,
            zones,
            file,
        } => run_text(&file, number, zones)?,
        Command::ExtractText {
            from_page,
            to_page,
            structured,
            file,
        } => run_extract_text(&file, from_page, to_page, structured)?,
        Command::Outline { file } => run_outline(&file)?,
    }

    Ok(())
}

fn run_compare_command(command: &Command) -> anyhow::Result<bool> {
    match command {
        Command::CompareRenderPage {
            number,
            oracle,
            max_different_pixels,
            max_abs_delta,
            max_delta_pixels,
            max_mean_abs_delta,
            file,
        } => run_compare_render_page(
            file,
            *number,
            oracle,
            RenderCompareLimits::new(
                *max_different_pixels,
                *max_abs_delta,
                *max_delta_pixels,
                *max_mean_abs_delta,
            ),
        )?,
        Command::CompareRenderPages {
            oracle_dir,
            mode,
            from_page,
            to_page,
            max_different_pixels,
            max_abs_delta,
            max_delta_pixels,
            max_mean_abs_delta,
            file,
        } => run_compare_render_pages(
            file,
            oracle_dir,
            *mode,
            *from_page,
            *to_page,
            RenderCompareLimits::new(
                *max_different_pixels,
                *max_abs_delta,
                *max_delta_pixels,
                *max_mean_abs_delta,
            ),
        )?,
        Command::CompareRenderPageLayer {
            number,
            mode,
            oracle,
            max_different_pixels,
            max_abs_delta,
            max_delta_pixels,
            max_mean_abs_delta,
            file,
        } => run_compare_render_page_layer(
            file,
            *number,
            *mode,
            oracle,
            RenderCompareLimits::new(
                *max_different_pixels,
                *max_abs_delta,
                *max_delta_pixels,
                *max_mean_abs_delta,
            ),
        )?,
        Command::ComparePpm {
            actual,
            expected,
            max_different_pixels,
            max_abs_delta,
            max_delta_pixels,
            max_mean_abs_delta,
        } => run_compare_ppm(
            actual,
            expected,
            RenderCompareLimits::new(
                *max_different_pixels,
                *max_abs_delta,
                *max_delta_pixels,
                *max_mean_abs_delta,
            ),
        )?,
        _ => return Ok(false),
    }

    Ok(true)
}
