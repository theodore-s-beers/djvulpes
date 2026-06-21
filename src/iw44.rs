use crate::bzz::SpecZpDecoder;
use std::fmt;

const IW44_QUANT_LO_INIT: [u32; 16] = [
    0x0000_4000,
    0x0000_8000,
    0x0000_8000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0001_0000,
    0x0002_0000,
    0x0002_0000,
    0x0002_0000,
    0x0002_0000,
];
const IW44_QUANT_HI_INIT: [u32; 10] = [
    0,
    0x0002_0000,
    0x0002_0000,
    0x0004_0000,
    0x0004_0000,
    0x0004_0000,
    0x0008_0000,
    0x0004_0000,
    0x0004_0000,
    0x0008_0000,
];
const IW44_BAND_BUCKETS: [(u8, u8); 10] = [
    (0, 0),
    (1, 1),
    (2, 2),
    (3, 3),
    (4, 7),
    (8, 11),
    (12, 15),
    (16, 31),
    (32, 47),
    (48, 63),
];
const IW44_BUCKETS_PER_BLOCK: usize = 64;
const IW44_COEFFICIENTS_PER_BUCKET: usize = 16;
const IW44_COEFFICIENTS_PER_BLOCK: usize = IW44_BUCKETS_PER_BLOCK * IW44_COEFFICIENTS_PER_BUCKET;
const IW44_FLAG_ZERO: u8 = 1;
const IW44_FLAG_ACTIVE: u8 = 2;
const IW44_FLAG_NEW: u8 = 4;
const IW44_FLAG_UNKNOWN: u8 = 8;

pub type Iw44Result<T> = Result<T, Iw44Error>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44Error(String);

impl Iw44Error {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Iw44Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Iw44Error {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44ChunkHeader {
    pub serial: u8,
    pub slices: u8,
    pub image: Option<Iw44ImageHeader>,
    pub payload_start: usize,
    pub payload_len: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44ImageHeader {
    pub major_version: u8,
    pub minor_version: u8,
    pub width: u16,
    pub height: u16,
    pub grayscale: bool,
    pub delay: u8,
    pub chroma_half: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44LayerSummary {
    pub image: Iw44ImageHeader,
    pub chunks: Vec<Iw44ChunkHeader>,
    pub total_slices: u32,
    pub total_payload_bytes: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44PageMapping {
    pub page_width: u32,
    pub page_height: u32,
    pub layer_width: u32,
    pub layer_height: u32,
    pub subsample: u32,
    pub scaled_width: u32,
    pub scaled_height: u32,
    pub horizontal_overscan: u32,
    pub vertical_overscan: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44Decoder {
    image: Option<Iw44ImageHeader>,
    chunks_decoded: usize,
    slices_decoded: u32,
    payload_bytes_seen: usize,
    y: Iw44PlaneState,
    cb: Option<Iw44PlaneState>,
    cr: Option<Iw44PlaneState>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44DecodedChunk {
    pub header: Iw44ChunkHeader,
    pub first_slice_index: u32,
    pub slice_count: u8,
    pub slices: Vec<Iw44DecodedSlice>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44DecodedSlice {
    pub index: u32,
    pub planes: Vec<Iw44PlaneSlice>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44PlaneSlice {
    pub plane: Iw44Plane,
    pub band: u8,
    pub first_bucket: u8,
    pub last_bucket: u8,
    pub quant: u32,
    pub is_null: bool,
    pub block_count: usize,
    pub scheduled_bucket_count: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44PlaneCoefficientSummary {
    pub plane: Iw44Plane,
    pub width: usize,
    pub height: usize,
    pub block_count: usize,
    pub non_zero_coefficients: usize,
    pub max_abs_coefficient: u16,
    pub coefficient_abs_sum: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44CoefficientPlane {
    pub plane: Iw44Plane,
    pub width: usize,
    pub height: usize,
    pub coefficients: Vec<i16>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44ReconstructionPlane {
    pub plane: Iw44Plane,
    pub width: usize,
    pub height: usize,
    pub samples: Vec<i16>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Iw44RgbImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Iw44Plane {
    Y,
    Cb,
    Cr,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Iw44ReconstructionOrder {
    ColumnsThenRows,
    RowsThenColumns,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Iw44ReconstructionExtent {
    Visible,
    Padded,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44CoefficientTraceTarget {
    pub plane: Iw44Plane,
    pub block: usize,
    pub coefficient: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Iw44CoefficientEventKind {
    BucketDecision {
        context: usize,
        decision: bool,
        block_state: u8,
        bucket_state: u8,
    },
    ActivationDecision {
        context: usize,
        decision: bool,
        unknown_count: usize,
    },
    Activated {
        sign: i32,
    },
    RefinementDecision {
        context_coded: bool,
        decision: bool,
        magnitude: i32,
    },
    Refined,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Iw44CoefficientEvent {
    pub slice_index: u32,
    pub plane: Iw44Plane,
    pub band: u8,
    pub block: usize,
    pub coefficient: usize,
    pub bucket: usize,
    pub bucket_offset: usize,
    pub quant: u32,
    pub before: i16,
    pub after: i16,
    pub kind: Iw44CoefficientEventKind,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44PlaneState {
    width: usize,
    height: usize,
    block_count: usize,
    current_band: u8,
    quant_lo: [u32; 16],
    quant_hi: [u32; 10],
    coefficients: Iw44CoefficientBuffer,
    ctx_decode_bucket: [u8; 1],
    ctx_decode_coef: [u8; 80],
    ctx_activate_coef: [u8; 16],
    ctx_increase_coef: [u8; 1],
    coeffstate: [[u8; IW44_COEFFICIENTS_PER_BUCKET]; IW44_COEFFICIENTS_PER_BUCKET],
    bucketstate: [u8; IW44_COEFFICIENTS_PER_BUCKET],
    block_band_state: u8,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Iw44CoefficientBuffer {
    width: usize,
    height: usize,
    block_columns: usize,
    block_rows: usize,
    blocks: Vec<[i16; IW44_COEFFICIENTS_PER_BLOCK]>,
    buckets: Vec<Iw44CoefficientBucket>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
struct Iw44CoefficientBucket {
    passes: u32,
}

#[derive(Debug, Clone, Copy)]
struct Iw44CoefficientEventTemplate {
    slice_index: u32,
    plane: Iw44Plane,
    band: u8,
    target: Iw44CoefficientTraceTarget,
    quant: u32,
}

pub struct Iw44Bitstream<'a> {
    zp: SpecZpDecoder<'a>,
}

impl Iw44LayerSummary {
    #[must_use]
    pub fn page_mapping(&self, page_width: u32, page_height: u32) -> Iw44PageMapping {
        let layer_width = u32::from(self.image.width);
        let layer_height = u32::from(self.image.height);
        let subsample = page_width
            .div_ceil(layer_width)
            .max(page_height.div_ceil(layer_height))
            .max(1);
        let scaled_width = layer_width.saturating_mul(subsample);
        let scaled_height = layer_height.saturating_mul(subsample);

        Iw44PageMapping {
            page_width,
            page_height,
            layer_width,
            layer_height,
            subsample,
            scaled_width,
            scaled_height,
            horizontal_overscan: scaled_width.saturating_sub(page_width),
            vertical_overscan: scaled_height.saturating_sub(page_height),
        }
    }
}

impl Iw44Decoder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            image: None,
            chunks_decoded: 0,
            slices_decoded: 0,
            payload_bytes_seen: 0,
            y: Iw44PlaneState::empty(),
            cb: None,
            cr: None,
        }
    }

    #[must_use]
    pub const fn image(&self) -> Option<Iw44ImageHeader> {
        self.image
    }

    #[must_use]
    pub const fn chunks_decoded(&self) -> usize {
        self.chunks_decoded
    }

    #[must_use]
    pub const fn slices_decoded(&self) -> u32 {
        self.slices_decoded
    }

    #[must_use]
    pub const fn payload_bytes_seen(&self) -> usize {
        self.payload_bytes_seen
    }

    #[must_use]
    pub fn plane_coefficient_summaries(&self) -> Vec<Iw44PlaneCoefficientSummary> {
        let mut summaries = Vec::with_capacity(3);
        if self.image.is_some() {
            summaries.push(self.y.coefficient_summary(Iw44Plane::Y));
            if let Some(cb) = &self.cb {
                summaries.push(cb.coefficient_summary(Iw44Plane::Cb));
            }
            if let Some(cr) = &self.cr {
                summaries.push(cr.coefficient_summary(Iw44Plane::Cr));
            }
        }
        summaries
    }

    #[must_use]
    pub fn coefficient_planes(&self) -> Vec<Iw44CoefficientPlane> {
        let mut planes = Vec::with_capacity(3);
        if self.image.is_some() {
            planes.push(self.y.coefficient_plane(Iw44Plane::Y));
            if let Some(cb) = &self.cb {
                planes.push(cb.coefficient_plane(Iw44Plane::Cb));
            }
            if let Some(cr) = &self.cr {
                planes.push(cr.coefficient_plane(Iw44Plane::Cr));
            }
        }
        planes
    }

    #[must_use]
    pub fn reconstruct_planes(&self) -> Vec<Iw44ReconstructionPlane> {
        self.reconstruct_planes_with_options(
            Iw44ReconstructionOrder::ColumnsThenRows,
            Iw44ReconstructionExtent::Visible,
        )
    }

    #[must_use]
    pub fn reconstruct_planes_with_order(
        &self,
        order: Iw44ReconstructionOrder,
    ) -> Vec<Iw44ReconstructionPlane> {
        self.reconstruct_planes_with_options(order, Iw44ReconstructionExtent::Visible)
    }

    #[must_use]
    pub fn reconstruct_planes_with_options(
        &self,
        order: Iw44ReconstructionOrder,
        extent: Iw44ReconstructionExtent,
    ) -> Vec<Iw44ReconstructionPlane> {
        let mut planes = Vec::with_capacity(3);
        if self.image.is_some() {
            planes.push(
                self.y
                    .reconstruct_plane_with_options(Iw44Plane::Y, order, extent),
            );
            if let Some(cb) = &self.cb {
                planes.push(cb.reconstruct_plane_with_options(Iw44Plane::Cb, order, extent));
            }
            if let Some(cr) = &self.cr {
                planes.push(cr.reconstruct_plane_with_options(Iw44Plane::Cr, order, extent));
            }
        }
        planes
    }

    #[must_use]
    pub fn reconstruct_plane_with_coefficient_value(
        &self,
        plane: Iw44Plane,
        block: usize,
        coefficient: usize,
        value: i16,
    ) -> Option<Iw44ReconstructionPlane> {
        self.reconstruct_plane_with_coefficient_values(plane, &[(block, coefficient, value)])
    }

    #[must_use]
    pub fn reconstruct_plane_with_coefficient_values(
        &self,
        plane: Iw44Plane,
        overrides: &[(usize, usize, i16)],
    ) -> Option<Iw44ReconstructionPlane> {
        let mut plane_state = match plane {
            Iw44Plane::Y => self.y.clone(),
            Iw44Plane::Cb => self.cb.clone()?,
            Iw44Plane::Cr => self.cr.clone()?,
        };
        for &(block, coefficient, value) in overrides {
            if block >= plane_state.coefficients.block_count()
                || coefficient >= IW44_COEFFICIENTS_PER_BLOCK
            {
                return None;
            }
            plane_state
                .coefficients
                .set_coefficient(block, coefficient, value);
        }

        Some(plane_state.reconstruct_plane(plane))
    }

    /// Reconstructs the IW44 layer and converts it to RGB pixels.
    ///
    /// # Errors
    ///
    /// Returns an error if no IW44 chunk has initialized the decoder, or if a
    /// color image is missing chroma planes.
    pub fn to_rgb_image(&self) -> Iw44Result<Iw44RgbImage> {
        let image = self
            .image
            .ok_or_else(|| Iw44Error::new("IW44 decoder has no image header"))?;
        let y = self.y.reconstruct_plane(Iw44Plane::Y);

        if image.grayscale {
            return Ok(grayscale_to_rgb(&y));
        }

        let cb = self
            .cb
            .as_ref()
            .ok_or_else(|| Iw44Error::new("IW44 color image is missing Cb plane"))?
            .reconstruct_plane(Iw44Plane::Cb);
        let cr = self
            .cr
            .as_ref()
            .ok_or_else(|| Iw44Error::new("IW44 color image is missing Cr plane"))?
            .reconstruct_plane(Iw44Plane::Cr);

        Ok(ycbcr_to_rgb(&y, &cb, &cr, image.chroma_half))
    }

    /// Advances the progressive IW44 decoder by one raw chunk.
    ///
    /// This currently validates chunk sequencing and accounts for progressive
    /// slices. Coefficient decoding will fill in the per-slice body.
    ///
    /// # Errors
    ///
    /// Returns an error if the chunk header is malformed, if the first chunk is
    /// missing, or if the serial number is not the next expected chunk.
    pub fn decode_chunk(&mut self, bytes: &[u8]) -> Iw44Result<Iw44DecodedChunk> {
        self.decode_chunk_with_observers(bytes, |_, _| {}, None, |_| {})
    }

    /// Advances the progressive IW44 decoder by one raw chunk, calling
    /// `observer` after each decoded slice.
    ///
    /// # Errors
    ///
    /// Returns an error if the chunk header is malformed, if the first chunk is
    /// missing, or if the serial number is not the next expected chunk.
    pub fn decode_chunk_with_slice_observer(
        &mut self,
        bytes: &[u8],
        mut observer: impl FnMut(&Self, &Iw44DecodedSlice),
    ) -> Iw44Result<Iw44DecodedChunk> {
        self.decode_chunk_with_observers(
            bytes,
            |decoder, slice| observer(decoder, slice),
            None,
            |_| {},
        )
    }

    /// Advances the progressive IW44 decoder by one raw chunk, calling
    /// `observer` for coding decisions that affect `target`.
    ///
    /// # Errors
    ///
    /// Returns an error if the chunk header is malformed, if the first chunk is
    /// missing, or if the serial number is not the next expected chunk.
    pub fn decode_chunk_with_coefficient_observer(
        &mut self,
        bytes: &[u8],
        target: Iw44CoefficientTraceTarget,
        observer: impl FnMut(Iw44CoefficientEvent),
    ) -> Iw44Result<Iw44DecodedChunk> {
        self.decode_chunk_with_observers(bytes, |_, _| {}, Some(target), observer)
    }

    fn decode_chunk_with_observers(
        &mut self,
        bytes: &[u8],
        mut slice_observer: impl FnMut(&Self, &Iw44DecodedSlice),
        trace_target: Option<Iw44CoefficientTraceTarget>,
        mut event_observer: impl FnMut(Iw44CoefficientEvent),
    ) -> Iw44Result<Iw44DecodedChunk> {
        let header = read_iw44_chunk_header(bytes)?;
        let expected_serial = u8::try_from(self.chunks_decoded).map_err(|_| {
            Iw44Error::new(format!(
                "IW44 decoder has already consumed {} chunks",
                usize::from(u8::MAX) + 1
            ))
        })?;
        if header.serial != expected_serial {
            return Err(Iw44Error::new(format!(
                "IW44 chunk serial {} does not match expected {expected_serial}",
                header.serial
            )));
        }
        if self.chunks_decoded == 0 {
            let image = header
                .image
                .ok_or_else(|| Iw44Error::new("IW44 first chunk has no image header"))?;
            self.image = Some(image);
            self.y = Iw44PlaneState::new(usize::from(image.width), usize::from(image.height));
            if !image.grayscale {
                let (chroma_width, chroma_height) = if image.chroma_half {
                    (
                        usize::from(image.width).div_ceil(2),
                        usize::from(image.height).div_ceil(2),
                    )
                } else {
                    (usize::from(image.width), usize::from(image.height))
                };
                self.cb = Some(Iw44PlaneState::new(chroma_width, chroma_height));
                self.cr = Some(Iw44PlaneState::new(chroma_width, chroma_height));
            }
        } else if header.image.is_some() {
            return Err(Iw44Error::new(
                "IW44 subsequent chunk unexpectedly has image header",
            ));
        }
        let image = self
            .image
            .ok_or_else(|| Iw44Error::new("IW44 decoder has no image header"))?;
        let mut bitstream = Iw44Bitstream::new(&bytes[header.payload_start..]);
        let mut slices = Vec::with_capacity(usize::from(header.slices));

        for _ in 0..header.slices {
            let index = self.slices_decoded;
            let mut planes = Vec::with_capacity(3);
            planes.push(self.y.decode_slice(
                Iw44Plane::Y,
                &mut bitstream,
                index,
                trace_target,
                &mut event_observer,
            ));
            if !image.grayscale && index + 1 > u32::from(image.delay) {
                if let Some(cb) = self.cb.as_mut() {
                    planes.push(cb.decode_slice(
                        Iw44Plane::Cb,
                        &mut bitstream,
                        index,
                        trace_target,
                        &mut event_observer,
                    ));
                }
                if let Some(cr) = self.cr.as_mut() {
                    planes.push(cr.decode_slice(
                        Iw44Plane::Cr,
                        &mut bitstream,
                        index,
                        trace_target,
                        &mut event_observer,
                    ));
                }
            }
            let slice = Iw44DecodedSlice { index, planes };
            self.slices_decoded += 1;
            slice_observer(self, &slice);
            slices.push(slice);
        }

        let first_slice_index = self.slices_decoded - u32::from(header.slices);
        self.chunks_decoded += 1;
        self.payload_bytes_seen += header.payload_len;

        Ok(Iw44DecodedChunk {
            header,
            first_slice_index,
            slice_count: header.slices,
            slices,
        })
    }
}

impl Iw44PlaneState {
    const fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            block_count: 0,
            current_band: 0,
            quant_lo: IW44_QUANT_LO_INIT,
            quant_hi: IW44_QUANT_HI_INIT,
            coefficients: Iw44CoefficientBuffer::empty(),
            ctx_decode_bucket: [0; 1],
            ctx_decode_coef: [0; 80],
            ctx_activate_coef: [0; 16],
            ctx_increase_coef: [0; 1],
            coeffstate: [[0; IW44_COEFFICIENTS_PER_BUCKET]; IW44_COEFFICIENTS_PER_BUCKET],
            bucketstate: [0; IW44_COEFFICIENTS_PER_BUCKET],
            block_band_state: 0,
        }
    }

    fn new(width: usize, height: usize) -> Self {
        let block_count = width.div_ceil(32).saturating_mul(height.div_ceil(32));

        Self {
            width,
            height,
            block_count,
            current_band: 0,
            quant_lo: IW44_QUANT_LO_INIT,
            quant_hi: IW44_QUANT_HI_INIT,
            coefficients: Iw44CoefficientBuffer::new(width, height),
            ctx_decode_bucket: [0; 1],
            ctx_decode_coef: [0; 80],
            ctx_activate_coef: [0; 16],
            ctx_increase_coef: [0; 1],
            coeffstate: [[0; IW44_COEFFICIENTS_PER_BUCKET]; IW44_COEFFICIENTS_PER_BUCKET],
            bucketstate: [0; IW44_COEFFICIENTS_PER_BUCKET],
            block_band_state: 0,
        }
    }

    fn decode_slice(
        &mut self,
        plane: Iw44Plane,
        bitstream: &mut Iw44Bitstream<'_>,
        slice_index: u32,
        trace_target: Option<Iw44CoefficientTraceTarget>,
        event_observer: &mut impl FnMut(Iw44CoefficientEvent),
    ) -> Iw44PlaneSlice {
        let band = self.current_band;
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(band)];
        let quant = self.current_quant();
        let is_null = self.prepare_slice_flags();
        let scheduled_bucket_count = self.record_slice_body(
            bitstream,
            is_null,
            plane,
            slice_index,
            trace_target,
            event_observer,
        );
        self.finish_slice();

        Iw44PlaneSlice {
            plane,
            band,
            first_bucket,
            last_bucket,
            quant,
            is_null,
            block_count: self.block_count,
            scheduled_bucket_count,
        }
    }

    fn current_quant(&self) -> u32 {
        if self.current_band == 0 {
            self.quant_lo.iter().copied().max().unwrap_or(0)
        } else {
            self.quant_hi[usize::from(self.current_band)]
        }
    }

    fn coefficient_summary(&self, plane: Iw44Plane) -> Iw44PlaneCoefficientSummary {
        let (non_zero_coefficients, max_abs_coefficient, coefficient_abs_sum) =
            self.coefficients.summary();

        Iw44PlaneCoefficientSummary {
            plane,
            width: self.width,
            height: self.height,
            block_count: self.block_count,
            non_zero_coefficients,
            max_abs_coefficient,
            coefficient_abs_sum,
        }
    }

    fn coefficient_plane(&self, plane: Iw44Plane) -> Iw44CoefficientPlane {
        Iw44CoefficientPlane {
            plane,
            width: self.width,
            height: self.height,
            coefficients: self.coefficients.to_row_major_plane(),
        }
    }

    fn reconstruct_plane(&self, plane: Iw44Plane) -> Iw44ReconstructionPlane {
        self.reconstruct_plane_with_options(
            plane,
            Iw44ReconstructionOrder::ColumnsThenRows,
            Iw44ReconstructionExtent::Visible,
        )
    }

    fn reconstruct_plane_with_options(
        &self,
        plane: Iw44Plane,
        order: Iw44ReconstructionOrder,
        extent: Iw44ReconstructionExtent,
    ) -> Iw44ReconstructionPlane {
        Iw44ReconstructionPlane {
            plane,
            width: self.width,
            height: self.height,
            samples: self.coefficients.reconstruct_with_options(order, extent),
        }
    }

    fn prepare_slice_flags(&mut self) -> bool {
        if self.current_band == 0 {
            let mut is_null = true;
            for (index, quant) in self.quant_lo.iter().copied().enumerate() {
                self.coeffstate[0][index] = IW44_FLAG_ZERO;
                if quant > 0 && quant < 0x8000 {
                    self.coeffstate[0][index] = IW44_FLAG_UNKNOWN;
                    is_null = false;
                }
            }
            is_null
        } else {
            let quant = self.quant_hi[usize::from(self.current_band)];
            !(quant > 0 && quant < 0x8000)
        }
    }

    fn decode_slice_body(
        &mut self,
        bitstream: &mut Iw44Bitstream<'_>,
        plane: Iw44Plane,
        slice_index: u32,
        trace_target: Option<Iw44CoefficientTraceTarget>,
        event_observer: &mut impl FnMut(Iw44CoefficientEvent),
    ) {
        for block_index in 0..self.coefficients.block_count() {
            self.preliminary_flag_computation(block_index);
            if self.block_band_decoding_pass(bitstream)
                && self.bucket_decoding_pass(
                    bitstream,
                    block_index,
                    plane,
                    slice_index,
                    trace_target,
                    event_observer,
                )
            {
                self.newly_active_coefficient_decoding_pass(
                    bitstream,
                    block_index,
                    plane,
                    slice_index,
                    trace_target,
                    event_observer,
                );
            }
            if (self.block_band_state & IW44_FLAG_ACTIVE) != 0 {
                self.previously_active_coefficient_decoding_pass(
                    bitstream,
                    block_index,
                    plane,
                    slice_index,
                    trace_target,
                    event_observer,
                );
            }
        }
    }

    fn preliminary_flag_computation(&mut self, block_index: usize) {
        self.block_band_state = 0;
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(self.current_band)];

        if self.current_band == 0 {
            let mut bucket_state = 0;
            for coefficient_index in 0..IW44_COEFFICIENTS_PER_BUCKET {
                if self.coeffstate[0][coefficient_index] != IW44_FLAG_ZERO {
                    self.coeffstate[0][coefficient_index] = if self
                        .coefficients
                        .coefficient(block_index, coefficient_index)
                        == 0
                    {
                        IW44_FLAG_UNKNOWN
                    } else {
                        IW44_FLAG_ACTIVE
                    };
                }
                bucket_state |= self.coeffstate[0][coefficient_index];
            }
            self.bucketstate[0] = bucket_state;
            self.block_band_state |= bucket_state;
        } else {
            for (bucket_offset, bucket_index) in (first_bucket..=last_bucket).enumerate() {
                let mut bucket_state = 0;
                let base = usize::from(bucket_index) * IW44_COEFFICIENTS_PER_BUCKET;
                for coefficient_offset in 0..IW44_COEFFICIENTS_PER_BUCKET {
                    let state = if self
                        .coefficients
                        .coefficient(block_index, base + coefficient_offset)
                        == 0
                    {
                        IW44_FLAG_UNKNOWN
                    } else {
                        IW44_FLAG_ACTIVE
                    };
                    self.coeffstate[bucket_offset][coefficient_offset] = state;
                    bucket_state |= state;
                }
                self.bucketstate[bucket_offset] = bucket_state;
                self.block_band_state |= bucket_state;
            }
        }
    }

    fn block_band_decoding_pass(&mut self, bitstream: &mut Iw44Bitstream<'_>) -> bool {
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(self.current_band)];
        let bucket_count = usize::from(last_bucket - first_bucket + 1);
        let should_mark_new = bucket_count < IW44_COEFFICIENTS_PER_BUCKET
            || (self.block_band_state & IW44_FLAG_ACTIVE) != 0
            || ((self.block_band_state & IW44_FLAG_UNKNOWN) != 0
                && bitstream.decode_context_bit(&mut self.ctx_decode_bucket[0]));
        if should_mark_new {
            self.block_band_state |= IW44_FLAG_NEW;
        }
        (self.block_band_state & IW44_FLAG_NEW) != 0
    }

    fn bucket_decoding_pass(
        &mut self,
        bitstream: &mut Iw44Bitstream<'_>,
        block_index: usize,
        plane: Iw44Plane,
        slice_index: u32,
        trace_target: Option<Iw44CoefficientTraceTarget>,
        event_observer: &mut impl FnMut(Iw44CoefficientEvent),
    ) -> bool {
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(self.current_band)];
        let mut any_new = false;

        for (bucket_offset, bucket_index) in (first_bucket..=last_bucket).enumerate() {
            if (self.bucketstate[bucket_offset] & IW44_FLAG_UNKNOWN) == 0 {
                continue;
            }

            let mut context_index = if self.current_band == 0 {
                0
            } else {
                let base = usize::from(bucket_index) * 4;
                (base..base + 4)
                    .filter(|coefficient| {
                        self.coefficients.coefficient(block_index, *coefficient) != 0
                    })
                    .count()
                    .min(3)
            };
            if (self.block_band_state & IW44_FLAG_ACTIVE) != 0 {
                context_index |= 4;
            }
            context_index += usize::from(self.current_band) * 8;

            let decision = bitstream.decode_context_bit(&mut self.ctx_decode_coef[context_index]);
            if let Some(target) =
                trace_target_for_bucket(trace_target, plane, block_index, usize::from(bucket_index))
            {
                let coefficient = self
                    .coefficients
                    .coefficient(block_index, target.coefficient);
                event_observer(
                    coefficient_event_template(
                        slice_index,
                        plane,
                        self.current_band,
                        target,
                        self.current_quant(),
                    )
                    .event(
                        coefficient,
                        coefficient,
                        Iw44CoefficientEventKind::BucketDecision {
                            context: context_index,
                            decision,
                            block_state: self.block_band_state,
                            bucket_state: self.bucketstate[bucket_offset],
                        },
                    ),
                );
            }
            if decision {
                self.bucketstate[bucket_offset] |= IW44_FLAG_NEW;
                any_new = true;
            }
        }

        any_new
    }

    fn newly_active_coefficient_decoding_pass(
        &mut self,
        bitstream: &mut Iw44Bitstream<'_>,
        block_index: usize,
        plane: Iw44Plane,
        slice_index: u32,
        trace_target: Option<Iw44CoefficientTraceTarget>,
        event_observer: &mut impl FnMut(Iw44CoefficientEvent),
    ) {
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(self.current_band)];
        let mut step = self.quant_hi[usize::from(self.current_band)];

        for (bucket_offset, bucket_index) in (first_bucket..=last_bucket).enumerate() {
            if (self.bucketstate[bucket_offset] & IW44_FLAG_NEW) == 0 {
                continue;
            }

            let context_shift = if (self.bucketstate[bucket_offset] & IW44_FLAG_ACTIVE) != 0 {
                8
            } else {
                0
            };
            let mut unknown_count = self.coeffstate[bucket_offset]
                .iter()
                .filter(|state| (**state & IW44_FLAG_UNKNOWN) != 0)
                .count();

            for coefficient_offset in 0..IW44_COEFFICIENTS_PER_BUCKET {
                if (self.coeffstate[bucket_offset][coefficient_offset] & IW44_FLAG_UNKNOWN) == 0 {
                    continue;
                }

                let activation_context = context_shift + unknown_count.min(7);
                let coefficient_index =
                    (usize::from(bucket_index) * IW44_COEFFICIENTS_PER_BUCKET) + coefficient_offset;
                let before = self
                    .coefficients
                    .coefficient(block_index, coefficient_index);
                let is_trace_target =
                    trace_target_matches(trace_target, plane, block_index, coefficient_index);
                let decision =
                    bitstream.decode_context_bit(&mut self.ctx_activate_coef[activation_context]);
                if is_trace_target {
                    event_observer(
                        coefficient_event_template(
                            slice_index,
                            plane,
                            self.current_band,
                            trace_target.expect("trace target should be present when it matches"),
                            self.current_quant(),
                        )
                        .event(
                            before,
                            before,
                            Iw44CoefficientEventKind::ActivationDecision {
                                context: activation_context,
                                decision,
                                unknown_count,
                            },
                        ),
                    );
                }
                if decision {
                    let sign = if bitstream.decode_passthrough_bit() {
                        -1
                    } else {
                        1
                    };
                    // Once one coefficient in the bucket activates, later
                    // unknowns use the lowest activation-count context.
                    unknown_count = 0;
                    if self.current_band == 0 {
                        step = self.quant_lo[coefficient_offset];
                    }
                    let step = i32::try_from(step).expect("IW44 quantization step should fit i32");
                    let value = sign * (step + (step >> 1) - (step >> 3));
                    let after = iw44_coefficient(value);
                    self.coefficients
                        .set_coefficient(block_index, coefficient_index, after);
                    if is_trace_target {
                        event_observer(
                            coefficient_event_template(
                                slice_index,
                                plane,
                                self.current_band,
                                trace_target
                                    .expect("trace target should be present when it matches"),
                                u32::try_from(step).expect("positive IW44 step should fit u32"),
                            )
                            .event(
                                before,
                                after,
                                Iw44CoefficientEventKind::Activated { sign },
                            ),
                        );
                    }
                }
                unknown_count = unknown_count.saturating_sub(1);
            }
        }
    }

    fn previously_active_coefficient_decoding_pass(
        &mut self,
        bitstream: &mut Iw44Bitstream<'_>,
        block_index: usize,
        plane: Iw44Plane,
        slice_index: u32,
        trace_target: Option<Iw44CoefficientTraceTarget>,
        event_observer: &mut impl FnMut(Iw44CoefficientEvent),
    ) {
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(self.current_band)];
        let mut step = self.quant_hi[usize::from(self.current_band)];

        for (bucket_offset, bucket_index) in (first_bucket..=last_bucket).enumerate() {
            for coefficient_offset in 0..IW44_COEFFICIENTS_PER_BUCKET {
                if (self.coeffstate[bucket_offset][coefficient_offset] & IW44_FLAG_ACTIVE) == 0 {
                    continue;
                }

                if self.current_band == 0 {
                    step = self.quant_lo[coefficient_offset];
                }
                let coefficient_index =
                    (usize::from(bucket_index) * IW44_COEFFICIENTS_PER_BUCKET) + coefficient_offset;
                let coefficient = self
                    .coefficients
                    .coefficient(block_index, coefficient_index);
                let mut magnitude = i32::from(coefficient.unsigned_abs());
                let step = i32::try_from(step).expect("IW44 quantization step should fit i32");
                let context_coded = magnitude <= 3 * step;
                let should_increase = if context_coded {
                    magnitude += step >> 2;
                    bitstream.decode_context_bit(&mut self.ctx_increase_coef[0])
                } else {
                    bitstream.decode_passthrough_bit()
                };
                if trace_target_matches(trace_target, plane, block_index, coefficient_index) {
                    event_observer(
                        coefficient_event_template(
                            slice_index,
                            plane,
                            self.current_band,
                            trace_target.expect("trace target should be present when it matches"),
                            u32::try_from(step).expect("positive IW44 step should fit u32"),
                        )
                        .event(
                            coefficient,
                            coefficient,
                            Iw44CoefficientEventKind::RefinementDecision {
                                context_coded,
                                decision: should_increase,
                                magnitude: i32::from(coefficient.unsigned_abs()),
                            },
                        ),
                    );
                }

                if should_increase {
                    magnitude += step >> 1;
                } else {
                    magnitude += -step + (step >> 1);
                }

                let value = if coefficient < 0 {
                    -magnitude
                } else {
                    magnitude
                };
                self.coefficients.set_coefficient(
                    block_index,
                    coefficient_index,
                    iw44_coefficient(value),
                );
                if trace_target_matches(trace_target, plane, block_index, coefficient_index) {
                    event_observer(
                        coefficient_event_template(
                            slice_index,
                            plane,
                            self.current_band,
                            trace_target.expect("trace target should be present when it matches"),
                            u32::try_from(step).expect("positive IW44 step should fit u32"),
                        )
                        .event(
                            coefficient,
                            iw44_coefficient(value),
                            Iw44CoefficientEventKind::Refined,
                        ),
                    );
                }
            }
        }
    }

    fn record_slice_body(
        &mut self,
        bitstream: &mut Iw44Bitstream<'_>,
        is_null: bool,
        plane: Iw44Plane,
        slice_index: u32,
        trace_target: Option<Iw44CoefficientTraceTarget>,
        event_observer: &mut impl FnMut(Iw44CoefficientEvent),
    ) -> usize {
        let (first_bucket, last_bucket) = IW44_BAND_BUCKETS[usize::from(self.current_band)];
        let scheduled_bucket_count =
            self.coefficients
                .mark_bucket_range(first_bucket, last_bucket, is_null);
        if !is_null {
            self.decode_slice_body(bitstream, plane, slice_index, trace_target, event_observer);
        }
        scheduled_bucket_count
    }

    fn finish_slice(&mut self) {
        self.quant_hi[usize::from(self.current_band)] >>= 1;
        if self.current_band == 0 {
            for quant in &mut self.quant_lo {
                *quant >>= 1;
            }
        }
        self.current_band += 1;
        if self.current_band == 10 {
            self.current_band = 0;
        }
    }
}

fn trace_target_matches(
    trace_target: Option<Iw44CoefficientTraceTarget>,
    plane: Iw44Plane,
    block: usize,
    coefficient: usize,
) -> bool {
    trace_target.is_some_and(|target| {
        target.plane == plane && target.block == block && target.coefficient == coefficient
    })
}

fn trace_target_for_bucket(
    trace_target: Option<Iw44CoefficientTraceTarget>,
    plane: Iw44Plane,
    block: usize,
    bucket: usize,
) -> Option<Iw44CoefficientTraceTarget> {
    trace_target.filter(|target| {
        target.plane == plane
            && target.block == block
            && target.coefficient / IW44_COEFFICIENTS_PER_BUCKET == bucket
    })
}

const fn coefficient_event_template(
    slice_index: u32,
    plane: Iw44Plane,
    band: u8,
    target: Iw44CoefficientTraceTarget,
    quant: u32,
) -> Iw44CoefficientEventTemplate {
    Iw44CoefficientEventTemplate {
        slice_index,
        plane,
        band,
        target,
        quant,
    }
}

impl Iw44CoefficientEventTemplate {
    const fn event(
        self,
        before: i16,
        after: i16,
        kind: Iw44CoefficientEventKind,
    ) -> Iw44CoefficientEvent {
        Iw44CoefficientEvent {
            slice_index: self.slice_index,
            plane: self.plane,
            band: self.band,
            block: self.target.block,
            coefficient: self.target.coefficient,
            bucket: self.target.coefficient / IW44_COEFFICIENTS_PER_BUCKET,
            bucket_offset: self.target.coefficient % IW44_COEFFICIENTS_PER_BUCKET,
            quant: self.quant,
            before,
            after,
            kind,
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
const fn iw44_coefficient(value: i32) -> i16 {
    // IW44 lifting arithmetic narrows through two's-complement i16 wrapping.
    value as i16
}

const fn iw44_sample(value: i32) -> i16 {
    iw44_coefficient(value)
}

fn normalize_iw44_sample(value: i16) -> i32 {
    ((i32::from(value) + 32) >> 6).clamp(-128, 127)
}

fn grayscale_to_rgb(y: &Iw44ReconstructionPlane) -> Iw44RgbImage {
    let mut pixels = Vec::with_capacity(y.width.saturating_mul(y.height).saturating_mul(3));

    for output_row in 0..y.height {
        let source_row = y.height - 1 - output_row;
        let source_offset = source_row * y.width;
        for sample in &y.samples[source_offset..source_offset + y.width] {
            let value = u8::try_from((127 - normalize_iw44_sample(*sample)).clamp(0, 255))
                .expect("normalized grayscale value should fit u8");
            pixels.extend_from_slice(&[value, value, value]);
        }
    }

    Iw44RgbImage {
        width: y.width,
        height: y.height,
        pixels,
    }
}

fn ycbcr_to_rgb(
    y: &Iw44ReconstructionPlane,
    cb: &Iw44ReconstructionPlane,
    cr: &Iw44ReconstructionPlane,
    chroma_half: bool,
) -> Iw44RgbImage {
    let mut pixels = Vec::with_capacity(y.width.saturating_mul(y.height).saturating_mul(3));

    for output_row in 0..y.height {
        let row = y.height - 1 - output_row;
        for col in 0..y.width {
            let y_value = normalize_iw44_sample(y.samples[row * y.width + col]);
            let chroma_row = if chroma_half { row / 2 } else { row }.min(cb.height - 1);
            let chroma_col = if chroma_half { col / 2 } else { col }.min(cb.width - 1);
            let chroma_index = chroma_row * cb.width + chroma_col;
            let blue_chroma = normalize_iw44_sample(cb.samples[chroma_index]);
            let red_chroma = normalize_iw44_sample(cr.samples[chroma_index]);

            pixels.extend_from_slice(&ycbcr_pixel_to_rgb(y_value, blue_chroma, red_chroma));
        }
    }

    Iw44RgbImage {
        width: y.width,
        height: y.height,
        pixels,
    }
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

const fn iw44_zigzag_row(index: usize) -> usize {
    let b1 = (index >> 1) & 1;
    let b3 = (index >> 3) & 1;
    let b5 = (index >> 5) & 1;
    let b7 = (index >> 7) & 1;
    let b9 = (index >> 9) & 1;

    (b1 * 16) + (b3 * 8) + (b5 * 4) + (b7 * 2) + b9
}

const fn iw44_zigzag_col(index: usize) -> usize {
    let b0 = index & 1;
    let b2 = (index >> 2) & 1;
    let b4 = (index >> 4) & 1;
    let b6 = (index >> 6) & 1;
    let b8 = (index >> 8) & 1;

    (b0 * 16) + (b2 * 8) + (b4 * 4) + (b6 * 2) + b8
}

fn inverse_wavelet_transform_with_order(
    plane: &mut [i16],
    width: usize,
    height: usize,
    stride: usize,
    order: Iw44ReconstructionOrder,
) {
    let mut scale = 16;
    while scale > 0 {
        match order {
            Iw44ReconstructionOrder::ColumnsThenRows => {
                inverse_wavelet_columns(plane, width, height, stride, scale);
                inverse_wavelet_rows(plane, width, height, stride, scale);
            }
            Iw44ReconstructionOrder::RowsThenColumns => {
                inverse_wavelet_rows(plane, width, height, stride, scale);
                inverse_wavelet_columns(plane, width, height, stride, scale);
            }
        }
        scale >>= 1;
    }
}

fn inverse_wavelet_columns(
    plane: &mut [i16],
    width: usize,
    height: usize,
    stride: usize,
    step: usize,
) {
    for col in (0..width).step_by(step) {
        let mut line = Vec::with_capacity(height.div_ceil(step));
        for row in (0..height).step_by(step) {
            line.push(plane[row * stride + col]);
        }
        inverse_wavelet_line(&mut line);
        for (index, row) in (0..height).step_by(step).enumerate() {
            plane[row * stride + col] = line[index];
        }
    }
}

fn inverse_wavelet_rows(
    plane: &mut [i16],
    width: usize,
    height: usize,
    stride: usize,
    step: usize,
) {
    for row in (0..height).step_by(step) {
        let offset = row * stride;
        let mut line = Vec::with_capacity(width.div_ceil(step));
        for col in (0..width).step_by(step) {
            line.push(plane[offset + col]);
        }
        inverse_wavelet_line(&mut line);
        for (index, col) in (0..width).step_by(step).enumerate() {
            plane[offset + col] = line[index];
        }
    }
}

fn inverse_wavelet_line(line: &mut [i16]) {
    if line.is_empty() {
        return;
    }

    let last = line.len() - 1;
    for index in (0..=last).step_by(2) {
        let previous_one = if index >= 1 {
            i32::from(line[index - 1])
        } else {
            0
        };
        let next_one = if index < last {
            i32::from(line[index + 1])
        } else {
            0
        };
        let previous_three = if index >= 3 {
            i32::from(line[index - 3])
        } else {
            0
        };
        let next_three = if index + 3 <= last {
            i32::from(line[index + 3])
        } else {
            0
        };
        let adjacent = previous_one + next_one;
        let distant = previous_three + next_three;
        let value = i32::from(line[index]) - (((adjacent << 3) + adjacent - distant + 16) >> 5);
        line[index] = iw44_sample(value);
    }

    if last < 1 {
        return;
    }

    let border = last.saturating_sub(3);
    for index in (1..=last).step_by(2) {
        let value = if index >= 3 && index <= border {
            let previous_three = i32::from(line[index - 3]);
            let previous_one = i32::from(line[index - 1]);
            let next_one = i32::from(line[index + 1]);
            let next_three = i32::from(line[index + 3]);
            let adjacent = previous_one + next_one;
            i32::from(line[index])
                + (((adjacent << 3) + adjacent - (previous_three + next_three) + 8) >> 4)
        } else if index < last {
            let previous_one = i32::from(line[index - 1]);
            let next_one = i32::from(line[index + 1]);
            i32::from(line[index]) + ((previous_one + next_one + 1) >> 1)
        } else {
            i32::from(line[index]) + i32::from(line[index - 1])
        };
        line[index] = iw44_sample(value);
    }
}

fn crop_plane(padded: &[i16], width: usize, height: usize, stride: usize) -> Vec<i16> {
    let mut cropped = Vec::with_capacity(width.saturating_mul(height));
    for row in 0..height {
        let offset = row * stride;
        cropped.extend_from_slice(&padded[offset..offset + width]);
    }
    cropped
}

impl Iw44CoefficientBuffer {
    const fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            block_columns: 0,
            block_rows: 0,
            blocks: Vec::new(),
            buckets: Vec::new(),
        }
    }

    fn new(width: usize, height: usize) -> Self {
        let block_columns = width.div_ceil(32);
        let block_rows = height.div_ceil(32);
        let block_count = block_columns.saturating_mul(block_rows);

        Self {
            width,
            height,
            block_columns,
            block_rows,
            blocks: vec![[0; IW44_COEFFICIENTS_PER_BLOCK]; block_count],
            buckets: vec![Iw44CoefficientBucket::default(); block_count * IW44_BUCKETS_PER_BLOCK],
        }
    }

    fn coefficient(&self, block: usize, coefficient: usize) -> i16 {
        self.blocks[block][coefficient]
    }

    fn set_coefficient(&mut self, block: usize, coefficient: usize, value: i16) {
        self.blocks[block][coefficient] = value;
    }

    fn summary(&self) -> (usize, u16, u64) {
        let mut non_zero_coefficients = 0;
        let mut max_abs_coefficient = 0;
        let mut coefficient_abs_sum = 0u64;

        for coefficient in self.blocks.iter().flat_map(|block| block.iter().copied()) {
            let absolute = coefficient.unsigned_abs();
            if absolute != 0 {
                non_zero_coefficients += 1;
                max_abs_coefficient = max_abs_coefficient.max(absolute);
                coefficient_abs_sum += u64::from(absolute);
            }
        }

        (
            non_zero_coefficients,
            max_abs_coefficient,
            coefficient_abs_sum,
        )
    }

    fn to_row_major_plane(&self) -> Vec<i16> {
        let mut plane = vec![0; self.width.saturating_mul(self.height)];

        for block_row in 0..self.block_rows {
            for block_col in 0..self.block_columns {
                let block_index = block_row * self.block_columns + block_col;
                let block = &self.blocks[block_index];
                let row_base = block_row * 32;
                let col_base = block_col * 32;

                for (coefficient_index, coefficient) in block.iter().copied().enumerate() {
                    let row = row_base + iw44_zigzag_row(coefficient_index);
                    let col = col_base + iw44_zigzag_col(coefficient_index);
                    if row < self.height && col < self.width {
                        plane[row * self.width + col] = coefficient;
                    }
                }
            }
        }

        plane
    }

    fn reconstruct_with_options(
        &self,
        order: Iw44ReconstructionOrder,
        extent: Iw44ReconstructionExtent,
    ) -> Vec<i16> {
        let padded_width = self.block_columns * 32;
        let padded_height = self.block_rows * 32;
        let mut padded = self.to_padded_row_major_plane();
        let (transform_width, transform_height) = match extent {
            Iw44ReconstructionExtent::Visible => (self.width, self.height),
            Iw44ReconstructionExtent::Padded => (padded_width, padded_height),
        };

        inverse_wavelet_transform_with_order(
            &mut padded,
            transform_width,
            transform_height,
            padded_width,
            order,
        );
        crop_plane(&padded, self.width, self.height, padded_width)
    }

    fn to_padded_row_major_plane(&self) -> Vec<i16> {
        let padded_width = self.block_columns * 32;
        let padded_height = self.block_rows * 32;
        let mut plane = vec![0; padded_width.saturating_mul(padded_height)];

        for block_row in 0..self.block_rows {
            for block_col in 0..self.block_columns {
                let block_index = block_row * self.block_columns + block_col;
                let block = &self.blocks[block_index];
                let row_base = block_row * 32;
                let col_base = block_col * 32;

                for (coefficient_index, coefficient) in block.iter().copied().enumerate() {
                    let row = row_base + iw44_zigzag_row(coefficient_index);
                    let col = col_base + iw44_zigzag_col(coefficient_index);
                    plane[row * padded_width + col] = coefficient;
                }
            }
        }

        plane
    }

    fn mark_bucket_range(&mut self, first_bucket: u8, last_bucket: u8, is_null: bool) -> usize {
        if is_null {
            return 0;
        }

        let first = usize::from(first_bucket);
        let last = usize::from(last_bucket);
        let bucket_count = last - first + 1;

        for block in self.buckets.chunks_mut(IW44_BUCKETS_PER_BLOCK) {
            for bucket in &mut block[first..=last] {
                bucket.passes += 1;
            }
        }

        self.block_count() * bucket_count
    }

    const fn block_count(&self) -> usize {
        self.block_columns * self.block_rows
    }

    #[cfg(test)]
    fn bucket_passes(&self, block: usize, bucket: usize) -> u32 {
        self.buckets[(block * IW44_BUCKETS_PER_BLOCK) + bucket].passes
    }
}

impl Default for Iw44Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Iw44Bitstream<'a> {
    #[must_use]
    pub fn new(payload: &'a [u8]) -> Self {
        Self {
            zp: SpecZpDecoder::new(payload),
        }
    }

    pub fn decode_passthrough_bit(&mut self) -> bool {
        self.zp.decode_iw44_passthrough_bit()
    }

    pub fn decode_context_bit(&mut self, context: &mut u8) -> bool {
        self.zp.decode_context_bit(context)
    }
}

/// Reads the IW44 chunk-local header from a raw `FG44`, `BG44`, or `TH44`
/// payload.
///
/// # Errors
///
/// Returns an error if the payload is too short, declares a truncated first
/// chunk header, or declares zero image dimensions.
pub fn read_iw44_chunk_header(bytes: &[u8]) -> Iw44Result<Iw44ChunkHeader> {
    if bytes.len() < 2 {
        return Err(Iw44Error::new("IW44 chunk is too short"));
    }

    let serial = bytes[0];
    let slices = bytes[1];
    let (image, payload_start) = if serial == 0 {
        if bytes.len() < 9 {
            return Err(Iw44Error::new("IW44 first chunk header is too short"));
        }
        let major_version = bytes[2];
        let minor_version = bytes[3];
        let width = u16::from_be_bytes([bytes[4], bytes[5]]);
        let height = u16::from_be_bytes([bytes[6], bytes[7]]);
        if width == 0 || height == 0 {
            return Err(Iw44Error::new("IW44 image has zero dimension"));
        }
        let delay_byte = bytes[8];
        let grayscale = (major_version & 0x80) != 0;
        let delay = if minor_version >= 2 {
            delay_byte & 0x7f
        } else {
            0
        };
        let chroma_half = !grayscale && minor_version >= 2 && (delay_byte & 0x80) == 0;

        (
            Some(Iw44ImageHeader {
                major_version,
                minor_version,
                width,
                height,
                grayscale,
                delay,
                chroma_half,
            }),
            9,
        )
    } else {
        (None, 2)
    };

    Ok(Iw44ChunkHeader {
        serial,
        slices,
        image,
        payload_start,
        payload_len: bytes.len() - payload_start,
    })
}

/// Reads and validates a progressive IW44 chunk sequence.
///
/// # Errors
///
/// Returns an error if the sequence is empty, does not start with serial `0`,
/// has missing image metadata on the first chunk, or has non-contiguous serials.
pub fn summarize_iw44_layer<'a, I>(chunks: I) -> Iw44Result<Iw44LayerSummary>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let mut headers = Vec::new();
    let mut total_slices = 0u32;
    let mut total_payload_bytes = 0usize;

    for (index, chunk) in chunks.into_iter().enumerate() {
        let header = read_iw44_chunk_header(chunk)?;
        let expected_serial = u8::try_from(index).map_err(|_| {
            Iw44Error::new(format!(
                "IW44 layer has more than {} chunks",
                usize::from(u8::MAX) + 1
            ))
        })?;
        if header.serial != expected_serial {
            return Err(Iw44Error::new(format!(
                "IW44 chunk serial {} does not match expected {expected_serial}",
                header.serial
            )));
        }
        total_slices += u32::from(header.slices);
        total_payload_bytes += header.payload_len;
        headers.push(header);
    }

    let first = headers
        .first()
        .ok_or_else(|| Iw44Error::new("IW44 layer has no chunks"))?;
    let image = first
        .image
        .ok_or_else(|| Iw44Error::new("IW44 layer first chunk has no image header"))?;

    Ok(Iw44LayerSummary {
        image,
        chunks: headers,
        total_slices,
        total_payload_bytes,
    })
}

#[cfg(test)]
mod tests {
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
        let header =
            read_iw44_chunk_header(&[0x00, 0x01, 0x80, 0x02, 0x00, 0x20, 0x00, 0x10, 0x00])
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

        let summary = summarize_iw44_layer([first.as_slice(), second.as_slice()])
            .expect("layer should parse");

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
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
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
        const RYPKA: &[u8] = include_bytes!("../Rypka-HIL.djvu");
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
        let explicit =
            decoder.reconstruct_planes_with_order(Iw44ReconstructionOrder::ColumnsThenRows);
        let alternate =
            decoder.reconstruct_planes_with_order(Iw44ReconstructionOrder::RowsThenColumns);
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
}
