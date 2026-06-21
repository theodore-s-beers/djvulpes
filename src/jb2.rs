use crate::bzz::SpecZpDecoder;
use crate::render::BitonalBitmap;
use std::fmt;

pub type Jb2Result<T> = Result<T, Jb2Error>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Jb2Error(String);

impl Jb2Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Jb2Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Jb2Error {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Jb2ImageHeader {
    pub width: u32,
    pub height: u32,
    pub inherited_dictionary_symbols: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Jb2RecordKind {
    StartOfImage,
    NewSymbolAddAndBlit,
    NewSymbolAddOnly,
    NewSymbolBlitOnly,
    MatchedRefineAddAndBlit,
    MatchedRefineAddOnly,
    MatchedRefineBlitOnly,
    MatchedCopyBlitOnly,
    NonSymbol,
    RequiredDictOrReset,
    Comment,
    EndOfData,
}

impl Jb2RecordKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewSymbolAddAndBlit => "new-symbol-add-and-blit",
            Self::StartOfImage => "start-of-image",
            Self::NewSymbolAddOnly => "new-symbol-add-only",
            Self::NewSymbolBlitOnly => "new-symbol-blit-only",
            Self::MatchedRefineAddAndBlit => "matched-refine-add-and-blit",
            Self::MatchedRefineAddOnly => "matched-refine-add-only",
            Self::MatchedRefineBlitOnly => "matched-refine-blit-only",
            Self::MatchedCopyBlitOnly => "matched-copy-blit-only",
            Self::NonSymbol => "non-symbol",
            Self::RequiredDictOrReset => "required-dict-or-reset",
            Self::Comment => "comment",
            Self::EndOfData => "end-of-data",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Jb2RecordSummary {
    pub index: usize,
    pub kind: Jb2RecordKind,
    pub symbol_width: Option<u32>,
    pub symbol_height: Option<u32>,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Jb2RecordPrefix {
    pub header: Jb2ImageHeader,
    pub records: Vec<Jb2RecordSummary>,
    pub stopped_before: Option<Jb2RecordKind>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Jb2PartialImage {
    pub header: Jb2ImageHeader,
    pub mask: BitonalBitmap,
    pub records: Vec<Jb2RecordSummary>,
    pub dictionary_symbol_count: usize,
    pub stopped_before: Option<Jb2RecordKind>,
    pub reached_end_of_data: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Jb2Dictionary {
    symbols: Vec<Jb2SymbolBitmap>,
}

impl Jb2Dictionary {
    #[must_use]
    pub const fn symbol_count(&self) -> usize {
        self.symbols.len()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Jb2SymbolBitmap {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Jb2SymbolBitmap {
    fn dictionary_symbol(&self) -> Self {
        let cropped = self.crop_to_content();
        if cropped.width == 0 || cropped.height == 0 {
            self.clone()
        } else {
            cropped
        }
    }

    fn crop_to_content(&self) -> Self {
        let mut min_x = self.width;
        let mut min_y = self.height;
        let mut max_x = None;
        let mut max_y = None;

        for y in 0..self.height {
            for x in 0..self.width {
                if self.pixel(x, y) == 0 {
                    continue;
                }
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = Some(max_x.map_or(x, |max_x: u32| max_x.max(x)));
                max_y = Some(max_y.map_or(y, |max_y: u32| max_y.max(y)));
            }
        }

        let Some(max_x) = max_x else {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        };
        let max_y = max_y.expect("max_y should be set when max_x is set");
        let width = max_x - min_x + 1;
        let height = max_y - min_y + 1;
        let mut pixels = vec![0; (width as usize).saturating_mul(height as usize)];

        for y in 0..height {
            for x in 0..width {
                let value = self.pixel(min_x + x, min_y + y);
                let target_index = (y as usize)
                    .checked_mul(width as usize)
                    .and_then(|offset| offset.checked_add(x as usize))
                    .expect("cropped JB2 symbol offset should not overflow");
                pixels[target_index] = value;
            }
        }

        Self {
            width,
            height,
            pixels,
        }
    }

    fn pixel(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let width = usize::try_from(self.width).expect("symbol width should fit usize");
        let x = usize::try_from(x).expect("symbol x should fit usize");
        let y = usize::try_from(y).expect("symbol y should fit usize");
        let Some(index) = y
            .checked_mul(width)
            .and_then(|offset| offset.checked_add(x))
        else {
            return 0;
        };

        self.pixels.get(index).copied().unwrap_or(0)
    }
}

/// Reads the JB2 image preamble and dimensions from an `Sjbz` payload.
///
/// # Errors
///
/// Returns an error if the payload is too short, does not start with a JB2
/// image record, has an invalid reserved flag, or declares invalid dimensions.
pub fn read_jb2_image_header(bytes: &[u8]) -> Jb2Result<Jb2ImageHeader> {
    let mut decoder = Jb2Decoder::new(bytes)?;

    decoder.read_header()
}

/// Reads a prefix of JB2 records after the image header.
///
/// Direct bitmap records are consumed so subsequent record types are decoded
/// from the correct arithmetic-coder position. Matched/refinement records are
/// reported as `stopped_before` because full dictionary bitmap support is not
/// implemented yet.
///
/// # Errors
///
/// Returns an error if the JB2 header or a supported prefix record is malformed.
#[allow(clippy::too_many_lines)]
pub fn read_jb2_record_prefix(bytes: &[u8], max_records: usize) -> Jb2Result<Jb2RecordPrefix> {
    let mut decoder = Jb2Decoder::new(bytes)?;
    let header = decoder.read_header()?;
    let mut records = Vec::new();

    for index in 0..max_records {
        let kind = decoder.next_record_kind()?;
        let record = match kind {
            Jb2RecordKind::NewSymbolAddAndBlit | Jb2RecordKind::NewSymbolBlitOnly => {
                let symbol_width = decoder.decode_symbol_width()?;
                let symbol_height = decoder.decode_symbol_height()?;
                let _ = decoder.decode_direct_bitmap(symbol_width, symbol_height)?;
                let (x, y) = decoder.decode_symbol_coords(symbol_width, symbol_height);
                Some(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: Some(symbol_width),
                    symbol_height: Some(symbol_height),
                    x: Some(x),
                    y: Some(y),
                })
            }
            Jb2RecordKind::StartOfImage => Some(Jb2RecordSummary {
                index,
                kind,
                symbol_width: None,
                symbol_height: None,
                x: None,
                y: None,
            }),
            Jb2RecordKind::NewSymbolAddOnly => {
                let symbol_width = decoder.decode_symbol_width()?;
                let symbol_height = decoder.decode_symbol_height()?;
                let _ = decoder.decode_direct_bitmap(symbol_width, symbol_height)?;
                Some(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: Some(symbol_width),
                    symbol_height: Some(symbol_height),
                    x: None,
                    y: None,
                })
            }
            Jb2RecordKind::NonSymbol => {
                let symbol_width = decoder.decode_symbol_width()?;
                let symbol_height = decoder.decode_symbol_height()?;
                let _ = decoder.decode_direct_bitmap(symbol_width, symbol_height)?;
                let image_width = i32::try_from(header.width)
                    .map_err(|_| Jb2Error::new("JB2 image width exceeds scanner range"))?;
                let image_height = i32::try_from(header.height)
                    .map_err(|_| Jb2Error::new("JB2 image height exceeds scanner range"))?;
                let x = decode_num(
                    &mut decoder.zp,
                    &mut decoder.horizontal_absolute_location_context,
                    1,
                    image_width,
                ) - 1;
                let top = decode_num(
                    &mut decoder.zp,
                    &mut decoder.vertical_absolute_location_context,
                    1,
                    image_height,
                );
                let symbol_height_i32 = i32::try_from(symbol_height)
                    .map_err(|_| Jb2Error::new("JB2 symbol height exceeds scanner range"))?;
                let y = top - symbol_height_i32;
                Some(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: Some(symbol_width),
                    symbol_height: Some(symbol_height),
                    x: Some(x),
                    y: Some(y),
                })
            }
            Jb2RecordKind::Comment => {
                let length = decode_num(
                    &mut decoder.zp,
                    &mut decoder.comment_length_context,
                    0,
                    262_142,
                );
                for _ in 0..length {
                    decode_num(&mut decoder.zp, &mut decoder.comment_octet_context, 0, 255);
                }
                Some(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: None,
                    symbol_height: None,
                    x: None,
                    y: None,
                })
            }
            Jb2RecordKind::EndOfData => {
                records.push(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: None,
                    symbol_height: None,
                    x: None,
                    y: None,
                });
                return Ok(Jb2RecordPrefix {
                    header,
                    records,
                    stopped_before: None,
                });
            }
            Jb2RecordKind::MatchedRefineAddAndBlit
            | Jb2RecordKind::MatchedRefineAddOnly
            | Jb2RecordKind::MatchedRefineBlitOnly
            | Jb2RecordKind::MatchedCopyBlitOnly
            | Jb2RecordKind::RequiredDictOrReset => {
                return Ok(Jb2RecordPrefix {
                    header,
                    records,
                    stopped_before: Some(kind),
                });
            }
        };

        if let Some(record) = record {
            records.push(record);
        }
    }

    Ok(Jb2RecordPrefix {
        header,
        records,
        stopped_before: None,
    })
}

/// Decodes and paints the supported prefix of a JB2 image.
///
/// This intentionally stops before dictionary-reset records. The returned mask
/// contains every decoded blit in the supported prefix.
///
/// # Errors
///
/// Returns an error if the JB2 header or a supported prefix record is malformed.
#[allow(clippy::too_many_lines)]
pub fn render_jb2_supported_prefix(bytes: &[u8], max_records: usize) -> Jb2Result<Jb2PartialImage> {
    render_jb2_records(bytes, max_records, None)
}

/// Decodes and paints a complete JB2 image.
///
/// # Errors
///
/// Returns an error if the JB2 image is malformed, requires an external
/// dictionary/reset operation, or does not reach `EndOfData` before the
/// decoder's safety record limit.
pub fn render_jb2_image(bytes: &[u8]) -> Jb2Result<Jb2PartialImage> {
    const MAX_IMAGE_RECORDS: usize = 1_000_000;

    let image = render_jb2_records(bytes, MAX_IMAGE_RECORDS, None)?;
    if let Some(kind) = image.stopped_before {
        return Err(Jb2Error::new(format!(
            "JB2 image stopped before unsupported {} record",
            kind.as_str()
        )));
    }
    if !image.reached_end_of_data {
        return Err(Jb2Error::new(format!(
            "JB2 image did not reach end-of-data within {MAX_IMAGE_RECORDS} records"
        )));
    }

    Ok(image)
}

/// Decodes and paints a complete JB2 image using a shared symbol dictionary.
///
/// # Errors
///
/// Returns an error if the JB2 image is malformed, requires more inherited
/// symbols than the dictionary provides, or does not reach `EndOfData`.
pub fn render_jb2_image_with_dictionary(
    bytes: &[u8],
    dictionary: &Jb2Dictionary,
) -> Jb2Result<Jb2PartialImage> {
    const MAX_IMAGE_RECORDS: usize = 1_000_000;

    let image = render_jb2_records(bytes, MAX_IMAGE_RECORDS, Some(dictionary))?;
    if let Some(kind) = image.stopped_before {
        return Err(Jb2Error::new(format!(
            "JB2 image stopped before unsupported {} record",
            kind.as_str()
        )));
    }
    if !image.reached_end_of_data {
        return Err(Jb2Error::new(format!(
            "JB2 image did not reach end-of-data within {MAX_IMAGE_RECORDS} records"
        )));
    }

    Ok(image)
}

/// Decodes a shared JB2 dictionary from a `Djbz` payload.
///
/// # Errors
///
/// Returns an error if the dictionary stream is malformed or contains a record
/// kind that is invalid for dictionary chunks.
pub fn decode_jb2_dictionary(bytes: &[u8]) -> Jb2Result<Jb2Dictionary> {
    decode_jb2_dictionary_with_inherited(bytes, None)
}

fn decode_jb2_dictionary_with_inherited(
    bytes: &[u8],
    inherited: Option<&Jb2Dictionary>,
) -> Jb2Result<Jb2Dictionary> {
    const MAX_DICTIONARY_RECORDS: usize = 1_000_000;

    let mut decoder = Jb2Decoder::new(bytes)?;
    let header = decoder.read_header()?;
    let mut dictionary = initial_dictionary(inherited, header.inherited_dictionary_symbols)?;

    for _ in 0..MAX_DICTIONARY_RECORDS {
        match decoder.next_record_kind()? {
            Jb2RecordKind::NewSymbolAddOnly => {
                let symbol = decoder.decode_direct_symbol()?;
                dictionary.push(symbol.dictionary_symbol());
            }
            Jb2RecordKind::MatchedRefineAddOnly => {
                let symbol = decoder.decode_refined_symbol(&dictionary)?;
                dictionary.push(symbol.dictionary_symbol());
            }
            Jb2RecordKind::RequiredDictOrReset => {}
            Jb2RecordKind::Comment => decoder.skip_comment(),
            Jb2RecordKind::EndOfData => {
                return Ok(Jb2Dictionary {
                    symbols: dictionary,
                });
            }
            kind => {
                return Err(Jb2Error::new(format!(
                    "JB2 dictionary contains unexpected {} record",
                    kind.as_str()
                )));
            }
        }
    }

    Err(Jb2Error::new(
        "JB2 dictionary did not reach end-of-data before record limit",
    ))
}

#[allow(clippy::too_many_lines)]
fn render_jb2_records(
    bytes: &[u8],
    max_records: usize,
    shared_dictionary: Option<&Jb2Dictionary>,
) -> Jb2Result<Jb2PartialImage> {
    let mut decoder = Jb2Decoder::new(bytes)?;
    let header = decoder.read_header()?;
    let mut mask = BitonalBitmap::new(header.width, header.height);
    let mut dictionary =
        initial_dictionary(shared_dictionary, header.inherited_dictionary_symbols)?;
    let mut records = Vec::new();

    for index in 0..max_records {
        let kind = decoder.next_record_kind()?;
        let record = match kind {
            Jb2RecordKind::NewSymbolAddAndBlit => {
                let symbol = decoder.decode_direct_symbol()?;
                let (x, y) = decoder.decode_symbol_coords(symbol.width, symbol.height);
                blit_symbol(&mut mask, &symbol, x, y);
                let record = symbol_record(index, kind, &symbol, Some((x, y)));
                dictionary.push(symbol.dictionary_symbol());
                Some(record)
            }
            Jb2RecordKind::NewSymbolAddOnly => {
                let symbol = decoder.decode_direct_symbol()?;
                let record = symbol_record(index, kind, &symbol, None);
                dictionary.push(symbol.dictionary_symbol());
                Some(record)
            }
            Jb2RecordKind::NewSymbolBlitOnly => {
                let symbol = decoder.decode_direct_symbol()?;
                let (x, y) = decoder.decode_symbol_coords(symbol.width, symbol.height);
                blit_symbol(&mut mask, &symbol, x, y);
                Some(symbol_record(index, kind, &symbol, Some((x, y))))
            }
            Jb2RecordKind::NonSymbol => {
                let symbol = decoder.decode_direct_symbol()?;
                let image_width = i32::try_from(header.width)
                    .map_err(|_| Jb2Error::new("JB2 image width exceeds scanner range"))?;
                let image_height = i32::try_from(header.height)
                    .map_err(|_| Jb2Error::new("JB2 image height exceeds scanner range"))?;
                let x = decode_num(
                    &mut decoder.zp,
                    &mut decoder.horizontal_absolute_location_context,
                    1,
                    image_width,
                ) - 1;
                let top = decode_num(
                    &mut decoder.zp,
                    &mut decoder.vertical_absolute_location_context,
                    1,
                    image_height,
                );
                let symbol_height = i32::try_from(symbol.height)
                    .map_err(|_| Jb2Error::new("JB2 symbol height exceeds scanner range"))?;
                let y = top - symbol_height;
                blit_symbol(&mut mask, &symbol, x, y);
                Some(symbol_record(index, kind, &symbol, Some((x, y))))
            }
            Jb2RecordKind::MatchedRefineAddAndBlit => {
                let symbol = decoder.decode_refined_symbol(&dictionary)?;
                let (x, y) = decoder.decode_symbol_coords(symbol.width, symbol.height);
                blit_symbol(&mut mask, &symbol, x, y);
                let record = symbol_record(index, kind, &symbol, Some((x, y)));
                dictionary.push(symbol.dictionary_symbol());
                Some(record)
            }
            Jb2RecordKind::MatchedRefineAddOnly => {
                let symbol = decoder.decode_refined_symbol(&dictionary)?;
                let record = symbol_record(index, kind, &symbol, None);
                dictionary.push(symbol.dictionary_symbol());
                Some(record)
            }
            Jb2RecordKind::MatchedRefineBlitOnly => {
                let symbol = decoder.decode_refined_symbol(&dictionary)?;
                let (x, y) = decoder.decode_symbol_coords(symbol.width, symbol.height);
                blit_symbol(&mut mask, &symbol, x, y);
                Some(symbol_record(index, kind, &symbol, Some((x, y))))
            }
            Jb2RecordKind::MatchedCopyBlitOnly => {
                if dictionary.is_empty() {
                    return Err(Jb2Error::new(
                        "JB2 copy record references an empty dictionary",
                    ));
                }
                let symbol_index = decode_num(
                    &mut decoder.zp,
                    &mut decoder.symbol_index_context,
                    0,
                    i32::try_from(dictionary.len() - 1)
                        .map_err(|_| Jb2Error::new("JB2 dictionary is too large"))?,
                );
                let symbol_index = usize::try_from(symbol_index)
                    .map_err(|_| Jb2Error::new("JB2 copy symbol index is negative"))?;
                let symbol = dictionary
                    .get(symbol_index)
                    .ok_or_else(|| Jb2Error::new("JB2 copy symbol index is out of range"))?;
                let (x, y) = decoder.decode_symbol_coords(symbol.width, symbol.height);
                blit_symbol(&mut mask, symbol, x, y);
                Some(symbol_record(index, kind, symbol, Some((x, y))))
            }
            Jb2RecordKind::StartOfImage => Some(Jb2RecordSummary {
                index,
                kind,
                symbol_width: None,
                symbol_height: None,
                x: None,
                y: None,
            }),
            Jb2RecordKind::Comment => {
                decoder.skip_comment();
                Some(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: None,
                    symbol_height: None,
                    x: None,
                    y: None,
                })
            }
            Jb2RecordKind::EndOfData => {
                records.push(Jb2RecordSummary {
                    index,
                    kind,
                    symbol_width: None,
                    symbol_height: None,
                    x: None,
                    y: None,
                });
                return Ok(Jb2PartialImage {
                    header,
                    mask,
                    records,
                    dictionary_symbol_count: dictionary.len(),
                    stopped_before: None,
                    reached_end_of_data: true,
                });
            }
            Jb2RecordKind::RequiredDictOrReset => None,
        };

        if let Some(record) = record {
            records.push(record);
        }
    }

    Ok(Jb2PartialImage {
        header,
        mask,
        records,
        dictionary_symbol_count: dictionary.len(),
        stopped_before: None,
        reached_end_of_data: false,
    })
}

fn initial_dictionary(
    shared_dictionary: Option<&Jb2Dictionary>,
    requested_symbols: u32,
) -> Jb2Result<Vec<Jb2SymbolBitmap>> {
    if requested_symbols == 0 {
        return Ok(Vec::new());
    }
    let shared_dictionary =
        shared_dictionary.ok_or_else(|| Jb2Error::new("JB2 image requires a shared dictionary"))?;
    let requested_symbols = usize::try_from(requested_symbols)
        .map_err(|_| Jb2Error::new("JB2 inherited dictionary size exceeds decoder range"))?;
    if requested_symbols > shared_dictionary.symbols.len() {
        return Err(Jb2Error::new(format!(
            "JB2 image requires {requested_symbols} shared symbols, dictionary has {}",
            shared_dictionary.symbols.len()
        )));
    }

    Ok(shared_dictionary.symbols[..requested_symbols].to_vec())
}

struct Jb2Decoder<'a> {
    zp: SpecZpDecoder<'a>,
    record_type_context: NumContext,
    image_size_context: NumContext,
    symbol_width_context: NumContext,
    symbol_height_context: NumContext,
    inherited_size_context: NumContext,
    coord_contexts: CoordContexts,
    symbol_index_context: NumContext,
    symbol_width_difference_context: NumContext,
    symbol_height_difference_context: NumContext,
    horizontal_absolute_location_context: NumContext,
    vertical_absolute_location_context: NumContext,
    comment_length_context: NumContext,
    comment_octet_context: NumContext,
    direct_bitmap_contexts: [u8; 1024],
    refinement_bitmap_contexts: [u8; 2048],
    layout: Option<LayoutState>,
}

impl<'a> Jb2Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Jb2Result<Self> {
        if bytes.len() < 2 {
            return Err(Jb2Error::new("JB2 input is too short"));
        }

        Ok(Self {
            zp: SpecZpDecoder::new(bytes),
            record_type_context: NumContext::new(),
            image_size_context: NumContext::new(),
            symbol_width_context: NumContext::new(),
            symbol_height_context: NumContext::new(),
            inherited_size_context: NumContext::new(),
            coord_contexts: CoordContexts::new(),
            symbol_index_context: NumContext::new(),
            symbol_width_difference_context: NumContext::new(),
            symbol_height_difference_context: NumContext::new(),
            horizontal_absolute_location_context: NumContext::new(),
            vertical_absolute_location_context: NumContext::new(),
            comment_length_context: NumContext::new(),
            comment_octet_context: NumContext::new(),
            direct_bitmap_contexts: [0; 1024],
            refinement_bitmap_contexts: [0; 2048],
            layout: None,
        })
    }

    fn read_header(&mut self) -> Jb2Result<Jb2ImageHeader> {
        let mut record_type = decode_num(&mut self.zp, &mut self.record_type_context, 0, 11);
        let inherited_dictionary_symbols = if record_type == 9 {
            let count = decode_num(&mut self.zp, &mut self.inherited_size_context, 0, 262_142);
            record_type = decode_num(&mut self.zp, &mut self.record_type_context, 0, 11);
            u32::try_from(count)
                .map_err(|_| Jb2Error::new("JB2 inherited dictionary is negative"))?
        } else {
            0
        };

        if record_type != 0 {
            return Err(Jb2Error::new(format!(
                "JB2 image starts with record type {record_type}, expected 0"
            )));
        }

        let width = decode_dimension(&mut self.zp, &mut self.image_size_context, "width")?;
        let height = decode_dimension(&mut self.zp, &mut self.image_size_context, "height")?;
        let mut flag_context = 0;
        if self.zp.decode_context_bit(&mut flag_context) {
            return Err(Jb2Error::new("JB2 image has invalid reserved header flag"));
        }

        self.layout = Some(LayoutState::new(
            i32::try_from(height).expect("JB2 height should fit i32"),
        ));

        Ok(Jb2ImageHeader {
            width,
            height,
            inherited_dictionary_symbols,
        })
    }

    fn next_record_kind(&mut self) -> Jb2Result<Jb2RecordKind> {
        let record_type = decode_num(&mut self.zp, &mut self.record_type_context, 0, 11);

        match record_type {
            0 => Ok(Jb2RecordKind::StartOfImage),
            1 => Ok(Jb2RecordKind::NewSymbolAddAndBlit),
            2 => Ok(Jb2RecordKind::NewSymbolAddOnly),
            3 => Ok(Jb2RecordKind::NewSymbolBlitOnly),
            4 => Ok(Jb2RecordKind::MatchedRefineAddAndBlit),
            5 => Ok(Jb2RecordKind::MatchedRefineAddOnly),
            6 => Ok(Jb2RecordKind::MatchedRefineBlitOnly),
            7 => Ok(Jb2RecordKind::MatchedCopyBlitOnly),
            8 => Ok(Jb2RecordKind::NonSymbol),
            9 => Ok(Jb2RecordKind::RequiredDictOrReset),
            10 => Ok(Jb2RecordKind::Comment),
            11 => Ok(Jb2RecordKind::EndOfData),
            _ => Err(Jb2Error::new(format!(
                "JB2 stream contains unknown record type {record_type}"
            ))),
        }
    }

    fn decode_symbol_width(&mut self) -> Jb2Result<u32> {
        decode_dimension(&mut self.zp, &mut self.symbol_width_context, "symbol width")
    }

    fn decode_symbol_height(&mut self) -> Jb2Result<u32> {
        decode_dimension(
            &mut self.zp,
            &mut self.symbol_height_context,
            "symbol height",
        )
    }

    fn decode_symbol_coords(&mut self, symbol_width: u32, symbol_height: u32) -> (i32, i32) {
        decode_symbol_coords(
            &mut self.zp,
            &mut self.coord_contexts,
            self.layout
                .as_mut()
                .expect("JB2 layout should be initialized by image header"),
            i32::try_from(symbol_width).expect("symbol width should fit i32"),
            i32::try_from(symbol_height).expect("symbol height should fit i32"),
        )
    }

    fn skip_comment(&mut self) {
        let length = decode_num(&mut self.zp, &mut self.comment_length_context, 0, 262_142);
        for _ in 0..length {
            decode_num(&mut self.zp, &mut self.comment_octet_context, 0, 255);
        }
    }

    fn decode_direct_symbol(&mut self) -> Jb2Result<Jb2SymbolBitmap> {
        let width = self.decode_symbol_width()?;
        let height = self.decode_symbol_height()?;
        self.decode_direct_bitmap(width, height)
    }

    fn decode_direct_bitmap(&mut self, width: u32, height: u32) -> Jb2Result<Jb2SymbolBitmap> {
        if width == 0 || height == 0 {
            return Err(Jb2Error::new("JB2 direct bitmap has zero dimensions"));
        }

        let width_usize = usize::try_from(width).expect("bitmap width should fit usize");
        let height_usize = usize::try_from(height).expect("bitmap height should fit usize");
        let mut previous_two_rows = vec![0u8; width_usize];
        let mut previous_row = vec![0u8; width_usize];
        let mut current_row = vec![0u8; width_usize];
        let mut pixels = vec![0; width_usize.saturating_mul(height_usize)];

        for row in (0..height_usize).rev() {
            current_row.fill(0);
            let mut row_2_context =
                pixel(&previous_two_rows, 0) << 1 | pixel(&previous_two_rows, 1);
            let mut row_1_context = pixel(&previous_row, 0) << 2
                | pixel(&previous_row, 1) << 1
                | pixel(&previous_row, 2);
            let mut current_context = 0;

            for (x, current_pixel) in current_row.iter_mut().enumerate().take(width_usize) {
                let context = usize::try_from(
                    ((row_2_context << 7) | (row_1_context << 2) | current_context) & 1023,
                )
                .expect("direct bitmap context should fit usize");
                let bit = self
                    .zp
                    .decode_context_bit(&mut self.direct_bitmap_contexts[context]);
                let bit = u8::from(bit);
                *current_pixel = bit;
                row_2_context = ((row_2_context << 1) & 0b111) | pixel(&previous_two_rows, x + 2);
                row_1_context = ((row_1_context << 1) & 0b1_1111) | pixel(&previous_row, x + 3);
                current_context = ((current_context << 1) & 0b11) | u32::from(bit);
            }

            let row_start = row
                .checked_mul(width_usize)
                .expect("JB2 symbol row offset should not overflow");
            pixels[row_start..row_start + width_usize].copy_from_slice(&current_row);
            std::mem::swap(&mut previous_two_rows, &mut previous_row);
            std::mem::swap(&mut previous_row, &mut current_row);
        }

        Ok(Jb2SymbolBitmap {
            width,
            height,
            pixels,
        })
    }

    fn decode_refined_symbol(
        &mut self,
        dictionary: &[Jb2SymbolBitmap],
    ) -> Jb2Result<Jb2SymbolBitmap> {
        if dictionary.is_empty() {
            return Err(Jb2Error::new(
                "JB2 matched record references an empty dictionary",
            ));
        }
        let symbol_index = decode_num(
            &mut self.zp,
            &mut self.symbol_index_context,
            0,
            i32::try_from(dictionary.len() - 1)
                .map_err(|_| Jb2Error::new("JB2 dictionary is too large"))?,
        );
        let symbol_index = usize::try_from(symbol_index)
            .map_err(|_| Jb2Error::new("JB2 symbol index is negative"))?;
        let reference = dictionary
            .get(symbol_index)
            .ok_or_else(|| Jb2Error::new("JB2 symbol index is out of range"))?;
        let width_difference = decode_num(
            &mut self.zp,
            &mut self.symbol_width_difference_context,
            -262_143,
            262_142,
        );
        let height_difference = decode_num(
            &mut self.zp,
            &mut self.symbol_height_difference_context,
            -262_143,
            262_142,
        );
        let width = i32::try_from(reference.width)
            .map_err(|_| Jb2Error::new("JB2 reference width exceeds decoder range"))?
            + width_difference;
        let height = i32::try_from(reference.height)
            .map_err(|_| Jb2Error::new("JB2 reference height exceeds decoder range"))?
            + height_difference;
        if width < 0 || height < 0 {
            return Err(Jb2Error::new("JB2 refined bitmap has negative dimensions"));
        }
        if width == 0 || height == 0 {
            return Ok(Jb2SymbolBitmap {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            });
        }

        Ok(self.decode_refined_bitmap(
            u32::try_from(width).map_err(|_| Jb2Error::new("JB2 refined width is negative"))?,
            u32::try_from(height).map_err(|_| Jb2Error::new("JB2 refined height is negative"))?,
            reference,
        ))
    }

    fn decode_refined_bitmap(
        &mut self,
        width: u32,
        height: u32,
        reference: &Jb2SymbolBitmap,
    ) -> Jb2SymbolBitmap {
        let width_usize = usize::try_from(width).expect("bitmap width should fit usize");
        let height_usize = usize::try_from(height).expect("bitmap height should fit usize");
        let mut pixels = vec![0; width_usize.saturating_mul(height_usize)];
        let mut current_row = vec![0u8; width_usize];
        let mut previous_child_row = vec![0u8; width_usize];

        let child_center_row = (i32::try_from(height).expect("height should fit i32") - 1) >> 1;
        let child_center_col = (i32::try_from(width).expect("width should fit i32") - 1) >> 1;
        let reference_center_row =
            (i32::try_from(reference.height).expect("height should fit i32") - 1) >> 1;
        let reference_center_col =
            (i32::try_from(reference.width).expect("width should fit i32") - 1) >> 1;
        let row_shift = reference_center_row - child_center_row;
        let col_shift = reference_center_col - child_center_col;

        for row in (0..height_usize).rev() {
            current_row.fill(0);
            let row_i32 = i32::try_from(row).expect("row should fit i32");
            let mut child_previous_context = symbol_pixel(&previous_child_row, width_usize, 0, 0)
                << 1
                | symbol_pixel(&previous_child_row, width_usize, 1, 0);
            let mut current_context = 0;
            let reference_row_1 = row_i32 + row_shift;
            let reference_row_0 = reference_row_1 - 1;
            let mut reference_context_1 =
                symbol_pixel_at(reference, col_shift - 1, reference_row_1) << 2
                    | symbol_pixel_at(reference, col_shift, reference_row_1) << 1
                    | symbol_pixel_at(reference, col_shift + 1, reference_row_1);
            let mut reference_context_0 =
                symbol_pixel_at(reference, col_shift - 1, reference_row_0) << 2
                    | symbol_pixel_at(reference, col_shift, reference_row_0) << 1
                    | symbol_pixel_at(reference, col_shift + 1, reference_row_0);

            for (col, current_pixel) in current_row.iter_mut().enumerate().take(width_usize) {
                let col_i32 = i32::try_from(col).expect("column should fit i32");
                let reference_row_2 = row_i32 + row_shift + 1;
                let reference_bit_2 =
                    symbol_pixel_at(reference, col_i32 + col_shift, reference_row_2);
                let index = ((child_previous_context << 8)
                    | (current_context << 7)
                    | (reference_bit_2 << 6)
                    | (reference_context_1 << 3)
                    | reference_context_0)
                    & 2047;
                let bit = self.zp.decode_context_bit(
                    &mut self.refinement_bitmap_contexts
                        [usize::try_from(index).expect("refinement context should fit usize")],
                );
                let bit = u8::from(bit);
                *current_pixel = bit;

                child_previous_context = ((child_previous_context << 1) & 0b111)
                    | symbol_pixel(&previous_child_row, width_usize, col + 2, 0);
                current_context = u32::from(bit);
                reference_context_1 = ((reference_context_1 << 1) & 0b111)
                    | symbol_pixel_at(reference, col_i32 + col_shift + 2, reference_row_1);
                reference_context_0 = ((reference_context_0 << 1) & 0b111)
                    | symbol_pixel_at(reference, col_i32 + col_shift + 2, reference_row_0);
            }

            let row_start = row
                .checked_mul(width_usize)
                .expect("JB2 refined symbol row offset should not overflow");
            pixels[row_start..row_start + width_usize].copy_from_slice(&current_row);
            std::mem::swap(&mut previous_child_row, &mut current_row);
        }

        Jb2SymbolBitmap {
            width,
            height,
            pixels,
        }
    }
}

struct CoordContexts {
    offset_type: u8,
    hoff: NumContext,
    voff: NumContext,
    shoff: NumContext,
    svoff: NumContext,
}

impl CoordContexts {
    fn new() -> Self {
        Self {
            offset_type: 0,
            hoff: NumContext::new(),
            voff: NumContext::new(),
            shoff: NumContext::new(),
            svoff: NumContext::new(),
        }
    }
}

struct LayoutState {
    first_left: i32,
    first_bottom: i32,
    last_right: i32,
    baseline: Baseline,
}

impl LayoutState {
    const fn new(image_height: i32) -> Self {
        Self {
            first_left: -1,
            first_bottom: image_height - 1,
            last_right: 0,
            baseline: Baseline::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct Baseline {
    values: [i32; 3],
    index: i32,
}

impl Baseline {
    const fn new() -> Self {
        Self {
            values: [0; 3],
            index: -1,
        }
    }

    const fn fill(&mut self, value: i32) {
        self.values = [value; 3];
    }

    fn add(&mut self, value: i32) {
        self.index += 1;
        if self.index == 3 {
            self.index = 0;
        }
        self.values[usize::try_from(self.index).expect("baseline index should fit usize")] = value;
    }

    const fn value(&self) -> i32 {
        let [a, b, c] = self.values;
        if (a >= b && a <= c) || (a <= b && a >= c) {
            a
        } else if (b >= a && b <= c) || (b <= a && b >= c) {
            b
        } else {
            c
        }
    }
}

fn decode_symbol_coords(
    decoder: &mut SpecZpDecoder<'_>,
    contexts: &mut CoordContexts,
    layout: &mut LayoutState,
    symbol_width: i32,
    symbol_height: i32,
) -> (i32, i32) {
    let new_line = decoder.decode_context_bit(&mut contexts.offset_type);

    let (x, y) = if new_line {
        let hoff = decode_num(decoder, &mut contexts.hoff, -262_143, 262_142);
        let voff = decode_num(decoder, &mut contexts.voff, -262_143, 262_142);
        let x = layout.first_left + hoff;
        let y = layout.first_bottom + voff - symbol_height + 1;
        layout.first_left = x;
        layout.first_bottom = y;
        layout.baseline.fill(y);
        (x, y)
    } else {
        let hoff = decode_num(decoder, &mut contexts.shoff, -262_143, 262_142);
        let voff = decode_num(decoder, &mut contexts.svoff, -262_143, 262_142);
        (layout.last_right + hoff, layout.baseline.value() + voff)
    };

    layout.baseline.add(y);
    layout.last_right = x + symbol_width - 1;
    (x, y)
}

fn pixel(row: &[u8], x: usize) -> u32 {
    u32::from(row.get(x).copied().unwrap_or(0))
}

fn symbol_pixel(row: &[u8], width: usize, x: usize, y: usize) -> u32 {
    let Some(index) = y
        .checked_mul(width)
        .and_then(|offset| offset.checked_add(x))
    else {
        return 0;
    };
    u32::from(row.get(index).copied().unwrap_or(0))
}

fn symbol_pixel_at(symbol: &Jb2SymbolBitmap, x: i32, y: i32) -> u32 {
    if x < 0 || y < 0 {
        return 0;
    }
    let Ok(x) = u32::try_from(x) else {
        return 0;
    };
    let Ok(y) = u32::try_from(y) else {
        return 0;
    };
    if x >= symbol.width || y >= symbol.height {
        return 0;
    }
    let width = usize::try_from(symbol.width).expect("symbol width should fit usize");
    let x = usize::try_from(x).expect("symbol x should fit usize");
    let y = usize::try_from(y).expect("symbol y should fit usize");
    let Some(index) = y
        .checked_mul(width)
        .and_then(|offset| offset.checked_add(x))
    else {
        return 0;
    };

    u32::from(symbol.pixels.get(index).copied().unwrap_or(0))
}

fn symbol_record(
    index: usize,
    kind: Jb2RecordKind,
    symbol: &Jb2SymbolBitmap,
    position: Option<(i32, i32)>,
) -> Jb2RecordSummary {
    Jb2RecordSummary {
        index,
        kind,
        symbol_width: Some(symbol.width),
        symbol_height: Some(symbol.height),
        x: position.map(|(x, _)| x),
        y: position.map(|(_, y)| y),
    }
}

fn blit_symbol(mask: &mut BitonalBitmap, symbol: &Jb2SymbolBitmap, x: i32, y: i32) {
    let page_height = i32::try_from(mask.height).expect("page height should fit i32");
    let symbol_width = usize::try_from(symbol.width).expect("symbol width should fit usize");

    for source_y in 0..symbol.height {
        let dest_y = page_height
            .saturating_sub(y)
            .saturating_sub(1)
            .saturating_sub(i32::try_from(source_y).expect("source y should fit i32"));
        if dest_y < 0 {
            continue;
        }
        let Ok(dest_y) = u32::try_from(dest_y) else {
            continue;
        };
        for source_x in 0..symbol.width {
            let dest_x = x + i32::try_from(source_x).expect("source x should fit i32");
            if dest_x < 0 {
                continue;
            }
            let Ok(dest_x) = u32::try_from(dest_x) else {
                continue;
            };
            let source_index = usize::try_from(source_y)
                .expect("source y should fit usize")
                .checked_mul(symbol_width)
                .and_then(|offset| {
                    offset
                        .checked_add(usize::try_from(source_x).expect("source x should fit usize"))
                })
                .expect("symbol pixel offset should not overflow");
            if symbol.pixels[source_index] != 0 {
                mask.set_bit(dest_x, dest_y, true);
            }
        }
    }
}

fn decode_dimension(
    decoder: &mut SpecZpDecoder<'_>,
    context: &mut NumContext,
    name: &str,
) -> Jb2Result<u32> {
    let dimension = decode_num(decoder, context, 0, 262_142);
    let dimension = if dimension == 0 { 200 } else { dimension };

    u32::try_from(dimension).map_err(|_| Jb2Error::new(format!("JB2 {name} is negative")))
}

struct NumContext {
    contexts: Vec<u8>,
    left: Vec<usize>,
    right: Vec<usize>,
}

impl NumContext {
    const ROOT: usize = 1;

    fn new() -> Self {
        Self {
            contexts: vec![0, 0],
            left: vec![0, 0],
            right: vec![0, 0],
        }
    }

    fn left_child(&mut self, node: usize) -> usize {
        if self.left[node] == 0 {
            self.push_child(node, false);
        }
        self.left[node]
    }

    fn right_child(&mut self, node: usize) -> usize {
        if self.right[node] == 0 {
            self.push_child(node, true);
        }
        self.right[node]
    }

    fn push_child(&mut self, node: usize, right: bool) {
        let index = self.contexts.len();
        self.contexts.push(0);
        self.left.push(0);
        self.right.push(0);

        if right {
            self.right[node] = index;
        } else {
            self.left[node] = index;
        }
    }
}

fn decode_num(
    decoder: &mut SpecZpDecoder<'_>,
    context: &mut NumContext,
    mut low: i32,
    mut high: i32,
) -> i32 {
    let mut negative = false;
    let mut cutoff = 0;
    let mut phase = 1;
    let mut range = u32::MAX;
    let mut node = NumContext::ROOT;

    while range != 1 {
        let decision = if low >= cutoff {
            true
        } else if high >= cutoff {
            decoder.decode_context_bit(&mut context.contexts[node])
        } else {
            false
        };

        node = if decision {
            context.right_child(node)
        } else {
            context.left_child(node)
        };

        match phase {
            1 => {
                negative = !decision;
                if negative {
                    let old_low = low;
                    low = -high - 1;
                    high = -old_low - 1;
                }
                phase = 2;
                cutoff = 1;
            }
            2 => {
                if decision {
                    cutoff = cutoff * 2 + 1;
                } else {
                    phase = 3;
                    range = u32::try_from((cutoff + 1) / 2)
                        .expect("JB2 decode_num range should stay positive");
                    if range == 1 {
                        cutoff = 0;
                    } else {
                        cutoff -=
                            i32::try_from(range / 2).expect("range should fit in signed integer");
                    }
                }
            }
            3 => {
                range /= 2;
                if range == 0 {
                    range = 1;
                }
                if range != 1 {
                    if decision {
                        cutoff +=
                            i32::try_from(range / 2).expect("range should fit in signed integer");
                    } else {
                        cutoff -=
                            i32::try_from(range / 2).expect("range should fit in signed integer");
                    }
                } else if !decision {
                    cutoff -= 1;
                }
            }
            _ => unreachable!("JB2 decode_num has only three phases"),
        }
    }

    if negative { -cutoff - 1 } else { cutoff }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RYPKA_PAGE_1_SJBZ: &[u8] = include_bytes!("../tests/fixtures/jb2/rypka-page-1.jb2");

    #[test]
    fn reads_rypka_page_1_image_header() {
        let header = read_jb2_image_header(RYPKA_PAGE_1_SJBZ).expect("JB2 header should decode");

        assert_eq!(
            header,
            Jb2ImageHeader {
                width: 1560,
                height: 1633,
                inherited_dictionary_symbols: 0,
            }
        );
    }

    #[test]
    fn rejects_short_jb2_input() {
        let error = read_jb2_image_header(&[0]).expect_err("short input should fail");

        assert_eq!(error, Jb2Error::new("JB2 input is too short"));
    }

    #[test]
    fn reads_rypka_page_1_direct_record_prefix() {
        let prefix =
            read_jb2_record_prefix(RYPKA_PAGE_1_SJBZ, 8).expect("JB2 prefix should decode");

        assert_eq!(prefix.header.width, 1560);
        assert_eq!(
            prefix.stopped_before,
            Some(Jb2RecordKind::MatchedRefineAddAndBlit)
        );
        assert_eq!(prefix.records.len(), 5);
        assert_eq!(prefix.records[0].kind, Jb2RecordKind::NewSymbolAddAndBlit);
        assert_eq!(prefix.records[0].symbol_width, Some(104));
        assert_eq!(prefix.records[0].symbol_height, Some(68));
        assert_eq!(prefix.records[0].x, Some(0));
        assert_eq!(prefix.records[0].y, Some(1565));
    }

    #[test]
    fn renders_rypka_page_1_image_mask() {
        let partial = render_jb2_image(RYPKA_PAGE_1_SJBZ).expect("JB2 image should render");

        assert_eq!(partial.header.width, 1560);
        assert_eq!(partial.mask.width, 1560);
        assert_eq!(partial.mask.height, 1633);
        assert_eq!(partial.stopped_before, None);
        assert!(partial.reached_end_of_data);
        assert_eq!(partial.dictionary_symbol_count, 285);
        assert_eq!(partial.records.len(), 351);
        assert_eq!(
            partial.records.last().map(|record| record.kind),
            Some(Jb2RecordKind::EndOfData)
        );
        assert_eq!(
            partial.records[5].kind,
            Jb2RecordKind::MatchedRefineAddAndBlit
        );
        assert_eq!(partial.mask.black_pixel_count(), 167_493);
    }

    #[test]
    fn dictionary_symbols_crop_to_black_pixel_bounds() {
        let symbol = Jb2SymbolBitmap {
            width: 4,
            height: 3,
            pixels: vec![
                0, 0, 0, 0, // bottom row
                0, 1, 0, 1, //
                0, 0, 0, 0, // top row
            ],
        };

        let cropped = symbol.crop_to_content();

        assert_eq!(cropped.width, 3);
        assert_eq!(cropped.height, 1);
        assert_eq!(cropped.pixels, [1, 0, 1]);
    }

    #[test]
    fn dictionary_symbols_keep_empty_reference_dimensions() {
        let symbol = Jb2SymbolBitmap {
            width: 4,
            height: 3,
            pixels: vec![0; 12],
        };

        let dictionary_symbol = symbol.dictionary_symbol();

        assert_eq!(dictionary_symbol.width, 4);
        assert_eq!(dictionary_symbol.height, 3);
        assert_eq!(dictionary_symbol.pixels, vec![0; 12]);
    }

    #[test]
    fn baseline_uses_median_of_last_three_values() {
        let mut baseline = Baseline::new();
        baseline.fill(100);
        baseline.add(100);
        baseline.add(140);
        baseline.add(101);

        assert_eq!(baseline.value(), 101);
    }

    #[test]
    fn blits_bottom_origin_symbol_rows_to_top_origin_mask() {
        let symbol = Jb2SymbolBitmap {
            width: 2,
            height: 2,
            pixels: vec![
                1, 0, // bottom row
                0, 1, // top row
            ],
        };
        let mut mask = BitonalBitmap::new(5, 5);

        blit_symbol(&mut mask, &symbol, 1, 1);

        assert_eq!(mask.black_pixel_count(), 2);
        assert_eq!(mask.bit(1, 3), Some(true));
        assert_eq!(mask.bit(2, 2), Some(true));
        assert_eq!(mask.bit(1, 2), Some(false));
        assert_eq!(mask.bit(2, 3), Some(false));
    }
}
