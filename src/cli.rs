use crate::commands::{
    CompareRenderOptions, RenderPdfOptions, RenderPdfProgress, run_compare_ppm, run_compare_render,
    run_dirm, run_dump_bitonal, run_dump_image_layers, run_extract_text, run_form, run_forms,
    run_outline, run_page, run_pages, run_render_page_image, run_render_pdf, run_render_plan,
    run_summary,
};
use clap::{Parser, Subcommand};
use djvulpes::{PageRenderMode, RenderCompareLimits};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    version,
    about,
    long_about = None,
    args_conflicts_with_subcommands = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a high-level document summary.
    Summary { file: PathBuf },
    /// List pages or inspect one page by 1-based page number.
    Pages {
        #[arg(long)]
        page: Option<usize>,
        file: PathBuf,
    },
    /// List forms or inspect one FORM at an absolute byte offset.
    Forms {
        #[arg(long)]
        offset: Option<usize>,
        file: PathBuf,
    },
    /// Inspect the bundled document directory.
    Dirm { file: PathBuf },
    /// Show the renderer-facing chunk plan for one page.
    RenderPlan {
        #[arg(long)]
        page: usize,
        file: PathBuf,
    },
    /// Render one page image mode to a page-sized RGB PPM image.
    RenderPageImage {
        #[arg(long)]
        page: usize,
        #[arg(long, default_value = "full")]
        mode: PageRenderMode,
        #[arg(long)]
        output: PathBuf,
        file: PathBuf,
    },
    /// Render all pages into one RGB PDF.
    RenderPdf {
        #[arg(short, long)]
        output: PathBuf,
        #[arg(short = 'f', long, default_value_t = 1)]
        from_page: usize,
        #[arg(short = 't', long)]
        to_page: Option<usize>,
        #[arg(short, long, value_enum, default_value_t = RenderPdfProgress::Sparse)]
        progress: RenderPdfProgress,
        #[arg(short, long)]
        verbose: bool,
        #[arg(long)]
        timings: bool,
        #[arg(short, long)]
        jobs: Option<usize>,
        file: PathBuf,
    },
    /// Compare rendered pages against binary RGB PPM oracle files.
    CompareRender {
        #[arg(long)]
        oracle: Option<PathBuf>,
        #[arg(long)]
        oracle_dir: Option<PathBuf>,
        #[arg(long)]
        page: Option<usize>,
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
        #[arg(long)]
        page: usize,
        #[arg(long)]
        output_dir: PathBuf,
        file: PathBuf,
    },
    /// Dump FG44/BG44 IW44 payloads for one page.
    DumpImageLayers {
        #[arg(long)]
        page: usize,
        #[arg(long)]
        output_dir: PathBuf,
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
        file: PathBuf,
    },
    /// Print the document outline/bookmarks.
    Outline { file: PathBuf },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run_command(cli.command)
}

fn run_command(command: Command) -> anyhow::Result<()> {
    if run_compare_command(&command)? {
        return Ok(());
    }
    if run_render_command(&command)? {
        return Ok(());
    }

    match command {
        Command::Summary { file } => run_summary(&file)?,
        Command::Pages { page, file } => match page {
            Some(number) => run_page(&file, number)?,
            None => run_pages(&file)?,
        },
        Command::Forms { offset, file } => match offset {
            Some(offset) => run_form(&file, offset)?,
            None => run_forms(&file)?,
        },
        Command::Dirm { file } => run_dirm(&file)?,
        Command::CompareRender { .. } | Command::ComparePpm { .. } => {
            unreachable!("compare command already handled")
        }
        Command::RenderPlan { .. }
        | Command::RenderPageImage { .. }
        | Command::RenderPdf { .. }
        | Command::DumpBitonal { .. }
        | Command::DumpImageLayers { .. } => unreachable!("render command already handled"),
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

fn run_render_command(command: &Command) -> anyhow::Result<bool> {
    match command {
        Command::RenderPlan { page, file } => run_render_plan(file, *page)?,
        Command::RenderPageImage {
            page,
            mode,
            output,
            file,
        } => run_render_page_image(file, *page, *mode, output)?,
        Command::RenderPdf {
            output,
            from_page,
            to_page,
            progress,
            verbose,
            timings,
            jobs,
            file,
        } => run_render_pdf(
            file,
            output,
            *from_page,
            *to_page,
            RenderPdfOptions {
                progress: *progress,
                verbose: *verbose,
                timings: *timings,
                jobs: *jobs,
            },
        )?,
        Command::DumpBitonal {
            page,
            output_dir,
            file,
        } => run_dump_bitonal(file, *page, output_dir)?,
        Command::DumpImageLayers {
            page,
            output_dir,
            file,
        } => run_dump_image_layers(file, *page, output_dir)?,
        _ => return Ok(false),
    }

    Ok(true)
}

fn run_compare_command(command: &Command) -> anyhow::Result<bool> {
    match command {
        Command::CompareRender {
            oracle,
            oracle_dir,
            page,
            mode,
            from_page,
            to_page,
            max_different_pixels,
            max_abs_delta,
            max_delta_pixels,
            max_mean_abs_delta,
            file,
        } => run_compare_render(
            file,
            CompareRenderOptions {
                oracle: oracle.as_deref(),
                oracle_dir: oracle_dir.as_deref(),
                page: *page,
                mode: *mode,
                from_page: *from_page,
                to_page: *to_page,
            },
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
