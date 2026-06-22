use super::{Iw44PixelInspectOptions, Iw44PixelTrace, iw44_plane_name, with_page_render_plan};
use anyhow::{Context as _, bail};
use djvulpes::{PageRenderMode, PageRenderPlan};
use std::path::Path;

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44PixelInspection {
    role: djvulpes::Iw44LayerRole,
    page_x: u32,
    page_y: u32,
    source_x: usize,
    source_y: usize,
    rgb: [u8; 3],
    computed_rgb: [u8; 3],
    y: Iw44PlaneSample,
    cb: Option<Iw44PlaneSample>,
    cr: Option<Iw44PlaneSample>,
    y_neighborhood: Option<Iw44LumaNeighborhood>,
    y_coefficients: Option<Iw44CoefficientBlockSummary>,
    y_coefficient_trace: Option<Vec<Iw44CoefficientTraceStep>>,
    y_coefficient_event_traces: Option<Vec<Iw44CoefficientEventTrace>>,
    y_coefficient_reconstruction: Option<Iw44CoefficientReconstructionSummary>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44PlaneSample {
    plane: djvulpes::Iw44Plane,
    x: usize,
    y: usize,
    raw: i16,
    normalized: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44LumaNeighborhood {
    page_min_x: u32,
    page_min_y: u32,
    source_min_x: usize,
    source_min_y: usize,
    samples: Vec<Vec<Iw44PlaneSample>>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientBlockSummary {
    plane: djvulpes::Iw44Plane,
    plane_width: usize,
    block_x: usize,
    block_y: usize,
    block_col: usize,
    block_row: usize,
    width: usize,
    height: usize,
    non_zero: usize,
    max_abs: u16,
    abs_sum: u64,
    entries: Vec<Iw44CoefficientEntry>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientEntry {
    x: usize,
    y: usize,
    local_x: usize,
    local_y: usize,
    index: usize,
    bucket: usize,
    bucket_offset: usize,
    band: usize,
    value: i16,
    absolute: u16,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientTraceStep {
    chunk_index: usize,
    chunk_serial: u8,
    slice_index: Option<u32>,
    slices_decoded: u32,
    values: Vec<Iw44CoefficientEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientEventTrace {
    entry: Iw44CoefficientEntry,
    events: Vec<Iw44CoefficientTraceEvent>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientTraceEvent {
    chunk_index: usize,
    chunk_serial: u8,
    event: djvulpes::Iw44CoefficientEvent,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientReconstructionSummary {
    entries: Vec<Iw44CoefficientReconstructionTrace>,
    all_zeroed: Iw44CoefficientReconstructionAggregate,
    block_zeroed: Iw44CoefficientReconstructionAggregate,
    rows_then_columns: Iw44TransformOrderTrace,
    padded_extent: Iw44TransformOrderTrace,
    band_zeroed: Vec<Iw44CoefficientReconstructionBandAggregate>,
    bucket_zeroed: Vec<Iw44CoefficientReconstructionBucketAggregate>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44TransformOrderTrace {
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    alternate_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    alternate_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientReconstructionTrace {
    entry: Iw44CoefficientEntry,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientReconstructionAggregate {
    coefficient_count: usize,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct Iw44CoefficientReconstructionBandAggregate {
    band: usize,
    coefficient_count: usize,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientReconstructionBucketAggregate {
    bucket: usize,
    band: usize,
    coefficient_count: usize,
    sample_x: usize,
    sample_y: usize,
    original_raw: i16,
    zeroed_raw: i16,
    raw_delta: i32,
    original_normalized: i32,
    zeroed_normalized: i32,
    normalized_delta: i32,
    contributors: Vec<Iw44CoefficientEntry>,
}

struct Iw44OptionalPixelDiagnostics {
    neighborhood: Option<Iw44LumaNeighborhood>,
    coefficients: Option<Iw44CoefficientBlockSummary>,
    coefficient_trace: Option<Vec<Iw44CoefficientTraceStep>>,
    coefficient_event_traces: Option<Vec<Iw44CoefficientEventTrace>>,
    coefficient_reconstruction: Option<Iw44CoefficientReconstructionSummary>,
}

struct Iw44InspectionLayer<'a> {
    role: djvulpes::Iw44LayerRole,
    payloads: Vec<djvulpes::RenderChunkPayload<'a>>,
    geometry: djvulpes::Iw44LayerGeometry,
}

fn inspect_iw44_pixel(
    plan: &PageRenderPlan<'_>,
    bytes: &[u8],
    mode: PageRenderMode,
    options: &Iw44PixelInspectOptions,
) -> anyhow::Result<Iw44PixelInspection> {
    if options.radius > 10 {
        bail!("IW44 inspection radius must be 10 or less");
    }
    let layer = iw44_inspection_layer(plan, bytes, mode)?;
    if options.x >= layer.geometry.mapping.page_width
        || options.y >= layer.geometry.mapping.page_height
    {
        bail!(
            "page pixel x={} y={} is outside page {}x{}",
            options.x,
            options.y,
            layer.geometry.mapping.page_width,
            layer.geometry.mapping.page_height
        );
    }

    let decoder = decode_iw44_payloads(&layer.payloads)?;
    let image = decoder.to_rgb_image()?;
    let planes = decoder.reconstruct_planes();
    let source_x = iw44_source_coordinate(
        options.x,
        layer.geometry.mapping.horizontal_overscan,
        layer.geometry.mapping.subsample,
        image.width,
    );
    let source_y = iw44_source_coordinate(
        options.y,
        layer.geometry.mapping.vertical_overscan,
        layer.geometry.mapping.subsample,
        image.height,
    );
    let rgb_offset = (source_y * image.width + source_x) * 3;
    let rgb = [
        image.pixels[rgb_offset],
        image.pixels[rgb_offset + 1],
        image.pixels[rgb_offset + 2],
    ];
    let image_header = decoder
        .image()
        .context("IW44 decoder did not produce image metadata")?;
    let y_sample = iw44_plane_sample(&planes, djvulpes::Iw44Plane::Y, source_x, source_y, false)?;
    let blue_chroma_sample = if image_header.grayscale {
        None
    } else {
        Some(iw44_plane_sample(
            &planes,
            djvulpes::Iw44Plane::Cb,
            source_x,
            source_y,
            image_header.chroma_half,
        )?)
    };
    let red_chroma_sample = if image_header.grayscale {
        None
    } else {
        Some(iw44_plane_sample(
            &planes,
            djvulpes::Iw44Plane::Cr,
            source_x,
            source_y,
            image_header.chroma_half,
        )?)
    };
    let computed_rgb = iw44_samples_to_rgb(y_sample, blue_chroma_sample, red_chroma_sample);
    let diagnostics =
        iw44_optional_pixel_diagnostics(&layer, &decoder, &planes, &image, y_sample, options)?;

    Ok(Iw44PixelInspection {
        role: layer.role,
        page_x: options.x,
        page_y: options.y,
        source_x,
        source_y,
        rgb,
        computed_rgb,
        y: y_sample,
        cb: blue_chroma_sample,
        cr: red_chroma_sample,
        y_neighborhood: diagnostics.neighborhood,
        y_coefficients: diagnostics.coefficients,
        y_coefficient_trace: diagnostics.coefficient_trace,
        y_coefficient_event_traces: diagnostics.coefficient_event_traces,
        y_coefficient_reconstruction: diagnostics.coefficient_reconstruction,
    })
}

fn iw44_optional_pixel_diagnostics(
    layer: &Iw44InspectionLayer<'_>,
    decoder: &djvulpes::Iw44Decoder,
    planes: &[djvulpes::Iw44ReconstructionPlane],
    image: &djvulpes::Iw44RgbImage,
    y_sample: Iw44PlaneSample,
    options: &Iw44PixelInspectOptions,
) -> anyhow::Result<Iw44OptionalPixelDiagnostics> {
    let y_neighborhood = if options.radius == 0 {
        None
    } else {
        Some(iw44_luma_neighborhood(
            planes,
            &layer.geometry.mapping,
            image.width,
            image.height,
            options.x,
            options.y,
            options.radius,
        )?)
    };
    let y_coefficients = if options.coefficient_limit == 0 && options.coefficient_indices.is_empty()
    {
        None
    } else {
        Some(iw44_coefficient_block_summary(
            decoder,
            y_sample,
            options.coefficient_limit,
            &options.coefficient_indices,
        )?)
    };
    let y_coefficient_trace = if options.traces.contains(&Iw44PixelTrace::Coefficients) {
        let Some(coefficients) = &y_coefficients else {
            bail!("--trace-coefficients requires --coefficients or --coefficient-index");
        };
        Some(iw44_coefficient_trace(
            &layer.payloads,
            coefficients.plane,
            &coefficients.entries,
            options.traces.contains(&Iw44PixelTrace::Slices),
        )?)
    } else {
        None
    };
    let y_coefficient_event_traces = if options.traces.contains(&Iw44PixelTrace::Events) {
        let Some(coefficients) = &y_coefficients else {
            bail!("--trace-events requires --coefficients or --coefficient-index");
        };
        Some(iw44_coefficient_event_traces(
            &layer.payloads,
            coefficients.plane,
            coefficients.plane_width,
            coefficients.block_col,
            coefficients.block_row,
            &coefficients.entries,
        )?)
    } else {
        None
    };
    let y_coefficient_reconstruction = if options.traces.contains(&Iw44PixelTrace::Reconstruction) {
        let Some(coefficients) = &y_coefficients else {
            bail!("--trace-reconstruction requires --coefficients or --coefficient-index");
        };
        Some(iw44_coefficient_reconstruction_trace(
            decoder,
            coefficients,
            y_sample,
        )?)
    } else {
        None
    };

    Ok(Iw44OptionalPixelDiagnostics {
        neighborhood: y_neighborhood,
        coefficients: y_coefficients,
        coefficient_trace: y_coefficient_trace,
        coefficient_event_traces: y_coefficient_event_traces,
        coefficient_reconstruction: y_coefficient_reconstruction,
    })
}

fn iw44_inspection_layer<'a>(
    plan: &PageRenderPlan<'_>,
    bytes: &'a [u8],
    mode: PageRenderMode,
) -> anyhow::Result<Iw44InspectionLayer<'a>> {
    let (role, payloads, geometry) = match mode {
        PageRenderMode::Background => (
            djvulpes::Iw44LayerRole::Background,
            plan.background_layer_payloads(bytes),
            plan.background_layer_geometry(bytes)?,
        ),
        PageRenderMode::Foreground => (
            djvulpes::Iw44LayerRole::Foreground,
            plan.foreground_layer_payloads(bytes),
            plan.foreground_layer_geometry(bytes)?,
        ),
        PageRenderMode::Full | PageRenderMode::Mask => {
            bail!("IW44 pixel inspection requires background or foreground mode")
        }
    };
    let geometry = geometry.with_context(|| format!("{} IW44 layer not found", mode.as_str()))?;

    Ok(Iw44InspectionLayer {
        role,
        payloads,
        geometry,
    })
}

fn decode_iw44_payloads(
    payloads: &[djvulpes::RenderChunkPayload<'_>],
) -> anyhow::Result<djvulpes::Iw44Decoder> {
    let mut decoder = djvulpes::Iw44Decoder::new();
    for payload in payloads {
        decoder
            .decode_chunk(payload.bytes)
            .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
    }

    Ok(decoder)
}

fn iw44_source_coordinate(
    page_coordinate: u32,
    overscan: u32,
    subsample: u32,
    source_extent: usize,
) -> usize {
    let centered = page_coordinate.saturating_add(overscan / 2);
    let scaled = centered / subsample.max(1);
    (scaled as usize).min(source_extent.saturating_sub(1))
}

fn iw44_plane_sample(
    planes: &[djvulpes::Iw44ReconstructionPlane],
    plane: djvulpes::Iw44Plane,
    source_x: usize,
    source_y: usize,
    chroma_half: bool,
) -> anyhow::Result<Iw44PlaneSample> {
    let plane_data = planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} reconstruction plane", iw44_plane_name(plane)))?;
    let sample_y = plane_data.height - 1 - source_y.min(plane_data.height - 1);
    let sample_x = source_x.min(plane_data.width - 1);
    let (sample_x, sample_y) = if chroma_half {
        (sample_x / 2, sample_y / 2)
    } else {
        (sample_x, sample_y)
    };
    let raw = plane_data.samples[sample_y * plane_data.width + sample_x];

    Ok(Iw44PlaneSample {
        plane,
        x: sample_x,
        y: sample_y,
        raw,
        normalized: normalize_iw44_sample(raw),
    })
}

fn iw44_luma_neighborhood(
    planes: &[djvulpes::Iw44ReconstructionPlane],
    mapping: &djvulpes::Iw44PageMapping,
    image_width: usize,
    image_height: usize,
    x: u32,
    y: u32,
    radius: u8,
) -> anyhow::Result<Iw44LumaNeighborhood> {
    let radius = u32::from(radius);
    let min_page_x = x.saturating_sub(radius);
    let min_page_y = y.saturating_sub(radius);
    let max_page_x = x.saturating_add(radius).min(mapping.page_width - 1);
    let max_page_y = y.saturating_add(radius).min(mapping.page_height - 1);
    let source_min_x = iw44_source_coordinate(
        min_page_x,
        mapping.horizontal_overscan,
        mapping.subsample,
        image_width,
    );
    let source_min_y = iw44_source_coordinate(
        min_page_y,
        mapping.vertical_overscan,
        mapping.subsample,
        image_height,
    );
    let mut rows = Vec::new();

    for page_y in min_page_y..=max_page_y {
        let source_y = iw44_source_coordinate(
            page_y,
            mapping.vertical_overscan,
            mapping.subsample,
            image_height,
        );
        let mut row = Vec::new();
        for page_x in min_page_x..=max_page_x {
            let source_x = iw44_source_coordinate(
                page_x,
                mapping.horizontal_overscan,
                mapping.subsample,
                image_width,
            );
            row.push(iw44_plane_sample(
                planes,
                djvulpes::Iw44Plane::Y,
                source_x,
                source_y,
                false,
            )?);
        }
        rows.push(row);
    }

    Ok(Iw44LumaNeighborhood {
        page_min_x: min_page_x,
        page_min_y: min_page_y,
        source_min_x,
        source_min_y,
        samples: rows,
    })
}

fn iw44_coefficient_block_summary(
    decoder: &djvulpes::Iw44Decoder,
    sample: Iw44PlaneSample,
    entry_limit: usize,
    selected_indices: &[usize],
) -> anyhow::Result<Iw44CoefficientBlockSummary> {
    let coefficient_planes = decoder.coefficient_planes();
    let plane = coefficient_planes
        .iter()
        .find(|plane| plane.plane == sample.plane)
        .with_context(|| {
            format!(
                "missing {} coefficient plane",
                iw44_plane_name(sample.plane)
            )
        })?;
    let block_x = (sample.x / 32) * 32;
    let block_y = (sample.y / 32) * 32;
    let width = 32.min(plane.width - block_x);
    let height = 32.min(plane.height - block_y);
    let mut entries = Vec::new();
    let mut non_zero = 0usize;
    let mut max_abs = 0u16;
    let mut abs_sum = 0u64;

    for y in block_y..block_y + height {
        for x in block_x..block_x + width {
            let value = plane.coefficients[y * plane.width + x];
            let absolute = value.unsigned_abs();
            if absolute == 0 {
                continue;
            }

            non_zero += 1;
            max_abs = max_abs.max(absolute);
            abs_sum += u64::from(absolute);
            entries.push(iw44_coefficient_entry(block_x, block_y, x, y, value));
        }
    }

    entries.sort_by(|left, right| {
        right
            .absolute
            .cmp(&left.absolute)
            .then_with(|| left.y.cmp(&right.y))
            .then_with(|| left.x.cmp(&right.x))
    });
    entries.truncate(entry_limit);
    for &index in selected_indices {
        if index >= 1024 {
            bail!("IW44 coefficient index {index} is outside a 32x32 block");
        }
        if entries.iter().any(|entry| entry.index == index) {
            continue;
        }
        let x = block_x + iw44_zigzag_col(index);
        let y = block_y + iw44_zigzag_row(index);
        if x >= plane.width || y >= plane.height {
            bail!("IW44 coefficient index {index} is outside the edge block");
        }
        let value = plane.coefficients[y * plane.width + x];
        entries.push(iw44_coefficient_entry(block_x, block_y, x, y, value));
    }

    Ok(Iw44CoefficientBlockSummary {
        plane: sample.plane,
        plane_width: plane.width,
        block_x,
        block_y,
        block_col: block_x / 32,
        block_row: block_y / 32,
        width,
        height,
        non_zero,
        max_abs,
        abs_sum,
        entries,
    })
}

fn iw44_coefficient_entry(
    block_x: usize,
    block_y: usize,
    x: usize,
    y: usize,
    value: i16,
) -> Iw44CoefficientEntry {
    let local_x = x - block_x;
    let local_y = y - block_y;
    let index = iw44_inverse_zigzag_index(local_x, local_y);
    let bucket = index / 16;
    let bucket_offset = index % 16;

    Iw44CoefficientEntry {
        x,
        y,
        local_x,
        local_y,
        index,
        bucket,
        bucket_offset,
        band: iw44_bucket_band(bucket),
        value,
        absolute: value.unsigned_abs(),
    }
}

fn iw44_inverse_zigzag_index(local_x: usize, local_y: usize) -> usize {
    let mut index = 0usize;
    for bit in 0..5 {
        index |= ((local_x >> (4 - bit)) & 1) << (bit * 2);
        index |= ((local_y >> (4 - bit)) & 1) << (bit * 2 + 1);
    }
    index
}

fn iw44_zigzag_col(index: usize) -> usize {
    let mut col = 0usize;
    for bit in 0..5 {
        col |= ((index >> (bit * 2)) & 1) << (4 - bit);
    }
    col
}

fn iw44_zigzag_row(index: usize) -> usize {
    let mut row = 0usize;
    for bit in 0..5 {
        row |= ((index >> ((bit * 2) + 1)) & 1) << (4 - bit);
    }
    row
}

fn iw44_coefficient_value(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    x: usize,
    y: usize,
) -> anyhow::Result<i16> {
    let coefficient_planes = decoder.coefficient_planes();
    let plane_data = coefficient_planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} coefficient plane", iw44_plane_name(plane)))?;

    Ok(plane_data.coefficients[y * plane_data.width + x])
}

const fn iw44_bucket_band(bucket: usize) -> usize {
    match bucket {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=7 => 4,
        8..=11 => 5,
        12..=15 => 6,
        16..=31 => 7,
        32..=47 => 8,
        48..=63 => 9,
        _ => 10,
    }
}

fn iw44_coefficient_trace(
    payloads: &[djvulpes::RenderChunkPayload<'_>],
    plane: djvulpes::Iw44Plane,
    entries: &[Iw44CoefficientEntry],
    trace_slices: bool,
) -> anyhow::Result<Vec<Iw44CoefficientTraceStep>> {
    let mut decoder = djvulpes::Iw44Decoder::new();
    let mut trace = Vec::with_capacity(payloads.len());

    for payload in payloads {
        if trace_slices {
            let header = djvulpes::read_iw44_chunk_header(payload.bytes)
                .with_context(|| format!("failed to read IW44 chunk #{} header", payload.index))?;
            let mut trace_error = None;
            decoder
                .decode_chunk_with_slice_observer(payload.bytes, |decoder, slice| {
                    match iw44_trace_values(decoder, plane, entries) {
                        Ok(values) => trace.push(Iw44CoefficientTraceStep {
                            chunk_index: payload.index,
                            chunk_serial: header.serial,
                            slice_index: Some(slice.index),
                            slices_decoded: decoder.slices_decoded(),
                            values,
                        }),
                        Err(error) => trace_error = Some(error),
                    }
                })
                .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
            if let Some(error) = trace_error {
                return Err(error);
            }
        } else {
            let chunk = decoder
                .decode_chunk(payload.bytes)
                .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
            trace.push(Iw44CoefficientTraceStep {
                chunk_index: payload.index,
                chunk_serial: chunk.header.serial,
                slice_index: None,
                slices_decoded: decoder.slices_decoded(),
                values: iw44_trace_values(&decoder, plane, entries)?,
            });
        }
    }

    Ok(trace)
}

fn iw44_coefficient_event_traces(
    payloads: &[djvulpes::RenderChunkPayload<'_>],
    plane: djvulpes::Iw44Plane,
    plane_width: usize,
    block_col: usize,
    block_row: usize,
    entries: &[Iw44CoefficientEntry],
) -> anyhow::Result<Vec<Iw44CoefficientEventTrace>> {
    let block_columns = plane_width.div_ceil(32);
    let block = block_row * block_columns + block_col;
    let mut traces = Vec::with_capacity(entries.len());

    for entry in entries {
        let mut decoder = djvulpes::Iw44Decoder::new();
        let mut events = Vec::new();
        let target = djvulpes::Iw44CoefficientTraceTarget {
            plane,
            block,
            coefficient: entry.index,
        };

        for payload in payloads {
            let header = djvulpes::read_iw44_chunk_header(payload.bytes)
                .with_context(|| format!("failed to read IW44 chunk #{} header", payload.index))?;
            decoder
                .decode_chunk_with_coefficient_observer(payload.bytes, target, |event| {
                    events.push(Iw44CoefficientTraceEvent {
                        chunk_index: payload.index,
                        chunk_serial: header.serial,
                        event,
                    });
                })
                .with_context(|| format!("failed to decode IW44 chunk #{}", payload.index))?;
        }

        traces.push(Iw44CoefficientEventTrace {
            entry: *entry,
            events,
        });
    }

    Ok(traces)
}

fn iw44_coefficient_reconstruction_trace(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Iw44CoefficientReconstructionSummary> {
    let block_columns = coefficients.plane_width.div_ceil(32);
    let block = coefficients.block_row * block_columns + coefficients.block_col;
    let overrides = coefficients
        .entries
        .iter()
        .map(|entry| (block, entry.index, 0))
        .collect::<Vec<_>>();
    let block_overrides = (0..1024)
        .map(|coefficient| (block, coefficient, 0))
        .collect::<Vec<_>>();
    let entries =
        iw44_individual_coefficient_reconstruction_traces(decoder, coefficients, block, sample)?;
    let band_zeroed = iw44_band_zeroed_reconstruction(decoder, coefficients, block, sample)?;
    let bucket_zeroed =
        iw44_bucket_zeroed_reconstruction(decoder, coefficients, block, sample, &band_zeroed)?;

    Ok(Iw44CoefficientReconstructionSummary {
        entries,
        all_zeroed: iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &overrides,
            coefficients.entries.len(),
            sample,
            "listed coefficients",
        )?,
        block_zeroed: iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &block_overrides,
            1024,
            sample,
            "containing coefficient block",
        )?,
        rows_then_columns: iw44_transform_order_trace(decoder, coefficients.plane, sample)?,
        padded_extent: iw44_padded_extent_trace(decoder, coefficients.plane, sample)?,
        band_zeroed,
        bucket_zeroed,
    })
}

fn iw44_transform_order_trace(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Iw44TransformOrderTrace> {
    let planes =
        decoder.reconstruct_planes_with_order(djvulpes::Iw44ReconstructionOrder::RowsThenColumns);
    let alternate = planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} reconstruction plane", iw44_plane_name(plane)))?;
    let alternate_raw = alternate.samples[sample.y * alternate.width + sample.x];
    let alternate_normalized = normalize_iw44_sample(alternate_raw);

    Ok(Iw44TransformOrderTrace {
        sample_x: sample.x,
        sample_y: sample.y,
        original_raw: sample.raw,
        alternate_raw,
        raw_delta: i32::from(sample.raw) - i32::from(alternate_raw),
        original_normalized: sample.normalized,
        alternate_normalized,
        normalized_delta: sample.normalized - alternate_normalized,
    })
}

fn iw44_padded_extent_trace(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Iw44TransformOrderTrace> {
    let planes = decoder.reconstruct_planes_with_options(
        djvulpes::Iw44ReconstructionOrder::ColumnsThenRows,
        djvulpes::Iw44ReconstructionExtent::Padded,
    );
    let alternate = planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} reconstruction plane", iw44_plane_name(plane)))?;
    let alternate_raw = alternate.samples[sample.y * alternate.width + sample.x];
    let alternate_normalized = normalize_iw44_sample(alternate_raw);

    Ok(Iw44TransformOrderTrace {
        sample_x: sample.x,
        sample_y: sample.y,
        original_raw: sample.raw,
        alternate_raw,
        raw_delta: i32::from(sample.raw) - i32::from(alternate_raw),
        original_normalized: sample.normalized,
        alternate_normalized,
        normalized_delta: sample.normalized - alternate_normalized,
    })
}

fn iw44_individual_coefficient_reconstruction_traces(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    block: usize,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Vec<Iw44CoefficientReconstructionTrace>> {
    let mut traces = Vec::with_capacity(coefficients.entries.len());
    for entry in &coefficients.entries {
        let aggregate = iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &[(block, entry.index, 0)],
            1,
            sample,
            "coefficient",
        )?;
        traces.push(Iw44CoefficientReconstructionTrace {
            entry: *entry,
            sample_x: aggregate.sample_x,
            sample_y: aggregate.sample_y,
            original_raw: aggregate.original_raw,
            zeroed_raw: aggregate.zeroed_raw,
            raw_delta: aggregate.raw_delta,
            original_normalized: aggregate.original_normalized,
            zeroed_normalized: aggregate.zeroed_normalized,
            normalized_delta: aggregate.normalized_delta,
        });
    }

    Ok(traces)
}

fn iw44_band_zeroed_reconstruction(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    block: usize,
    sample: Iw44PlaneSample,
) -> anyhow::Result<Vec<Iw44CoefficientReconstructionBandAggregate>> {
    let mut bands = Vec::new();
    for band in 0..=9 {
        let overrides = (0..1024)
            .filter(|coefficient| iw44_bucket_band(coefficient / 16) == band)
            .map(|coefficient| (block, coefficient, 0))
            .collect::<Vec<_>>();
        if overrides.is_empty() {
            continue;
        }

        let aggregate = iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &overrides,
            overrides.len(),
            sample,
            "band",
        )?;
        bands.push(Iw44CoefficientReconstructionBandAggregate {
            band,
            coefficient_count: aggregate.coefficient_count,
            sample_x: aggregate.sample_x,
            sample_y: aggregate.sample_y,
            original_raw: aggregate.original_raw,
            zeroed_raw: aggregate.zeroed_raw,
            raw_delta: aggregate.raw_delta,
            original_normalized: aggregate.original_normalized,
            zeroed_normalized: aggregate.zeroed_normalized,
            normalized_delta: aggregate.normalized_delta,
        });
    }

    Ok(bands)
}

fn iw44_bucket_zeroed_reconstruction(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    block: usize,
    sample: Iw44PlaneSample,
    band_zeroed: &[Iw44CoefficientReconstructionBandAggregate],
) -> anyhow::Result<Vec<Iw44CoefficientReconstructionBucketAggregate>> {
    let high_impact_bands = band_zeroed
        .iter()
        .filter(|band| band.normalized_delta.abs() >= 10)
        .map(|band| band.band)
        .collect::<Vec<_>>();
    let mut buckets = Vec::new();
    for bucket in 0..64 {
        if !high_impact_bands.contains(&iw44_bucket_band(bucket)) {
            continue;
        }

        let overrides = (0..16)
            .map(|offset| (block, (bucket * 16) + offset, 0))
            .collect::<Vec<_>>();
        let aggregate = iw44_reconstruction_aggregate(
            decoder,
            coefficients.plane,
            &overrides,
            16,
            sample,
            "bucket",
        )?;
        if aggregate.raw_delta == 0 && aggregate.normalized_delta == 0 {
            continue;
        }
        buckets.push(Iw44CoefficientReconstructionBucketAggregate {
            bucket,
            band: iw44_bucket_band(bucket),
            coefficient_count: aggregate.coefficient_count,
            sample_x: aggregate.sample_x,
            sample_y: aggregate.sample_y,
            original_raw: aggregate.original_raw,
            zeroed_raw: aggregate.zeroed_raw,
            raw_delta: aggregate.raw_delta,
            original_normalized: aggregate.original_normalized,
            zeroed_normalized: aggregate.zeroed_normalized,
            normalized_delta: aggregate.normalized_delta,
            contributors: Vec::new(),
        });
    }
    buckets.sort_by_key(|bucket| std::cmp::Reverse(bucket.raw_delta.unsigned_abs()));
    for bucket in buckets.iter_mut().take(3) {
        bucket.contributors =
            iw44_bucket_coefficient_contributors(decoder, coefficients, bucket.bucket)?;
    }

    Ok(buckets)
}

fn iw44_bucket_coefficient_contributors(
    decoder: &djvulpes::Iw44Decoder,
    coefficients: &Iw44CoefficientBlockSummary,
    bucket: usize,
) -> anyhow::Result<Vec<Iw44CoefficientEntry>> {
    let mut entries = Vec::new();
    for offset in 0..16 {
        let coefficient = (bucket * 16) + offset;
        let x = coefficients.block_x + iw44_zigzag_col(coefficient);
        let y = coefficients.block_y + iw44_zigzag_row(coefficient);
        let value = iw44_coefficient_value(decoder, coefficients.plane, x, y)?;
        if value == 0 {
            continue;
        }
        entries.push(iw44_coefficient_entry(
            coefficients.block_x,
            coefficients.block_y,
            x,
            y,
            value,
        ));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.absolute));
    entries.truncate(4);

    Ok(entries)
}

fn iw44_reconstruction_aggregate(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    overrides: &[(usize, usize, i16)],
    coefficient_count: usize,
    sample: Iw44PlaneSample,
    label: &str,
) -> anyhow::Result<Iw44CoefficientReconstructionAggregate> {
    let zeroed = decoder
        .reconstruct_plane_with_coefficient_values(plane, overrides)
        .with_context(|| {
            format!(
                "failed to reconstruct {} plane with {label} zeroed",
                iw44_plane_name(plane)
            )
        })?;
    let zeroed_raw = zeroed.samples[sample.y * zeroed.width + sample.x];
    let zeroed_normalized = normalize_iw44_sample(zeroed_raw);

    Ok(Iw44CoefficientReconstructionAggregate {
        coefficient_count,
        sample_x: sample.x,
        sample_y: sample.y,
        original_raw: sample.raw,
        zeroed_raw,
        raw_delta: i32::from(sample.raw) - i32::from(zeroed_raw),
        original_normalized: sample.normalized,
        zeroed_normalized,
        normalized_delta: sample.normalized - zeroed_normalized,
    })
}

fn iw44_trace_values(
    decoder: &djvulpes::Iw44Decoder,
    plane: djvulpes::Iw44Plane,
    entries: &[Iw44CoefficientEntry],
) -> anyhow::Result<Vec<Iw44CoefficientEntry>> {
    let coefficient_planes = decoder.coefficient_planes();
    let plane_data = coefficient_planes
        .iter()
        .find(|candidate| candidate.plane == plane)
        .with_context(|| format!("missing {} coefficient plane", iw44_plane_name(plane)))?;

    Ok(entries
        .iter()
        .map(|entry| {
            let value = plane_data.coefficients[entry.y * plane_data.width + entry.x];
            Iw44CoefficientEntry {
                value,
                absolute: value.unsigned_abs(),
                ..*entry
            }
        })
        .collect())
}

fn iw44_samples_to_rgb(
    y: Iw44PlaneSample,
    cb: Option<Iw44PlaneSample>,
    cr: Option<Iw44PlaneSample>,
) -> [u8; 3] {
    let Some(cb) = cb else {
        let value = clamp_u8(127 - y.normalized);
        return [value, value, value];
    };
    let cr = cr.expect("Cb and Cr samples should both exist for color IW44");
    ycbcr_pixel_to_rgb(y.normalized, cb.normalized, cr.normalized)
}

fn normalize_iw44_sample(value: i16) -> i32 {
    ((i32::from(value) + 32) >> 6).clamp(-128, 127)
}

fn ycbcr_pixel_to_rgb(y: i32, cb: i32, cr: i32) -> [u8; 3] {
    let t2 = cr + (cr >> 1);
    let t3 = y + 128 - (cb >> 2);
    [
        clamp_u8(y + 128 + t2),
        clamp_u8(t3 - (t2 >> 1)),
        clamp_u8(t3 + (cb << 1)),
    ]
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).expect("clamped RGB component should fit u8")
}

fn print_iw44_pixel_inspection(inspection: &Iw44PixelInspection) {
    let role = match inspection.role {
        djvulpes::Iw44LayerRole::Foreground => "foreground",
        djvulpes::Iw44LayerRole::Background => "background",
    };
    println!("IW44 role: {role}");
    println!(
        "source pixel: x={} y={}",
        inspection.source_x, inspection.source_y
    );
    println!(
        "rgb: actual=#{:02x}{:02x}{:02x} computed=#{:02x}{:02x}{:02x}",
        inspection.rgb[0],
        inspection.rgb[1],
        inspection.rgb[2],
        inspection.computed_rgb[0],
        inspection.computed_rgb[1],
        inspection.computed_rgb[2]
    );
    print_iw44_plane_sample("Y", inspection.y);
    if let Some(cb) = inspection.cb {
        print_iw44_plane_sample("Cb", cb);
    }
    if let Some(cr) = inspection.cr {
        print_iw44_plane_sample("Cr", cr);
    }
    if let Some(neighborhood) = &inspection.y_neighborhood {
        print_iw44_luma_neighborhood(neighborhood);
    }
    if let Some(coefficients) = &inspection.y_coefficients {
        print_iw44_coefficient_block_summary(coefficients);
    }
    if let Some(trace) = &inspection.y_coefficient_trace {
        print_iw44_coefficient_trace(trace);
    }
    if let Some(event_traces) = &inspection.y_coefficient_event_traces {
        print_iw44_coefficient_event_traces(event_traces);
    }
    if let Some(reconstruction) = &inspection.y_coefficient_reconstruction {
        print_iw44_coefficient_reconstruction_trace(reconstruction);
    }
}

fn print_iw44_plane_sample(label: &str, sample: Iw44PlaneSample) {
    println!(
        "{label}: sample_x={} sample_y={} raw={} normalized={}",
        sample.x, sample.y, sample.raw, sample.normalized
    );
}

fn print_iw44_luma_neighborhood(neighborhood: &Iw44LumaNeighborhood) {
    println!(
        "Y neighborhood: page_origin={}x{} source_origin={}x{} width={} height={}",
        neighborhood.page_min_x,
        neighborhood.page_min_y,
        neighborhood.source_min_x,
        neighborhood.source_min_y,
        neighborhood.samples.first().map_or(0, Vec::len),
        neighborhood.samples.len()
    );
    println!("Y normalized:");
    for row in &neighborhood.samples {
        for sample in row {
            print!("{:>5}", sample.normalized);
        }
        println!();
    }
    println!("Y raw:");
    for row in &neighborhood.samples {
        for sample in row {
            print!("{:>7}", sample.raw);
        }
        println!();
    }
}

fn print_iw44_coefficient_block_summary(summary: &Iw44CoefficientBlockSummary) {
    println!(
        "{} coefficient block: block={}x{} origin={}x{} size={}x{} non_zero={} max_abs={} abs_sum={}",
        iw44_plane_name(summary.plane),
        summary.block_col,
        summary.block_row,
        summary.block_x,
        summary.block_y,
        summary.width,
        summary.height,
        summary.non_zero,
        summary.max_abs,
        summary.abs_sum
    );
    for entry in &summary.entries {
        println!(
            "  coefficient: x={} y={} local={}x{} index={} bucket={} offset={} band={} value={} abs={}",
            entry.x,
            entry.y,
            entry.local_x,
            entry.local_y,
            entry.index,
            entry.bucket,
            entry.bucket_offset,
            entry.band,
            entry.value,
            entry.absolute
        );
    }
}

fn print_iw44_coefficient_trace(trace: &[Iw44CoefficientTraceStep]) {
    println!("Y coefficient trace:");
    for step in trace {
        if let Some(slice_index) = step.slice_index {
            print!(
                "  chunk #{} serial={} slice={} slices={}",
                step.chunk_index, step.chunk_serial, slice_index, step.slices_decoded
            );
        } else {
            print!(
                "  chunk #{} serial={} slices={}",
                step.chunk_index, step.chunk_serial, step.slices_decoded
            );
        }
        for value in &step.values {
            print!(
                " index={} bucket={} band={} value={}",
                value.index, value.bucket, value.band, value.value
            );
        }
        println!();
    }
}

fn print_iw44_coefficient_event_traces(traces: &[Iw44CoefficientEventTrace]) {
    println!("Y coefficient event trace:");
    for trace in traces {
        println!(
            "  index={} bucket={} band={} x={} y={} events={}",
            trace.entry.index,
            trace.entry.bucket,
            trace.entry.band,
            trace.entry.x,
            trace.entry.y,
            trace.events.len()
        );
        for trace_event in &trace.events {
            let event = trace_event.event;
            print!(
                "    chunk #{} serial={} slice={} band={} q={} before={} after={}",
                trace_event.chunk_index,
                trace_event.chunk_serial,
                event.slice_index,
                event.band,
                event.quant,
                event.before,
                event.after
            );
            print_iw44_coefficient_event_kind(event.kind);
            println!();
        }
    }
}

fn print_iw44_coefficient_event_kind(kind: djvulpes::Iw44CoefficientEventKind) {
    match kind {
        djvulpes::Iw44CoefficientEventKind::BucketDecision {
            context,
            decision,
            block_state,
            bucket_state,
        } => print!(
            " bucket context={context} decision={decision} block_state={block_state:#04x} bucket_state={bucket_state:#04x}"
        ),
        djvulpes::Iw44CoefficientEventKind::ActivationDecision {
            context,
            decision,
            unknown_count,
        } => print!(
            " activation context={context} decision={decision} unknown_count={unknown_count}"
        ),
        djvulpes::Iw44CoefficientEventKind::Activated { sign } => {
            print!(" activated sign={sign}");
        }
        djvulpes::Iw44CoefficientEventKind::RefinementDecision {
            context_coded,
            decision,
            magnitude,
        } => print!(
            " refinement context_coded={context_coded} decision={decision} magnitude={magnitude}"
        ),
        djvulpes::Iw44CoefficientEventKind::Refined => print!(" refined"),
    }
}

fn print_iw44_coefficient_reconstruction_trace(summary: &Iw44CoefficientReconstructionSummary) {
    println!("Y coefficient reconstruction trace:");
    print_iw44_reconstruction_aggregate("all_listed_zeroed", summary.all_zeroed);
    print_iw44_reconstruction_aggregate("block_zeroed", summary.block_zeroed);
    print_iw44_transform_variant("rows_then_columns", summary.rows_then_columns);
    print_iw44_transform_variant("padded_extent", summary.padded_extent);
    print_iw44_band_reconstruction_trace(&summary.band_zeroed);
    print_iw44_bucket_reconstruction_trace(&summary.bucket_zeroed);
    print_iw44_individual_reconstruction_trace(&summary.entries);
}

fn print_iw44_reconstruction_aggregate(
    label: &str,
    aggregate: Iw44CoefficientReconstructionAggregate,
) {
    println!(
        "  {label} count={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
        aggregate.coefficient_count,
        aggregate.sample_x,
        aggregate.sample_y,
        aggregate.original_raw,
        aggregate.zeroed_raw,
        aggregate.raw_delta,
        aggregate.original_normalized,
        aggregate.zeroed_normalized,
        aggregate.normalized_delta
    );
}

fn print_iw44_transform_variant(label: &str, trace: Iw44TransformOrderTrace) {
    println!(
        "  {label} sample={}x{} original_raw={} alternate_raw={} raw_delta={} original_norm={} alternate_norm={} norm_delta={}",
        trace.sample_x,
        trace.sample_y,
        trace.original_raw,
        trace.alternate_raw,
        trace.raw_delta,
        trace.original_normalized,
        trace.alternate_normalized,
        trace.normalized_delta
    );
}

fn print_iw44_band_reconstruction_trace(bands: &[Iw44CoefficientReconstructionBandAggregate]) {
    for band in bands {
        println!(
            "  band_zeroed band={} count={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
            band.band,
            band.coefficient_count,
            band.sample_x,
            band.sample_y,
            band.original_raw,
            band.zeroed_raw,
            band.raw_delta,
            band.original_normalized,
            band.zeroed_normalized,
            band.normalized_delta
        );
    }
}

fn print_iw44_bucket_reconstruction_trace(
    buckets: &[Iw44CoefficientReconstructionBucketAggregate],
) {
    let mut buckets = buckets.to_vec();
    buckets.sort_by_key(|bucket| std::cmp::Reverse(bucket.raw_delta.unsigned_abs()));
    for bucket in &buckets {
        println!(
            "  bucket_zeroed bucket={} band={} count={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
            bucket.bucket,
            bucket.band,
            bucket.coefficient_count,
            bucket.sample_x,
            bucket.sample_y,
            bucket.original_raw,
            bucket.zeroed_raw,
            bucket.raw_delta,
            bucket.original_normalized,
            bucket.zeroed_normalized,
            bucket.normalized_delta
        );
        for contributor in &bucket.contributors {
            println!(
                "    contributor index={} offset={} x={} y={} value={} abs={}",
                contributor.index,
                contributor.bucket_offset,
                contributor.x,
                contributor.y,
                contributor.value,
                contributor.absolute
            );
        }
    }
}

fn print_iw44_individual_reconstruction_trace(traces: &[Iw44CoefficientReconstructionTrace]) {
    for trace in traces {
        println!(
            "  index={} bucket={} band={} coefficient={} sample={}x{} original_raw={} zeroed_raw={} raw_delta={} original_norm={} zeroed_norm={} norm_delta={}",
            trace.entry.index,
            trace.entry.bucket,
            trace.entry.band,
            trace.entry.value,
            trace.sample_x,
            trace.sample_y,
            trace.original_raw,
            trace.zeroed_raw,
            trace.raw_delta,
            trace.original_normalized,
            trace.zeroed_normalized,
            trace.normalized_delta
        );
    }
}

pub fn run_inspect_iw44_pixel(
    path: &Path,
    number: usize,
    mode: PageRenderMode,
    options: &Iw44PixelInspectOptions,
) -> anyhow::Result<()> {
    let inspection = with_page_render_plan(path, number, |bytes, plan| {
        inspect_iw44_pixel(&plan, bytes, mode, options)
    })?;

    println!("file: {}", path.display());
    println!("page: {number}");
    println!("mode: {}", mode.as_str());
    println!("page pixel: x={} y={}", options.x, options.y);
    print_iw44_pixel_inspection(&inspection);

    Ok(())
}
