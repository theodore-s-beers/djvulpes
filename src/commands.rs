use anyhow::{Context, bail};
use djvulpes::{
    Chunk, DirectoryEntry, Document, DocumentFormKind, Form, PageChunk, PageChunkKind,
    PageChunkPayload, PageChunkSource, PageRenderPlan, ParseResult, TextZone, parse_chunks,
    parse_dirm_tail, parse_form_at, parse_text_payload, parse_text_zones, read_page_details,
    write_bitmap_pdf,
};
use djvulpes::{decode_bzz, decode_dirm_tail};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const JB2_PLAN_PREFIX_RECORD_LIMIT: usize = 8;

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
    let render = plan.render_partial_bitmap(&bytes)?;
    let ppm = render.bitmap.to_ppm_bytes();

    fs::write(output, ppm).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!(
        "rendered: {}x{} dpi={} format=PPM/P6",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    );
    println!("output: {}", output.display());
    print_partial_render_summary(&render.bitonal_masks);
    print_pending_image_layer_summary(&plan);

    Ok(())
}

pub fn run_render_page_pdf(path: &Path, number: usize, output: &Path) -> anyhow::Result<()> {
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
    let render = plan.render_partial_bitmap(&bytes)?;
    let pdf = write_bitmap_pdf(std::slice::from_ref(&render.bitmap))?;

    fs::write(output, pdf).with_context(|| format!("failed to write {}", output.display()))?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!(
        "rendered: {}x{} dpi={} format=PDF",
        render.bitmap.width, render.bitmap.height, render.bitmap.dpi
    );
    println!("output: {}", output.display());
    print_partial_render_summary(&render.bitonal_masks);
    print_pending_image_layer_summary(&plan);

    Ok(())
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

fn print_partial_render_summary(bitonal_masks: &[(usize, djvulpes::Jb2PartialImage)]) {
    for (chunk_index, partial) in bitonal_masks {
        println!(
            "painted bitonal image #{chunk_index}: black_pixels={} dictionary_symbols={} end_of_data={} stopped_before={}",
            partial.mask.black_pixel_count(),
            partial.dictionary_symbol_count,
            partial.reached_end_of_data,
            partial
                .stopped_before
                .map_or("none", djvulpes::Jb2RecordKind::as_str)
        );
    }
}

fn print_pending_image_layer_summary(plan: &PageRenderPlan<'_>) {
    if plan.foreground_layers.is_empty() && plan.background_layers.is_empty() {
        return;
    }

    println!(
        "image layers: IW44 foreground/background pending decoder support (foreground={} background={})",
        plan.foreground_layers.len(),
        plan.background_layers.len()
    );
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
