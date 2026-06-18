#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(clippy::similar_names, clippy::uninlined_format_args)]

use std::env;
use std::fs;
use std::path::PathBuf;

use djvulpes::{
    Chunk, Result, parse_chunks, parse_dirm, parse_document_root, parse_form_at, read_page_info,
};

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let path = input_path();
    let bytes = fs::read(&path)?;

    println!("file: {}", path.display());
    println!("bytes: {}", bytes.len());

    let root = parse_document_root(&bytes)?;
    println!(
        "root: FORM:{} size={} data=[{}..{})",
        root.kind, root.chunk.size, root.chunk.data_start, root.chunk.data_end
    );

    println!();
    println!("root chunks:");
    let root_chunks = parse_chunks(&bytes, root.children_start, root.chunk.data_end)?;
    println!("  total: {}", root_chunks.len());
    print_root_chunk_counts(&bytes, &root_chunks)?;
    print_root_chunk_sample(&bytes, &root_chunks)?;

    if let Some(dirm_chunk) = root_chunks.iter().find(|chunk| chunk.id == "DIRM") {
        println!();
        print_dirm_summary(&bytes, dirm_chunk)?;
    }

    Ok(())
}

fn input_path() -> PathBuf {
    env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("Rypka-HIL.djvu"), PathBuf::from)
}

fn print_root_chunk_counts(bytes: &[u8], chunks: &[Chunk<'_>]) -> Result<()> {
    let mut dirm_count = 0;
    let mut navm_count = 0;
    let mut djvu_count = 0;
    let mut djvi_count = 0;
    let mut thum_count = 0;
    let mut other_count = 0;

    for chunk in chunks {
        match chunk.id {
            "DIRM" => dirm_count += 1,
            "NAVM" => navm_count += 1,
            "FORM" => match parse_form_at(bytes, chunk.data_start - 8)?.kind {
                "DJVU" => djvu_count += 1,
                "DJVI" => djvi_count += 1,
                "THUM" => thum_count += 1,
                _ => other_count += 1,
            },
            _ => other_count += 1,
        }
    }

    println!(
        "  counts: DIRM={dirm_count}, NAVM={navm_count}, FORM:DJVU={djvu_count}, FORM:DJVI={djvi_count}, FORM:THUM={thum_count}, other={other_count}"
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

fn print_dirm_summary(bytes: &[u8], chunk: &Chunk<'_>) -> Result<()> {
    let dirm = parse_dirm(bytes, chunk)?;
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

    let mut forms = Vec::new();
    let mut unresolved_offsets = 0;
    for offset in &dirm.offsets {
        let Ok(offset_start) = usize::try_from(*offset) else {
            unresolved_offsets += 1;
            continue;
        };

        let Ok(form) = parse_form_at(bytes, offset_start) else {
            unresolved_offsets += 1;
            continue;
        };
        forms.push((*offset, form));
    }

    let page_count = forms.iter().filter(|(_, form)| form.kind == "DJVU").count();
    let shared_count = forms.iter().filter(|(_, form)| form.kind == "DJVI").count();
    let thumbnail_count = forms.iter().filter(|(_, form)| form.kind == "THUM").count();
    println!(
        "  referenced forms: {} DJVU pages, {} DJVI shared, {} THUM thumbnails",
        page_count, shared_count, thumbnail_count
    );
    println!("  unresolved offsets: {unresolved_offsets}");

    println!();
    println!("first referenced forms:");
    for (index, (offset, form)) in forms.iter().take(12).enumerate() {
        print!(
            "  #{:<4} @{:<8} FORM:{:<4} size={:<8}",
            index + 1,
            offset,
            form.kind,
            form.chunk.size
        );

        if let Some(info) = read_page_info(bytes, form)? {
            print!(
                " INFO {}x{} dpi={} gamma={:.1} version={} rotation={}",
                info.width, info.height, info.dpi, info.gamma, info.version, info.rotation
            );
        }

        println!();
    }

    Ok(())
}
