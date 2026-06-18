#![forbid(unsafe_code)]
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(clippy::similar_names, clippy::uninlined_format_args)]

use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;

const DJVU_MAGIC: &[u8; 4] = b"AT&T";

type Result<T> = std::result::Result<T, ParseError>;

#[derive(Debug)]
struct ParseError(String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone)]
struct Chunk<'a> {
    id: &'a str,
    size: u32,
    data_start: usize,
    data_end: usize,
    next_start: usize,
}

#[derive(Debug, Clone)]
struct Form<'a> {
    chunk: Chunk<'a>,
    kind: &'a str,
    children_start: usize,
}

#[derive(Debug, Clone)]
struct Dirm {
    flags: u8,
    entry_count: u16,
    offsets: Vec<u32>,
    compressed_tail_len: usize,
}

#[derive(Debug, Clone)]
struct PageInfo {
    width: u16,
    height: u16,
    version: u8,
    dpi: u16,
    gamma: f32,
    rotation: u8,
}

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

fn input_path() -> PathBuf {
    env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("Rypka-HIL.djvu"), PathBuf::from)
}

fn parse_document_root(bytes: &[u8]) -> Result<Form<'_>> {
    require_range(bytes, 0, 4)?;
    if &bytes[0..4] != DJVU_MAGIC {
        return Err(ParseError("missing DjVu magic bytes `AT&T`".to_string()));
    }

    let root = parse_form_at(bytes, 4)?;
    match root.kind {
        "DJVU" | "DJVM" => Ok(root),
        other => Err(ParseError(format!(
            "unexpected DjVu root FORM kind `{other}`"
        ))),
    }
}

fn parse_form_at(bytes: &[u8], start: usize) -> Result<Form<'_>> {
    let chunk = parse_chunk_at(bytes, start)?;
    if chunk.id != "FORM" {
        return Err(ParseError(format!(
            "expected FORM at offset {start}, found {}",
            chunk.id
        )));
    }

    require_range(bytes, chunk.data_start, 4)?;
    let kind = ascii_tag(bytes, chunk.data_start)?;
    Ok(Form {
        children_start: chunk.data_start + 4,
        chunk,
        kind,
    })
}

fn parse_chunks(bytes: &[u8], start: usize, end: usize) -> Result<Vec<Chunk<'_>>> {
    let mut chunks = Vec::new();
    let mut cursor = start;

    while cursor < end {
        let chunk = parse_chunk_at(bytes, cursor)?;
        if chunk.data_end > end {
            return Err(ParseError(format!(
                "chunk {} at offset {cursor} extends past parent end {end}",
                chunk.id
            )));
        }

        let next_start = chunk.next_start;
        chunks.push(chunk);
        cursor = next_start;
    }

    Ok(chunks)
}

fn parse_chunk_at(bytes: &[u8], start: usize) -> Result<Chunk<'_>> {
    require_range(bytes, start, 8)?;

    let id = ascii_tag(bytes, start)?;
    let size = read_u32_be(bytes, start + 4)?;
    let data_start = start + 8;
    let data_end = checked_add(data_start, size as usize)?;
    require_range(bytes, data_start, size as usize)?;

    let next_start = checked_add(data_end, (size & 1) as usize)?;
    if next_start > bytes.len() {
        return Err(ParseError(format!(
            "chunk {id} at offset {start} has invalid padding"
        )));
    }

    Ok(Chunk {
        id,
        size,
        data_start,
        data_end,
        next_start,
    })
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
    for offset in &dirm.offsets {
        let Ok(form) = parse_form_at(bytes, *offset as usize) else {
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

fn parse_dirm(bytes: &[u8], chunk: &Chunk<'_>) -> Result<Dirm> {
    require_range(bytes, chunk.data_start, 3)?;

    let flags = bytes[chunk.data_start];
    let entry_count = read_u16_be(bytes, chunk.data_start + 1)?;
    let offsets_start = chunk.data_start + 3;
    let offsets_len = entry_count as usize * 4;
    let compressed_tail_start = checked_add(offsets_start, offsets_len)?;

    if compressed_tail_start > chunk.data_end {
        return Err(ParseError(format!(
            "DIRM declares {entry_count} entries, but the chunk is too small for their offsets"
        )));
    }

    let mut offsets = Vec::with_capacity(entry_count as usize);
    for index in 0..entry_count as usize {
        offsets.push(read_u32_be(bytes, offsets_start + index * 4)?);
    }

    Ok(Dirm {
        flags,
        entry_count,
        offsets,
        compressed_tail_len: chunk.data_end - compressed_tail_start,
    })
}

fn read_page_info(bytes: &[u8], form: &Form<'_>) -> Result<Option<PageInfo>> {
    if form.kind != "DJVU" {
        return Ok(None);
    }

    let children = parse_chunks(bytes, form.children_start, form.chunk.data_end)?;
    let Some(info_chunk) = children.first().filter(|chunk| chunk.id == "INFO") else {
        return Ok(None);
    };

    if info_chunk.size < 10 {
        return Ok(None);
    }

    let start = info_chunk.data_start;
    Ok(Some(PageInfo {
        width: read_u16_be(bytes, start)?,
        height: read_u16_be(bytes, start + 2)?,
        version: bytes[start + 4],
        dpi: read_u16_be(bytes, start + 5)?,
        gamma: f32::from(bytes[start + 8]) / 10.0,
        rotation: bytes[start + 9],
    }))
}

fn ascii_tag(bytes: &[u8], start: usize) -> Result<&str> {
    require_range(bytes, start, 4)?;
    std::str::from_utf8(&bytes[start..start + 4])
        .map_err(|_| ParseError(format!("non-ASCII chunk tag at offset {start}")))
}

fn read_u16_be(bytes: &[u8], start: usize) -> Result<u16> {
    require_range(bytes, start, 2)?;
    Ok(u16::from_be_bytes([bytes[start], bytes[start + 1]]))
}

fn read_u32_be(bytes: &[u8], start: usize) -> Result<u32> {
    require_range(bytes, start, 4)?;
    Ok(u32::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ]))
}

fn checked_add(lhs: usize, rhs: usize) -> Result<usize> {
    lhs.checked_add(rhs)
        .ok_or_else(|| ParseError("offset overflow".to_string()))
}

fn require_range(bytes: &[u8], start: usize, len: usize) -> Result<()> {
    let end = checked_add(start, len)?;
    if end > bytes.len() {
        return Err(ParseError(format!(
            "need bytes [{start}..{end}), but file only has {} bytes",
            bytes.len()
        )));
    }
    Ok(())
}
