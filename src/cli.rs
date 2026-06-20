use crate::commands::{
    run_dirm, run_form, run_forms, run_page, run_pages, run_render_plan, run_summary, run_text,
};
use clap::{Parser, Subcommand};
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
    /// Extract hidden text from one page by 1-based page number.
    Text {
        number: usize,
        #[arg(long)]
        zones: bool,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Summary { file: cli.file }) {
        Command::Summary { file } => run_summary(&file)?,
        Command::Pages { file } => run_pages(&file)?,
        Command::Forms { file } => run_forms(&file)?,
        Command::Form { offset, file } => run_form(&file, offset)?,
        Command::Dirm { file } => run_dirm(&file)?,
        Command::Page { number, file } => run_page(&file, number)?,
        Command::RenderPlan { number, file } => run_render_plan(&file, number)?,
        Command::Text {
            number,
            zones,
            file,
        } => run_text(&file, number, zones)?,
    }

    Ok(())
}
