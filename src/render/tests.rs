use super::*;
use crate::chunk::Chunk;
use crate::document::{PageChunkSource, ResolvedPageChunk};
use crate::page::{PageChunk, PageChunkPayload};
use crate::{Document, decode_dirm_tail, parse_dirm_tail, write_bitmap_pdf};

fn page_info() -> PageInfo {
    PageInfo {
        width: 100,
        height: 200,
        version: 25,
        dpi: 300,
        gamma: 2.2,
        rotation: 1,
    }
}

#[test]
fn page_render_mode_parses_cli_mode_strings() {
    assert_eq!("full".parse::<PageRenderMode>(), Ok(PageRenderMode::Full));
    assert_eq!(
        "background".parse::<PageRenderMode>(),
        Ok(PageRenderMode::Background)
    );
    assert_eq!(
        "foreground".parse::<PageRenderMode>(),
        Ok(PageRenderMode::Foreground)
    );
    assert_eq!("mask".parse::<PageRenderMode>(), Ok(PageRenderMode::Mask));
    assert_eq!(PageRenderMode::Foreground.as_str(), "foreground");

    let error = "bad"
        .parse::<PageRenderMode>()
        .expect_err("unknown mode should fail");

    assert_eq!(
        error,
        RenderError::new(
            "unknown render mode \"bad\"; expected full, background, foreground, or mask"
        )
    );
}

#[test]
fn page_bitmap_allocates_rgb_pixels_and_writes_in_bounds() {
    let mut bitmap = PageBitmap::new_rgb8(3, 2, 300, [0xff, 0xff, 0xff]);

    assert_eq!(bitmap.pixels.len(), 18);
    assert_eq!(bitmap.pixel_offset(2, 1), Some(15));
    assert!(bitmap.set_rgb(1, 1, [0x12, 0x34, 0x56]));
    assert!(!bitmap.set_rgb(3, 1, [0, 0, 0]));
    assert_eq!(&bitmap.pixels[12..15], &[0x12, 0x34, 0x56]);
}

#[test]
fn bitonal_bitmap_sets_and_reads_msb_first_bits() {
    let mut mask = BitonalBitmap::new(3, 3);

    assert_eq!(mask.bit(0, 0), Some(false));
    assert!(mask.set_bit(0, 0, true));
    assert!(mask.set_bit(2, 1, true));
    assert!(mask.set_bit(2, 1, false));
    assert!(!mask.set_bit(3, 0, true));

    assert_eq!(mask.bit(0, 0), Some(true));
    assert_eq!(mask.bit(2, 1), Some(false));
    assert_eq!(mask.bit(3, 0), None);
    assert_eq!(mask.bits, [0b1000_0000, 0]);
}

#[test]
fn bitonal_bitmap_validates_packed_bit_length() {
    assert!(BitonalBitmap::from_bits(9, 1, vec![0; 2]).is_some());
    assert!(BitonalBitmap::from_bits(9, 1, vec![0; 1]).is_none());
}

#[test]
fn bitonal_bitmap_writes_binary_pbm_rows() {
    let mut mask = BitonalBitmap::new(3, 2);
    assert!(mask.set_bit(0, 0, true));
    assert!(mask.set_bit(2, 1, true));

    assert_eq!(mask.to_pbm_bytes(), b"P4\n3 2\n\x80\x20".to_vec());
}

#[test]
fn page_bitmap_paints_bitonal_mask_pixels() {
    let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0xff, 0xff, 0xff]);
    let mut mask = BitonalBitmap::new(2, 2);

    assert!(mask.set_bit(1, 0, true));
    assert!(mask.set_bit(0, 1, true));
    assert!(bitmap.paint_bitonal_mask(&mask, [0, 0, 0]));

    assert_eq!(&bitmap.pixels[0..3], &[0xff, 0xff, 0xff]);
    assert_eq!(&bitmap.pixels[3..6], &[0, 0, 0]);
    assert_eq!(&bitmap.pixels[6..9], &[0, 0, 0]);
    assert_eq!(&bitmap.pixels[9..12], &[0xff, 0xff, 0xff]);
}

#[test]
fn page_bitmap_paints_scaled_iw44_rgb_layer() {
    let mut bitmap = PageBitmap::new_rgb8(4, 4, 300, [0xff, 0xff, 0xff]);
    let image = Iw44RgbImage {
        width: 2,
        height: 2,
        pixels: vec![
            0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0x22, 0x33, 0x44,
        ],
    };
    let mapping = Iw44PageMapping {
        page_width: 4,
        page_height: 4,
        layer_width: 2,
        layer_height: 2,
        subsample: 2,
        scaled_width: 4,
        scaled_height: 4,
        horizontal_overscan: 0,
        vertical_overscan: 0,
    };

    assert!(bitmap.paint_iw44_rgb_layer(&image, &mapping));

    assert_eq!(&bitmap.pixels[0..3], &[0xff, 0x00, 0x00]);
    assert_eq!(&bitmap.pixels[6..9], &[0x40, 0xbf, 0x00]);
    assert_eq!(&bitmap.pixels[24..27], &[0x40, 0x00, 0xbf]);
    assert_eq!(&bitmap.pixels[45..48], &[0x22, 0x33, 0x44]);
}

#[test]
fn iw44_scaler_coordinate_uses_accumulated_fraction_pattern() {
    let three_x: Vec<i32> = (0..6)
        .map(|coordinate| iw44_scaler_coordinate(4, 3, coordinate))
        .collect();
    let five_x: Vec<i32> = (0..8)
        .map(|coordinate| iw44_scaler_coordinate(4, 5, coordinate))
        .collect();

    assert_eq!(three_x, [-5, 0, 6, 11, 16, 22]);
    assert_eq!(five_x, [-6, -3, 0, 4, 7, 10, 13, 16]);
}

#[test]
fn iw44_top_down_scaler_coordinate_mirrors_bottom_origin_rows() {
    let mapping = Iw44PageMapping {
        page_width: 2099,
        page_height: 2853,
        layer_width: 420,
        layer_height: 571,
        subsample: 5,
        scaled_width: 2100,
        scaled_height: 2855,
        horizontal_overscan: 1,
        vertical_overscan: 2,
    };
    let top_down: Vec<i32> = (0..8)
        .map(|coordinate| iw44_top_down_scaler_coordinate(571, &mapping, coordinate))
        .collect();

    assert_eq!(top_down, [0, 3, 6, 9, 12, 16, 19, 22]);
}

#[test]
fn page_bitmap_crops_iw44_overscan_from_top_left() {
    let mut bitmap = PageBitmap::new_rgb8(2, 4, 300, [0xff, 0xff, 0xff]);
    let image = Iw44RgbImage {
        width: 1,
        height: 3,
        pixels: vec![
            0x10, 0x00, 0x00, // overscanned top row
            0x20, 0x00, 0x00, //
            0x30, 0x00, 0x00, //
        ],
    };
    let mapping = Iw44PageMapping {
        page_width: 2,
        page_height: 4,
        layer_width: 1,
        layer_height: 3,
        subsample: 2,
        scaled_width: 2,
        scaled_height: 6,
        horizontal_overscan: 0,
        vertical_overscan: 2,
    };

    assert!(bitmap.paint_iw44_rgb_layer(&image, &mapping));

    assert_eq!(
        bitmap.pixels,
        vec![
            0x1c, 0x00, 0x00, 0x1c, 0x00, 0x00, //
            0x24, 0x00, 0x00, 0x24, 0x00, 0x00, //
            0x2c, 0x00, 0x00, 0x2c, 0x00, 0x00, //
            0x30, 0x00, 0x00, 0x30, 0x00, 0x00, //
        ]
    );
}

#[test]
fn page_bitmap_paints_scaled_iw44_rgb_layer_through_mask() {
    let mut bitmap = PageBitmap::new_rgb8(4, 4, 300, [0xff, 0xff, 0xff]);
    let mut mask = BitonalBitmap::new(4, 4);
    assert!(mask.set_bit(1, 0, true));
    assert!(mask.set_bit(3, 3, true));
    let image = Iw44RgbImage {
        width: 2,
        height: 2,
        pixels: vec![
            0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0x22, 0x33, 0x44,
        ],
    };
    let mapping = Iw44PageMapping {
        page_width: 4,
        page_height: 4,
        layer_width: 2,
        layer_height: 2,
        subsample: 2,
        scaled_width: 4,
        scaled_height: 4,
        horizontal_overscan: 0,
        vertical_overscan: 0,
    };

    assert!(bitmap.paint_iw44_rgb_layer_through_mask(&image, &mapping, &mask));

    assert_eq!(&bitmap.pixels[0..3], &[0xff, 0xff, 0xff]);
    assert_eq!(&bitmap.pixels[3..6], &[0xff, 0x00, 0x00]);
    assert_eq!(&bitmap.pixels[42..45], &[0xff, 0xff, 0xff]);
    assert_eq!(&bitmap.pixels[45..48], &[0x22, 0x33, 0x44]);
}

#[test]
fn page_bitmap_paints_iw44_foreground_through_mask_without_horizontal_overscan() {
    let mut bitmap = PageBitmap::new_rgb8(2, 1, 300, [0xff, 0xff, 0xff]);
    let mut mask = BitonalBitmap::new(2, 1);
    assert!(mask.set_bit(0, 0, true));
    assert!(mask.set_bit(1, 0, true));
    let image = Iw44RgbImage {
        width: 2,
        height: 1,
        pixels: vec![0x10, 0x00, 0x00, 0x20, 0x00, 0x00],
    };
    let mapping = Iw44PageMapping {
        page_width: 2,
        page_height: 1,
        layer_width: 2,
        layer_height: 1,
        subsample: 2,
        scaled_width: 4,
        scaled_height: 2,
        horizontal_overscan: 2,
        vertical_overscan: 0,
    };

    assert!(bitmap.paint_iw44_rgb_layer_through_mask(&image, &mapping, &mask));

    assert_eq!(bitmap.pixels, vec![0x10, 0x00, 0x00, 0x10, 0x00, 0x00]);
}

#[test]
fn page_bitmap_rejects_mask_with_mismatched_dimensions() {
    let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0xff, 0xff, 0xff]);
    let mask = BitonalBitmap::new(1, 2);

    assert!(!bitmap.paint_bitonal_mask(&mask, [0, 0, 0]));
}

#[test]
fn page_bitmap_can_be_created_from_page_info() {
    let bitmap = PageBitmap::white_rgb8(&page_info());

    assert_eq!(bitmap.width, 100);
    assert_eq!(bitmap.height, 200);
    assert_eq!(bitmap.dpi, 300);
    assert!(bitmap.pixels.iter().all(|byte| *byte == 0xff));
}

#[test]
fn page_bitmap_writes_binary_ppm_bytes() {
    let mut bitmap = PageBitmap::new_rgb8(1, 2, 300, [0xff, 0xff, 0xff]);
    assert!(bitmap.set_rgb(0, 1, [0, 0, 0]));

    assert_eq!(
        bitmap.to_ppm_bytes(),
        b"P6\n1 2\n255\n\xff\xff\xff\0\0\0".to_vec()
    );
}

#[test]
fn page_bitmap_reads_binary_ppm_bytes() {
    let ppm = b"P6\n# fixture\n2 1\n255\n\x00\x01\x02\xfd\xfe\xff";

    let bitmap = PageBitmap::from_ppm_bytes(ppm, 144).expect("PPM should parse");

    assert_eq!(bitmap.width, 2);
    assert_eq!(bitmap.height, 1);
    assert_eq!(bitmap.dpi, 144);
    assert_eq!(bitmap.pixels, [0, 1, 2, 253, 254, 255]);
}

#[test]
fn page_bitmap_rejects_unsupported_ppm() {
    assert_eq!(
        PageBitmap::from_ppm_bytes(b"P3\n1 1\n255\n0 0 0", 300)
            .expect_err("plain-text PPM should fail")
            .to_string(),
        "PPM oracle must use binary P6 format"
    );
    assert_eq!(
        PageBitmap::from_ppm_bytes(b"P6\n1 1\n15\n\x00\x00\x00", 300)
            .expect_err("non-8-bit PPM should fail")
            .to_string(),
        "PPM oracle max value 15 is not supported"
    );
}

#[test]
fn page_bitmap_diff_reports_component_deltas() {
    let mut actual = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);
    let mut expected = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);
    assert!(actual.set_rgb(1, 0, [10, 20, 30]));
    assert!(expected.set_rgb(1, 0, [7, 25, 40]));

    let diff = actual.diff(&expected).expect("matching dimensions");

    assert_eq!(diff.width, 2);
    assert_eq!(diff.height, 1);
    assert_eq!(diff.compared_pixels, 2);
    assert_eq!(diff.exact_pixels, 1);
    assert_eq!(diff.differing_pixels, 1);
    assert_eq!(diff.total_abs_delta, 18);
    assert_eq!(diff.max_abs_delta, 10);
    assert_eq!(diff.max_delta_pixels, 1);
    assert!((diff.mean_abs_delta - 3.0).abs() < f64::EPSILON);
    assert_eq!(
        diff.channels,
        [
            PageBitmapChannelDiff {
                total_abs_delta: 3,
                signed_delta: 3,
                max_abs_delta: 3,
                mean_abs_delta: 1.5,
                mean_signed_delta: 1.5,
            },
            PageBitmapChannelDiff {
                total_abs_delta: 5,
                signed_delta: -5,
                max_abs_delta: 5,
                mean_abs_delta: 2.5,
                mean_signed_delta: -2.5,
            },
            PageBitmapChannelDiff {
                total_abs_delta: 10,
                signed_delta: -10,
                max_abs_delta: 10,
                mean_abs_delta: 5.0,
                mean_signed_delta: -5.0,
            },
        ]
    );
    assert_eq!(
        diff.bounds,
        Some(PageBitmapDiffBounds {
            min_x: 1,
            min_y: 0,
            max_x: 1,
            max_y: 0,
        })
    );
    assert_eq!(
        diff.first_difference,
        Some(PageBitmapDiffPixel {
            x: 1,
            y: 0,
            actual: [10, 20, 30],
            expected: [7, 25, 40],
            abs_delta_sum: 18,
            max_abs_delta: 10,
        })
    );
    assert_eq!(diff.max_difference, diff.first_difference);
}

#[test]
fn page_bitmap_diff_counts_pixels_at_max_delta() {
    let mut actual = PageBitmap::new_rgb8(3, 1, 300, [0, 0, 0]);
    let expected = PageBitmap::new_rgb8(3, 1, 300, [0, 0, 0]);
    assert!(actual.set_rgb(0, 0, [9, 0, 0]));
    assert!(actual.set_rgb(1, 0, [0, 9, 0]));
    assert!(actual.set_rgb(2, 0, [0, 0, 3]));

    let diff = actual.diff(&expected).expect("matching dimensions");

    assert_eq!(diff.differing_pixels, 3);
    assert_eq!(diff.max_abs_delta, 9);
    assert_eq!(diff.max_delta_pixels, 2);
}

#[test]
fn bitmap_diff_failures_reports_exceeded_compare_limits() {
    let mut actual = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);
    let expected = PageBitmap::new_rgb8(2, 1, 300, [0xff, 0xff, 0xff]);
    assert!(actual.set_rgb(1, 0, [0xff, 0xff, 0xfe]));
    let diff = actual.diff(&expected).expect("matching dimensions");

    assert_eq!(
        bitmap_diff_failures(&diff, RenderCompareLimits::new(1, 1, Some(0), 1.0)),
        [
            "render differs in 2 pixels; allowed 1",
            "render max absolute delta is 255; allowed 1",
            "render has 1 pixels at max absolute delta; allowed 0",
            "render mean absolute delta is 127.666667; allowed 1.000000",
        ]
    );
}

#[test]
fn bitmap_diff_region_summary_counts_local_difference_characteristics() {
    let mut actual = PageBitmap::new_rgb8(3, 2, 300, [0xff, 0xff, 0xff]);
    let mut expected = actual.clone();
    assert!(actual.set_rgb(1, 0, [0, 0, 0]));
    assert!(actual.set_rgb(2, 0, [0x10, 0x20, 0x30]));
    assert!(expected.set_rgb(2, 0, [0x10, 0x20, 0x31]));

    let summary = bitmap_diff_region_summary(
        &actual,
        &expected,
        PageBitmapDiffBounds {
            min_x: 1,
            min_y: 0,
            max_x: 2,
            max_y: 0,
        },
    )
    .expect("region should be in bounds");

    assert_eq!(
        summary,
        PageBitmapDiffRegionSummary {
            pixels: 2,
            differing_pixels: 2,
            total_abs_delta: 766,
            max_abs_delta: 255,
            max_delta_pixels: 1,
            actual_black_pixels: 1,
            expected_black_pixels: 0,
            actual_white_pixels: 0,
            expected_white_pixels: 1,
        }
    );
}

#[test]
fn bitmap_diff_tile_summaries_reports_only_differing_tiles() {
    let mut actual = PageBitmap::new_rgb8(5, 3, 300, [0, 0, 0]);
    let expected = actual.clone();
    assert!(actual.set_rgb(4, 2, [0xff, 0, 0]));

    let tiles = bitmap_diff_tile_summaries(&actual, &expected, 2, 2)
        .expect("matching dimensions should produce tile summaries");

    assert_eq!(
        tiles,
        [PageBitmapDiffTileSummary {
            bounds: PageBitmapDiffBounds {
                min_x: 4,
                min_y: 2,
                max_x: 4,
                max_y: 2,
            },
            summary: PageBitmapDiffRegionSummary {
                pixels: 1,
                differing_pixels: 1,
                total_abs_delta: 255,
                max_abs_delta: 255,
                max_delta_pixels: 1,
                actual_black_pixels: 0,
                expected_black_pixels: 1,
                actual_white_pixels: 0,
                expected_white_pixels: 0,
            },
        }]
    );
}

#[test]
fn bitmap_diff_tile_summaries_rejects_invalid_inputs() {
    let actual = PageBitmap::new_rgb8(1, 1, 300, [0, 0, 0]);
    let expected = PageBitmap::new_rgb8(2, 1, 300, [0, 0, 0]);

    assert!(bitmap_diff_tile_summaries(&actual, &actual, 0, 1).is_none());
    assert!(bitmap_diff_tile_summaries(&actual, &actual, 1, 0).is_none());
    assert!(bitmap_diff_tile_summaries(&actual, &expected, 1, 1).is_none());
}

#[test]
fn page_bitmap_stats_describe_rgb_pixels() {
    let mut bitmap = PageBitmap::new_rgb8(2, 2, 300, [0xff, 0xff, 0xff]);
    assert!(bitmap.set_rgb(0, 0, [0, 0, 0]));
    assert!(bitmap.set_rgb(1, 0, [10, 20, 30]));
    assert!(bitmap.set_rgb(0, 1, [7, 7, 7]));

    let stats = bitmap.stats();

    assert_eq!(stats.width, 2);
    assert_eq!(stats.height, 2);
    assert_eq!(stats.pixel_count, 4);
    assert_eq!(stats.black_pixels, 1);
    assert_eq!(stats.white_pixels, 1);
    assert_eq!(stats.non_gray_pixels, 1);
    assert_eq!(stats.component_sum, 846);
    assert_ne!(stats.fingerprint, 0);
}

#[test]
fn render_plan_creates_base_bitmap_from_info() {
    let plan = PageRenderPlan::new(page_info(), Vec::new());
    let bitmap = plan.render_base_bitmap();

    assert_eq!(bitmap.width, 100);
    assert_eq!(bitmap.height, 200);
    assert_eq!(bitmap.dpi, 300);
}

fn resolved_chunk(kind: PageChunkKind, id: &'static str) -> ResolvedPageChunk<'static> {
    ResolvedPageChunk {
        source: PageChunkSource::Page,
        chunk: PageChunk {
            chunk: Chunk {
                id,
                size: 0,
                data_start: 0,
                data_end: 0,
                next_start: 0,
            },
            kind,
            payload: PageChunkPayload::Raw,
        },
    }
}

#[test]
fn render_plan_classifies_effective_page_chunks() {
    let plan = PageRenderPlan::new(
        page_info(),
        vec![
            resolved_chunk(PageChunkKind::Info, "INFO"),
            resolved_chunk(PageChunkKind::Djbz, "Djbz"),
            resolved_chunk(PageChunkKind::Sjbz, "Sjbz"),
            resolved_chunk(PageChunkKind::Fg44, "FG44"),
            resolved_chunk(PageChunkKind::Bg44, "BG44"),
            resolved_chunk(PageChunkKind::Txtz, "TXTz"),
            resolved_chunk(PageChunkKind::Unknown, "ZZZZ"),
        ],
    );

    assert!(plan.has_image_data());
    assert!(plan.has_text());
    assert_eq!(plan.bitonal_dictionaries, [1]);
    assert_eq!(plan.bitonal_images, [2]);
    assert_eq!(plan.foreground_layers, [3]);
    assert_eq!(plan.background_layers, [4]);
    assert_eq!(plan.text_chunks, [5]);
    assert_eq!(plan.unknown_chunks, [6]);
    assert_eq!(
        plan.chunk(2).map(|chunk| chunk.chunk.chunk.id),
        Some("Sjbz")
    );
}

#[test]
fn render_plan_recognizes_all_rypka_effective_chunks() {
    const RYPKA: &[u8] = include_bytes!("../../Rypka-HIL.djvu");
    let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
    let decoded_tail;
    let tail_entries = if let Some(dirm) = &document.directory {
        decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
        parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
    } else {
        Vec::new()
    };

    let mut page_count = 0usize;
    for (index, page) in document.pages(RYPKA).enumerate() {
        let page = page.expect("fixture page should parse");
        let plan = document
            .page_render_plan(RYPKA, &page, &tail_entries)
            .expect("fixture page should plan");
        assert!(
            plan.unknown_chunks.is_empty(),
            "page {} has unknown effective chunks: {:?}",
            index + 1,
            plan.unknown_chunks
        );
        page_count += 1;
    }

    assert_eq!(page_count, 961);
}

#[test]
fn render_plan_extracts_bitonal_payloads_by_chunk_index() {
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![
            resolved_chunk(PageChunkKind::Djbz, "Djbz"),
            resolved_chunk(PageChunkKind::Sjbz, "Sjbz"),
            resolved_chunk(PageChunkKind::Txtz, "TXTz"),
        ],
    );
    plan.chunks[0].chunk.chunk.data_start = 1;
    plan.chunks[0].chunk.chunk.data_end = 4;
    plan.chunks[1].chunk.chunk.data_start = 4;
    plan.chunks[1].chunk.chunk.data_end = 7;
    let bytes = b"Xdictimg";

    let dictionaries = plan.bitonal_dictionary_payloads(bytes);
    let images = plan.bitonal_image_payloads(bytes);

    assert_eq!(
        dictionaries,
        [RenderChunkPayload {
            index: 0,
            bytes: b"dic"
        }]
    );
    assert_eq!(
        images,
        [RenderChunkPayload {
            index: 1,
            bytes: b"tim"
        }]
    );
}

#[test]
fn render_plan_extracts_iw44_layer_payloads_by_chunk_index() {
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![
            resolved_chunk(PageChunkKind::Fg44, "FG44"),
            resolved_chunk(PageChunkKind::Bg44, "BG44"),
            resolved_chunk(PageChunkKind::Bg44, "BG44"),
            resolved_chunk(PageChunkKind::Sjbz, "Sjbz"),
        ],
    );
    plan.chunks[0].chunk.chunk.data_start = 1;
    plan.chunks[0].chunk.chunk.data_end = 4;
    plan.chunks[1].chunk.chunk.data_start = 4;
    plan.chunks[1].chunk.chunk.data_end = 7;
    plan.chunks[2].chunk.chunk.data_start = 7;
    plan.chunks[2].chunk.chunk.data_end = 9;
    let bytes = b"Xfgbg0bg1";

    let foreground = plan.foreground_layer_payloads(bytes);
    let background = plan.background_layer_payloads(bytes);

    assert_eq!(
        foreground,
        [RenderChunkPayload {
            index: 0,
            bytes: b"fgb"
        }]
    );
    assert_eq!(
        background,
        [
            RenderChunkPayload {
                index: 1,
                bytes: b"g0b"
            },
            RenderChunkPayload {
                index: 2,
                bytes: b"g1"
            }
        ]
    );
}

#[test]
fn render_plan_reads_iw44_layer_summaries() {
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![
            resolved_chunk(PageChunkKind::Fg44, "FG44"),
            resolved_chunk(PageChunkKind::Bg44, "BG44"),
            resolved_chunk(PageChunkKind::Bg44, "BG44"),
        ],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = 10;
    plan.chunks[1].chunk.chunk.data_start = 10;
    plan.chunks[1].chunk.chunk.data_end = 20;
    plan.chunks[2].chunk.chunk.data_start = 20;
    plan.chunks[2].chunk.chunk.data_end = 23;
    let bytes = [
        0x00, 0x05, 0x01, 0x02, 0x00, 0x20, 0x00, 0x10, 0x80, 0xaa, 0x00, 0x07, 0x01, 0x02, 0x00,
        0x40, 0x00, 0x20, 0x00, 0xbb, 0x01, 0x03, 0xcc,
    ];

    let foreground = plan
        .foreground_layer_summary(&bytes)
        .expect("foreground summary should parse")
        .expect("foreground layer should exist");
    let background = plan
        .background_layer_summary(&bytes)
        .expect("background summary should parse")
        .expect("background layer should exist");

    assert_eq!(foreground.image.width, 32);
    assert_eq!(foreground.image.height, 16);
    assert_eq!(foreground.total_slices, 5);
    assert_eq!(foreground.total_payload_bytes, 1);
    assert_eq!(background.image.width, 64);
    assert_eq!(background.image.height, 32);
    assert_eq!(background.total_slices, 10);
    assert_eq!(background.total_payload_bytes, 2);
}

#[test]
fn render_plan_maps_iw44_layers_to_page_space() {
    let mut plan = PageRenderPlan::new(
        PageInfo {
            width: 1560,
            height: 1633,
            version: 25,
            dpi: 200,
            gamma: 2.2,
            rotation: 1,
        },
        vec![
            resolved_chunk(PageChunkKind::Fg44, "FG44"),
            resolved_chunk(PageChunkKind::Bg44, "BG44"),
        ],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = 10;
    plan.chunks[1].chunk.chunk.data_start = 10;
    plan.chunks[1].chunk.chunk.data_end = 20;
    let bytes = [
        0x00, 0x64, 0x01, 0x02, 0x00, 0xc3, 0x00, 0xcd, 0x80, 0xaa, 0x00, 0x4a, 0x01, 0x02, 0x03,
        0x0c, 0x03, 0x31, 0x8a, 0xbb,
    ];

    let foreground = plan
        .foreground_layer_geometry(&bytes)
        .expect("foreground geometry should parse")
        .expect("foreground layer should exist");
    let background = plan
        .background_layer_geometry(&bytes)
        .expect("background geometry should parse")
        .expect("background layer should exist");

    assert_eq!(foreground.mapping.subsample, 8);
    assert_eq!(foreground.mapping.scaled_width, 1560);
    assert_eq!(foreground.mapping.scaled_height, 1640);
    assert_eq!(foreground.mapping.vertical_overscan, 7);
    assert_eq!(background.mapping.subsample, 2);
    assert_eq!(background.mapping.scaled_width, 1560);
    assert_eq!(background.mapping.scaled_height, 1634);
    assert_eq!(background.mapping.vertical_overscan, 1);
}

#[test]
fn render_plan_returns_sjbz_payloads_without_bzz_decoding() {
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = 3;

    let images = plan.bitonal_image_payloads(b"jb2");

    assert_eq!(
        images,
        [RenderChunkPayload {
            index: 0,
            bytes: b"jb2"
        }]
    );
}

#[test]
fn render_plan_reads_bitonal_image_headers() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let headers = plan
        .bitonal_image_headers(RYPKA_PAGE_1_SJBZ)
        .expect("JB2 header should parse");

    assert_eq!(
        headers,
        [BitonalImageHeader {
            index: 0,
            header: Jb2ImageHeader {
                width: 1560,
                height: 1633,
                inherited_dictionary_symbols: 0,
            },
        }]
    );
}

#[test]
fn render_plan_reads_bitonal_record_prefixes() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let prefixes = plan
        .bitonal_record_prefixes(RYPKA_PAGE_1_SJBZ, 8)
        .expect("JB2 record prefix should parse");

    assert_eq!(prefixes.len(), 1);
    assert_eq!(prefixes[0].0, 0);
    assert_eq!(prefixes[0].1.header.width, 1560);
    assert!(!prefixes[0].1.records.is_empty());
}

#[test]
fn render_plan_reads_partial_bitonal_masks() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let masks = plan
        .partial_bitonal_masks(RYPKA_PAGE_1_SJBZ, 8)
        .expect("partial JB2 mask should render");

    assert_eq!(masks.len(), 1);
    assert_eq!(masks[0].1.mask.width, 1560);
    assert!(!masks[0].1.reached_end_of_data);
    assert!(masks[0].1.mask.black_pixel_count() > 0);
}

#[test]
fn render_plan_reads_complete_bitonal_masks() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let masks = plan
        .bitonal_masks(RYPKA_PAGE_1_SJBZ)
        .expect("complete JB2 mask should render");

    assert_eq!(masks.len(), 1);
    assert!(masks[0].1.reached_end_of_data);
    assert_eq!(masks[0].1.mask.black_pixel_count(), 167_493);
}

#[test]
fn render_plan_paints_bitonal_masks_into_rgb_bitmap() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        PageInfo {
            width: 1560,
            height: 1633,
            version: 25,
            dpi: 200,
            gamma: 2.2,
            rotation: 1,
        },
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let render = plan
        .render_partial_bitmap(RYPKA_PAGE_1_SJBZ)
        .expect("partial bitmap should render");
    let black_pixels = render
        .bitmap
        .pixels
        .chunks_exact(3)
        .filter(|pixel| **pixel == [0, 0, 0])
        .count();

    assert_eq!(render.bitonal_masks.len(), 1);
    assert!(render.bitonal_masks[0].1.reached_end_of_data);
    assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
    assert_eq!(black_pixels, 167_493);
}

#[test]
fn render_plan_foreground_mode_paints_bitonal_masks_without_iw44_foreground() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        PageInfo {
            width: 1560,
            height: 1633,
            version: 25,
            dpi: 200,
            gamma: 2.2,
            rotation: 1,
        },
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let render = plan
        .render_bitmap_with_mode(RYPKA_PAGE_1_SJBZ, PageRenderMode::Foreground)
        .expect("foreground mode should render");
    let black_pixels = render
        .bitmap
        .pixels
        .chunks_exact(3)
        .filter(|pixel| **pixel == [0, 0, 0])
        .count();

    assert!(render.iw44_layers.is_empty());
    assert_eq!(render.bitonal_masks.len(), 1);
    assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
    assert_eq!(black_pixels, 167_493);
}

#[test]
fn render_plan_paints_iw44_background_before_bitonal_masks() {
    const RYPKA: &[u8] = include_bytes!("../../Rypka-HIL.djvu");
    let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
    let page = document
        .pages(RYPKA)
        .next()
        .expect("fixture should have a first page")
        .expect("first page should parse");
    let mut decoded_tail = Vec::new();
    let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
        decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
        parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
    });
    let plan = document
        .page_render_plan(RYPKA, &page, &tail_entries)
        .expect("render plan should parse");

    let render = plan
        .render_partial_bitmap(RYPKA)
        .expect("partial bitmap should render");
    let stats = render.bitmap.stats();
    let black_pixels = render
        .bitmap
        .pixels
        .chunks_exact(3)
        .filter(|pixel| **pixel == [0, 0, 0])
        .count();
    let white_pixels = render
        .bitmap
        .pixels
        .chunks_exact(3)
        .filter(|pixel| **pixel == [0xff, 0xff, 0xff])
        .count();
    let foreground_colored_mask_pixels = (0..render.bitmap.height)
        .flat_map(|y| (0..render.bitmap.width).map(move |x| (x, y)))
        .filter(|(x, y)| render.bitonal_masks[0].1.mask.bit(*x, *y).unwrap_or(false))
        .filter(|(x, y)| {
            let offset = render
                .bitmap
                .pixel_offset(*x, *y)
                .expect("mask coordinate should be in bitmap");
            render.bitmap.pixels[offset..offset + 3] != [0, 0, 0]
        })
        .count();

    assert_eq!(render.iw44_layers.len(), 2);
    assert_eq!(render.iw44_layers[0].role, Iw44LayerRole::Background);
    assert_eq!(render.iw44_layers[0].chunk_indices, [3, 4, 5, 6]);
    assert_eq!(render.iw44_layers[0].image.width, 780);
    assert_eq!(render.iw44_layers[0].image.height, 817);
    assert_eq!(render.iw44_layers[1].role, Iw44LayerRole::Foreground);
    assert_eq!(render.iw44_layers[1].chunk_indices, [2]);
    assert_eq!(render.bitonal_masks.len(), 1);
    assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 167_493);
    assert!(black_pixels < 167_493);
    assert!(foreground_colored_mask_pixels > 0);
    assert!(white_pixels < 100_000);
    assert_eq!(stats.width, 1560);
    assert_eq!(stats.height, 1633);
    assert_eq!(stats.pixel_count, 1560 * 1633);
    assert_eq!(
        (
            stats.black_pixels,
            stats.white_pixels,
            stats.non_gray_pixels,
            stats.component_sum,
            stats.fingerprint,
        ),
        (0, 0, 2_547_440, 621_553_200, 9_231_463_871_951_002_652)
    );

    let pdf = write_bitmap_pdf(std::slice::from_ref(&render.bitmap))
        .expect("rendered bitmap should serialize as PDF");
    let text = String::from_utf8_lossy(&pdf);

    assert!(text.starts_with("%PDF-1.4\n"));
    assert!(text.contains("/Type /Catalog"));
    assert!(text.contains("/Type /Page"));
    assert!(text.contains("/MediaBox [0 0 561.6000 587.8800]"));
    assert!(text.contains("/Subtype /Image /Width 1560 /Height 1633"));
    assert!(text.contains("/Filter "));
    assert!(text.contains("xref\n0 6\n"));
    assert!(pdf.ends_with(b"%%EOF\n"));
}

#[test]
fn render_plan_paints_rypka_page_961_background_without_iw44_artifact() {
    const RYPKA: &[u8] = include_bytes!("../../Rypka-HIL.djvu");
    let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
    let page = document
        .pages(RYPKA)
        .nth(960)
        .expect("fixture should have page 961")
        .expect("page 961 should parse");
    let mut decoded_tail = Vec::new();
    let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
        decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
        parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
    });
    let plan = document
        .page_render_plan(RYPKA, &page, &tail_entries)
        .expect("render plan should parse");
    let background = plan
        .background_iw44_layer(RYPKA)
        .expect("background IW44 should decode")
        .expect("page should have background IW44");
    let mut bitmap = plan.render_base_bitmap();

    assert!(bitmap.paint_iw44_rgb_layer(&background.image, &background.geometry.mapping));

    let offset = bitmap
        .pixel_offset(1167, 834)
        .expect("target pixel should be in page");
    assert_eq!(&bitmap.pixels[offset..offset + 3], &[0xff, 0xff, 0xff]);
    assert_eq!(background.role, Iw44LayerRole::Background);
    assert_eq!(background.chunk_indices, [1, 2, 3, 4]);
    assert_eq!(background.image.width, 3486);
    assert_eq!(background.image.height, 2783);
    assert_eq!(background.geometry.mapping.subsample, 1);

    let stats = bitmap.stats();
    assert_eq!(stats.width, 3486);
    assert_eq!(stats.height, 2783);
    assert_eq!(
        (
            stats.black_pixels,
            stats.white_pixels,
            stats.non_gray_pixels,
            stats.component_sum,
            stats.fingerprint,
        ),
        (
            4_577,
            4_757_199,
            2_831_750,
            6_781_848_240,
            0xb902_dd6e_3b4e_270d
        )
    );
}

#[test]
fn render_plan_uses_inherited_jb2_dictionary() {
    const RYPKA: &[u8] = include_bytes!("../../Rypka-HIL.djvu");
    let document = Document::parse(RYPKA).expect("fixture DjVu should parse");
    let page = document
        .pages(RYPKA)
        .nth(2)
        .expect("fixture should have a third page")
        .expect("third page should parse");
    let mut decoded_tail = Vec::new();
    let tail_entries = document.directory.as_ref().map_or_else(Vec::new, |dirm| {
        decoded_tail = decode_dirm_tail(RYPKA, dirm).expect("DIRM tail should decode");
        parse_dirm_tail(dirm, &decoded_tail).expect("DIRM tail should parse")
    });
    let plan = document
        .page_render_plan(RYPKA, &page, &tail_entries)
        .expect("render plan should parse");

    let render = plan
        .render_partial_bitmap(RYPKA)
        .expect("partial bitmap should render");
    let stats = render.bitmap.stats();

    assert_eq!(render.bitonal_masks.len(), 1);
    assert_eq!(
        render.bitonal_masks[0]
            .1
            .header
            .inherited_dictionary_symbols,
        284
    );
    assert!(render.bitonal_masks[0].1.reached_end_of_data);
    assert_eq!(render.bitonal_masks[0].1.dictionary_symbol_count, 293);
    assert_eq!(render.bitonal_masks[0].1.mask.black_pixel_count(), 29_084);
    assert_eq!(stats.black_pixels, 29_084);
    assert_eq!(stats.white_pixels, 17_344_292);
    assert_eq!(stats.fingerprint, 0x5004_65d0_2fa7_ab6f);
}

#[test]
fn render_plan_rejects_bitonal_masks_with_mismatched_page_dimensions() {
    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../../tests/fixtures/jb2/rypka-page-1.jb2");
    let mut plan = PageRenderPlan::new(
        page_info(),
        vec![resolved_chunk(PageChunkKind::Sjbz, "Sjbz")],
    );
    plan.chunks[0].chunk.chunk.data_start = 0;
    plan.chunks[0].chunk.chunk.data_end = RYPKA_PAGE_1_SJBZ.len();

    let error = plan
        .render_partial_bitmap(RYPKA_PAGE_1_SJBZ)
        .expect_err("mismatched dimensions should fail");

    assert_eq!(
        error.to_string(),
        "bitonal image #0 dimensions 1560x1633 do not match page 100x200"
    );
}
