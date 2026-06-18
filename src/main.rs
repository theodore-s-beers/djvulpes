#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(clippy::similar_names, clippy::uninlined_format_args)]

use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use djvulpes::{
    Chunk, Document, DocumentFormKind, Form, Result, parse_chunks, parse_form_at, read_page_info,
};

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
    /// Inspect one page by 1-based page number.
    Page {
        number: usize,
        #[arg(default_value = DEFAULT_FILE)]
        file: PathBuf,
    },
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Summary { file: cli.file }) {
        Command::Summary { file } => run_summary(&file)?,
        Command::Pages { file } => run_pages(&file)?,
        Command::Forms { file } => run_forms(&file)?,
        Command::Page { number, file } => run_page(&file, number)?,
    }

    Ok(())
}

fn run_summary(path: &PathBuf) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
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

fn run_pages(path: &PathBuf) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
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

fn run_forms(path: &PathBuf) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
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

fn run_page(path: &PathBuf, number: usize) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if number == 0 {
        return Err("page number must be 1 or greater".into());
    }

    let bytes = fs::read(path)?;
    let document = Document::parse(&bytes)?;
    let Some(document_form) = document
        .forms
        .iter()
        .filter(|form| form.kind == DocumentFormKind::Page)
        .nth(number - 1)
    else {
        return Err(format!(
            "page {number} not found; document has {} pages",
            document.form_kind_counts().pages
        )
        .into());
    };

    println!("file: {}", path.display());
    println!("page: {number}");
    print_form_detail(&bytes, &document_form.form, document_form.offset)?;

    Ok(())
}

fn print_root_chunk_counts(document: &Document<'_>, bytes: &[u8]) -> Result<()> {
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

fn print_root_chunk_sample(bytes: &[u8], chunks: &[Chunk<'_>]) -> Result<()> {
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

fn print_chunk_line(bytes: &[u8], chunk: &Chunk<'_>) -> Result<()> {
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

fn print_dirm_summary(document: &Document<'_>, bytes: &[u8]) -> Result<()> {
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

fn print_form_detail(bytes: &[u8], form: &Form<'_>, offset: u32) -> Result<()> {
    println!(
        "form: @{offset} FORM:{} size={} data=[{}..{})",
        form.kind, form.chunk.size, form.chunk.data_start, form.chunk.data_end
    );

    if let Some(info) = read_page_info(bytes, form)? {
        println!(
            "INFO: {}x{} dpi={} gamma={:.1} version={} rotation={}",
            info.width, info.height, info.dpi, info.gamma, info.version, info.rotation
        );
    }

    println!();
    println!("child chunks:");
    for chunk in parse_chunks(bytes, form.children_start, form.chunk.data_end)? {
        print_chunk_line(bytes, &chunk)?;
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
