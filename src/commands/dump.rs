use super::{print_decoded_iw44_payload_summary, read_file};
use anyhow::{Context as _, bail};
use djvulpes::{Document, decode_dirm_tail, parse_dirm_tail};
use std::{fs, path::Path};

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
