use super::*;
use crate::{Document, decode_dirm_tail, parse_dirm_tail};

#[test]
fn reads_first_iw44_chunk_header() {
    let header =
        read_iw44_chunk_header(&[0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff])
            .expect("IW44 header should parse");

    assert_eq!(
        header,
        Iw44ChunkHeader {
            serial: 0,
            slices: 74,
            image: Some(Iw44ImageHeader {
                major_version: 1,
                minor_version: 2,
                width: 780,
                height: 817,
                grayscale: false,
                delay: 10,
                chroma_half: false,
            }),
            payload_start: 9,
            payload_len: 1,
        }
    );
}

#[test]
fn reads_subsequent_iw44_chunk_header() {
    let header = read_iw44_chunk_header(&[0x01, 0x0a, 0xaa]).expect("IW44 header should parse");

    assert_eq!(
        header,
        Iw44ChunkHeader {
            serial: 1,
            slices: 10,
            image: None,
            payload_start: 2,
            payload_len: 1,
        }
    );
}

#[test]
fn reads_grayscale_first_iw44_chunk_header() {
    let header = read_iw44_chunk_header(&[0x00, 0x01, 0x80, 0x02, 0x00, 0x20, 0x00, 0x10, 0x00])
        .expect("IW44 header should parse");

    assert_eq!(
        header.image,
        Some(Iw44ImageHeader {
            major_version: 0x80,
            minor_version: 2,
            width: 32,
            height: 16,
            grayscale: true,
            delay: 0,
            chroma_half: false,
        })
    );
}

#[test]
fn rejects_short_iw44_chunks() {
    assert_eq!(
        read_iw44_chunk_header(&[0]).expect_err("short chunk should fail"),
        Iw44Error::new("IW44 chunk is too short")
    );
    assert_eq!(
        read_iw44_chunk_header(&[0, 1, 0]).expect_err("short first header should fail"),
        Iw44Error::new("IW44 first chunk header is too short")
    );
}

#[test]
fn rejects_zero_iw44_dimensions() {
    let error = read_iw44_chunk_header(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00])
        .expect_err("zero dimension should fail");

    assert_eq!(error, Iw44Error::new("IW44 image has zero dimension"));
}

#[test]
fn summarizes_iw44_layer_chunks() {
    let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
    let second = [0x01, 0x0a, 0xaa, 0xbb];

    let summary =
        summarize_iw44_layer([first.as_slice(), second.as_slice()]).expect("layer should parse");

    assert_eq!(summary.image.width, 780);
    assert_eq!(summary.image.height, 817);
    assert_eq!(summary.chunks.len(), 2);
    assert_eq!(summary.total_slices, 84);
    assert_eq!(summary.total_payload_bytes, 3);
}

#[test]
fn maps_iw44_layer_to_page_space() {
    let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
    let summary = summarize_iw44_layer([first.as_slice()]).expect("layer should parse");

    assert_eq!(
        summary.page_mapping(1560, 1633),
        Iw44PageMapping {
            page_width: 1560,
            page_height: 1633,
            layer_width: 780,
            layer_height: 817,
            subsample: 2,
            scaled_width: 1560,
            scaled_height: 1634,
            horizontal_overscan: 0,
            vertical_overscan: 1,
        }
    );
}

#[test]
fn decoder_tracks_progressive_iw44_chunks_and_slices() {
    let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
    let second = [0x01, 0x0a, 0xaa, 0xbb];
    let mut decoder = Iw44Decoder::new();

    let first_chunk = decoder
        .decode_chunk(&first)
        .expect("first IW44 chunk should decode");
    let second_chunk = decoder
        .decode_chunk(&second)
        .expect("second IW44 chunk should decode");

    assert_eq!(decoder.image().map(|image| image.width), Some(780));
    assert_eq!(decoder.chunks_decoded(), 2);
    assert_eq!(decoder.slices_decoded(), 84);
    assert_eq!(decoder.payload_bytes_seen(), 3);
    assert_eq!(first_chunk.first_slice_index, 0);
    assert_eq!(first_chunk.slice_count, 74);
    assert_eq!(first_chunk.slices.len(), 74);
    assert_eq!(first_chunk.slices[0].index, 0);
    assert_eq!(
        first_chunk.slices[0].planes,
        vec![expected_plane_slice(
            Iw44Plane::Y,
            0,
            0..=0,
            0x0002_0000,
            650
        )]
    );
    assert_eq!(
        first_chunk.slices[10].planes,
        vec![
            expected_plane_slice(Iw44Plane::Y, 0, 0..=0, 0x0001_0000, 650),
            expected_plane_slice(Iw44Plane::Cb, 0, 0..=0, 0x0002_0000, 650),
            expected_plane_slice(Iw44Plane::Cr, 0, 0..=0, 0x0002_0000, 650),
        ]
    );
    assert_eq!(second_chunk.first_slice_index, 74);
    assert_eq!(second_chunk.slice_count, 10);
    assert_eq!(second_chunk.slices[0].index, 74);
    assert_eq!(
        second_chunk.slices[0].planes,
        vec![
            expected_plane_slice(Iw44Plane::Y, 4, 4..=7, 0x0000_0800, 2600),
            expected_plane_slice(Iw44Plane::Cb, 4, 4..=7, 0x0000_1000, 2600),
            expected_plane_slice(Iw44Plane::Cr, 4, 4..=7, 0x0000_1000, 2600),
        ]
    );
    assert_eq!(decoder.y.coefficients.block_count(), 650);
    assert_eq!(decoder.y.coefficients.bucket_passes(0, 0), 9);
    assert_eq!(decoder.y.coefficients.bucket_passes(0, 4), 4);
    assert_eq!(decoder.y.coefficients.bucket_passes(0, 8), 4);
    assert_eq!(
        decoder
            .cb
            .as_ref()
            .expect("color decoder should have Cb plane")
            .coefficients
            .bucket_passes(0, 0),
        8
    );
}

fn expected_plane_slice(
    plane: Iw44Plane,
    band: u8,
    buckets: std::ops::RangeInclusive<u8>,
    quant: u32,
    scheduled_bucket_count: usize,
) -> Iw44PlaneSlice {
    Iw44PlaneSlice {
        plane,
        band,
        first_bucket: *buckets.start(),
        last_bucket: *buckets.end(),
        quant,
        is_null: false,
        block_count: 650,
        scheduled_bucket_count,
    }
}

#[test]
fn decoder_rejects_iw44_chunk_serial_gap() {
    let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
    let third = [0x02, 0x0a, 0xaa];
    let mut decoder = Iw44Decoder::new();

    decoder
        .decode_chunk(&first)
        .expect("first IW44 chunk should decode");
    let error = decoder
        .decode_chunk(&third)
        .expect_err("serial gap should fail");

    assert_eq!(
        error,
        Iw44Error::new("IW44 chunk serial 2 does not match expected 1")
    );
}

#[test]
fn decoder_builds_coefficients_from_rypka_background_chunks() {
    const RYPKA: &[u8] = include_bytes!("../../fixtures/Rypka-HIL.djvu");
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
    let background = plan.background_layer_payloads(RYPKA);
    assert_eq!(background.len(), 4);

    let mut first_chunk_decoder = Iw44Decoder::new();
    first_chunk_decoder
        .decode_chunk(background[0].bytes)
        .expect("first BG44 chunk should decode");
    let first_chunk_y = first_chunk_decoder.plane_coefficient_summaries()[0];

    let mut full_decoder = Iw44Decoder::new();
    for payload in &background {
        full_decoder
            .decode_chunk(payload.bytes)
            .expect("BG44 chunk should decode");
    }

    assert_eq!(full_decoder.image().map(|image| image.width), Some(780));
    assert_eq!(full_decoder.chunks_decoded(), 4);
    assert_eq!(full_decoder.slices_decoded(), 97);
    let summaries = full_decoder.plane_coefficient_summaries();
    assert_eq!(summaries.len(), 3);
    assert_eq!(summaries[0].plane, Iw44Plane::Y);
    assert_eq!(summaries[0].width, 780);
    assert_eq!(summaries[0].height, 817);
    assert_eq!(summaries[0].block_count, 650);
    assert!(summaries[0].non_zero_coefficients >= first_chunk_y.non_zero_coefficients);
    for summary in &summaries {
        assert_eq!(summary.block_count, 650);
        assert!(summary.non_zero_coefficients > 0);
        assert!(summary.max_abs_coefficient > 0);
        assert!(summary.coefficient_abs_sum > 0);
    }

    let coefficient_planes = full_decoder.coefficient_planes();
    assert_eq!(coefficient_planes.len(), 3);
    assert_eq!(coefficient_planes[0].plane, Iw44Plane::Y);
    assert_eq!(coefficient_planes[0].width, 780);
    assert_eq!(coefficient_planes[0].height, 817);
    assert_eq!(coefficient_planes[0].coefficients.len(), 780 * 817);
    let plane_non_zero = coefficient_planes[0]
        .coefficients
        .iter()
        .filter(|coefficient| **coefficient != 0)
        .count();
    let plane_abs_sum = coefficient_planes[0]
        .coefficients
        .iter()
        .map(|coefficient| u64::from(coefficient.unsigned_abs()))
        .sum::<u64>();
    assert!(plane_non_zero > 0);
    assert!(plane_non_zero <= summaries[0].non_zero_coefficients);
    assert!(plane_abs_sum > 0);
    assert!(plane_abs_sum <= summaries[0].coefficient_abs_sum);

    let reconstructed = full_decoder.reconstruct_planes();
    assert_eq!(reconstructed.len(), 3);
    assert_eq!(reconstructed[0].plane, Iw44Plane::Y);
    assert_eq!(reconstructed[0].width, 780);
    assert_eq!(reconstructed[0].height, 817);
    assert_eq!(reconstructed[0].samples.len(), 780 * 817);
    assert!(reconstructed[0].samples.iter().any(|sample| *sample != 0));

    let rgb = full_decoder
        .to_rgb_image()
        .expect("decoded BG44 layer should convert to RGB");
    assert_eq!(rgb.width, 780);
    assert_eq!(rgb.height, 817);
    assert_eq!(rgb.pixels.len(), 780 * 817 * 3);
    assert!(rgb.pixels.iter().any(|component| *component != 0xff));
}

#[test]
fn reconstructs_rypka_page_961_background_without_saturating_wavelet_artifact() {
    const RYPKA: &[u8] = include_bytes!("../../fixtures/Rypka-HIL.djvu");
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

    let mut decoder = Iw44Decoder::new();
    for payload in plan.background_layer_payloads(RYPKA) {
        decoder
            .decode_chunk(payload.bytes)
            .expect("BG44 chunk should decode");
    }

    let reconstructed = decoder.reconstruct_planes();
    let y = reconstructed
        .iter()
        .find(|plane| plane.plane == Iw44Plane::Y)
        .expect("Y plane should reconstruct");
    assert_eq!(y.width, 3486);
    assert_eq!(y.height, 2783);
    assert_eq!(y.samples[1948 * y.width + 1167], 8202);

    let rgb = decoder
        .to_rgb_image()
        .expect("decoded BG44 layer should convert to RGB");
    let output_y = rgb.height - 1 - 1948;
    let offset = (output_y * rgb.width + 1167) * 3;
    assert_eq!(&rgb.pixels[offset..offset + 3], &[0xff, 0xff, 0xff]);
}

#[test]
fn plane_slice_decodes_new_band_zero_coefficient() {
    let mut plane = Iw44PlaneState::new(32, 32);
    let mut bitstream = Iw44Bitstream::new(&[0x00, 0x00]);

    let slice = plane.decode_slice(Iw44Plane::Y, &mut bitstream, 0, None, &mut |_| {});

    assert_eq!(
        slice,
        Iw44PlaneSlice {
            plane: Iw44Plane::Y,
            band: 0,
            first_bucket: 0,
            last_bucket: 0,
            quant: 0x0002_0000,
            is_null: false,
            block_count: 1,
            scheduled_bucket_count: 1,
        }
    );
    assert_eq!(plane.coefficients.coefficient(0, 0), -0x5800);
    assert_eq!(plane.coefficients.coefficient(0, 1), 0);
    assert_eq!(plane.coefficients.bucket_passes(0, 0), 1);
}

#[test]
fn plane_slice_reports_target_coefficient_events() {
    let mut plane = Iw44PlaneState::new(32, 32);
    let mut bitstream = Iw44Bitstream::new(&[0x00, 0x00]);
    let target = Iw44CoefficientTraceTarget {
        plane: Iw44Plane::Y,
        block: 0,
        coefficient: 0,
    };
    let mut events = Vec::new();

    plane.decode_slice(
        Iw44Plane::Y,
        &mut bitstream,
        7,
        Some(target),
        &mut |event| {
            events.push(event);
        },
    );

    assert_eq!(events.len(), 3);
    assert_eq!(
        events[0].kind,
        Iw44CoefficientEventKind::BucketDecision {
            context: 0,
            decision: true,
            block_state: IW44_FLAG_ZERO | IW44_FLAG_NEW | IW44_FLAG_UNKNOWN,
            bucket_state: IW44_FLAG_ZERO | IW44_FLAG_UNKNOWN,
        }
    );
    assert_eq!(
        events[1].kind,
        Iw44CoefficientEventKind::ActivationDecision {
            context: 1,
            decision: true,
            unknown_count: 1,
        }
    );
    assert_eq!(
        events[2],
        Iw44CoefficientEvent {
            slice_index: 7,
            plane: Iw44Plane::Y,
            band: 0,
            block: 0,
            coefficient: 0,
            bucket: 0,
            bucket_offset: 0,
            quant: 0x0000_4000,
            before: 0,
            after: -0x5800,
            kind: Iw44CoefficientEventKind::Activated { sign: -1 },
        }
    );
}

#[test]
fn reconstruct_plane_with_coefficient_values_applies_overrides() {
    let mut decoder = Iw44Decoder {
        image: Some(Iw44ImageHeader {
            major_version: 1,
            minor_version: 2,
            width: 32,
            height: 32,
            grayscale: true,
            delay: 0,
            chroma_half: false,
        }),
        chunks_decoded: 1,
        slices_decoded: 1,
        payload_bytes_seen: 0,
        y: Iw44PlaneState::new(32, 32),
        cb: None,
        cr: None,
    };
    decoder.y.coefficients.set_coefficient(0, 0, 1024);

    let original = decoder.y.reconstruct_plane(Iw44Plane::Y);
    let zeroed = decoder
        .reconstruct_plane_with_coefficient_values(Iw44Plane::Y, &[(0, 0, 0)])
        .expect("override should reconstruct");

    assert!(original.samples.iter().any(|sample| *sample != 0));
    assert!(zeroed.samples.iter().all(|sample| *sample == 0));
}

#[test]
fn reconstruct_planes_with_order_keeps_default_order_available() {
    let mut decoder = Iw44Decoder {
        image: Some(Iw44ImageHeader {
            major_version: 1,
            minor_version: 2,
            width: 32,
            height: 32,
            grayscale: true,
            delay: 0,
            chroma_half: false,
        }),
        chunks_decoded: 1,
        slices_decoded: 1,
        payload_bytes_seen: 0,
        y: Iw44PlaneState::new(32, 32),
        cb: None,
        cr: None,
    };
    decoder.y.coefficients.set_coefficient(0, 1, 1024);

    let default = decoder.reconstruct_planes();
    let explicit = decoder.reconstruct_planes_with_order(Iw44ReconstructionOrder::ColumnsThenRows);
    let alternate = decoder.reconstruct_planes_with_order(Iw44ReconstructionOrder::RowsThenColumns);
    let padded = decoder.reconstruct_planes_with_options(
        Iw44ReconstructionOrder::ColumnsThenRows,
        Iw44ReconstructionExtent::Padded,
    );

    assert_eq!(default, explicit);
    assert_eq!(alternate.len(), 1);
    assert_eq!(alternate[0].plane, Iw44Plane::Y);
    assert_eq!(padded.len(), 1);
    assert_eq!(padded[0].plane, Iw44Plane::Y);
}

#[test]
fn coefficient_buffer_scatters_zigzag_blocks_to_row_major_plane() {
    let mut coefficients = Iw44CoefficientBuffer::new(33, 17);
    coefficients.set_coefficient(0, 0, 11);
    coefficients.set_coefficient(0, 1, 12);
    coefficients.set_coefficient(0, 2, 13);
    coefficients.set_coefficient(0, 3, 14);
    coefficients.set_coefficient(1, 0, 21);

    let plane = coefficients.to_row_major_plane();

    assert_eq!(plane.len(), 33 * 17);
    assert_eq!(plane[0], 11);
    assert_eq!(plane[16], 12);
    assert_eq!(plane[16 * 33], 13);
    assert_eq!(plane[(16 * 33) + 16], 14);
    assert_eq!(plane[32], 21);
}

#[test]
fn inverse_wavelet_line_reconstructs_lifted_samples() {
    let mut line = [10, 2, -5, 4, 8, -3];

    inverse_wavelet_line(&mut line);

    assert_eq!(line, [10, 4, -7, 5, 8, 5]);
}

#[test]
fn ycbcr_pixel_to_rgb_uses_djvu_conversion_formula() {
    assert_eq!(ycbcr_pixel_to_rgb(0, 0, 0), [128, 128, 128]);
    assert_eq!(ycbcr_pixel_to_rgb(10, -20, 30), [183, 121, 103]);
    assert_eq!(ycbcr_pixel_to_rgb(127, 127, 127), [255, 129, 255]);
    assert_eq!(ycbcr_pixel_to_rgb(-128, -128, -128), [0, 128, 0]);
}

#[test]
fn grayscale_reconstruction_plane_converts_to_rgb() {
    let plane = Iw44ReconstructionPlane {
        plane: Iw44Plane::Y,
        width: 2,
        height: 2,
        samples: vec![-8192, 0, 64, 8191],
    };

    let rgb = grayscale_to_rgb(&plane);

    assert_eq!(rgb.width, 2);
    assert_eq!(rgb.height, 2);
    assert_eq!(
        rgb.pixels,
        vec![126, 126, 126, 0, 0, 0, 255, 255, 255, 127, 127, 127]
    );
}

#[test]
fn iw44_bitstream_reads_passthrough_bits() {
    let mut zeros = Iw44Bitstream::new(&[0x00, 0x00]);
    let mut ones = Iw44Bitstream::new(&[0xff, 0xff]);

    assert!(zeros.decode_passthrough_bit());
    assert!(!ones.decode_passthrough_bit());
}

#[test]
fn iw44_bitstream_reads_context_bits_and_updates_state() {
    let mut wrapped = Iw44Bitstream::new(&[0xff, 0xff]);
    let mut direct = SpecZpDecoder::new(&[0xff, 0xff]);
    let mut wrapped_context = 0;
    let mut direct_context = 0;

    let wrapped_bit = wrapped.decode_context_bit(&mut wrapped_context);
    let direct_bit = direct.decode_context_bit(&mut direct_context);

    assert_eq!(wrapped_bit, direct_bit);
    assert_eq!(wrapped_context, direct_context);
    assert_ne!(wrapped_context, 0);
}

#[test]
fn rejects_empty_iw44_layer() {
    let chunks: [&[u8]; 0] = [];
    let error = summarize_iw44_layer(chunks).expect_err("empty layer should fail");

    assert_eq!(error, Iw44Error::new("IW44 layer has no chunks"));
}

#[test]
fn rejects_iw44_layer_without_first_chunk() {
    let error = summarize_iw44_layer([[0x01, 0x0a].as_slice()])
        .expect_err("missing first chunk should fail");

    assert_eq!(
        error,
        Iw44Error::new("IW44 chunk serial 1 does not match expected 0")
    );
}

#[test]
fn rejects_iw44_layer_with_serial_gap() {
    let first = [0x00, 0x4a, 0x01, 0x02, 0x03, 0x0c, 0x03, 0x31, 0x8a, 0xff];
    let third = [0x02, 0x0a, 0xaa];
    let error = summarize_iw44_layer([first.as_slice(), third.as_slice()])
        .expect_err("serial gap should fail");

    assert_eq!(
        error,
        Iw44Error::new("IW44 chunk serial 2 does not match expected 1")
    );
}
