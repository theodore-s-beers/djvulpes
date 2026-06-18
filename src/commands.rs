use djvulpes::{
    Chunk, Document, DocumentFormKind, Form, PageChunk, PageChunkPayload, Result, parse_form_at,
    read_page_details,
};
use std::fs;
use std::path::Path;

pub fn run_summary(path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
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

pub fn run_pages(path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
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

pub fn run_forms(path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
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

pub fn run_page(path: &Path, number: usize) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
    print_page_detail(&bytes, &document_form.form, document_form.offset)?;

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

fn print_page_detail(bytes: &[u8], form: &Form<'_>, offset: u32) -> Result<()> {
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
        print_page_chunk_line(bytes, &page_chunk)?;
    }

    Ok(())
}

fn print_page_chunk_line(bytes: &[u8], page_chunk: &PageChunk<'_>) -> Result<()> {
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
            format_page_chunk_payload(page_chunk)
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
            format_page_chunk_payload(page_chunk)
        );
    }

    Ok(())
}

fn format_page_chunk_payload(page_chunk: &PageChunk<'_>) -> String {
    match page_chunk.payload {
        PageChunkPayload::Include { id } => format!(" id={id}"),
        PageChunkPayload::Raw => String::new(),
    }
}

const fn display_form_kind(kind: DocumentFormKind) -> &'static str {
    match kind {
        DocumentFormKind::Page => "page",
        DocumentFormKind::Shared => "shared",
        DocumentFormKind::Thumbnails => "thumbs",
        DocumentFormKind::Other => "other",
    }
}
