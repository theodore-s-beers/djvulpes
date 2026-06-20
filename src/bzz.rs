use crate::dirm::Dirm;
use crate::error::ParseError;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

pub type BzzResult<T> = std::result::Result<T, BzzError>;

#[derive(Debug, thiserror::Error)]
pub enum BzzError {
    #[error("failed to read compressed DIRM tail: {0}")]
    DirmTail(#[from] ParseError),
    #[error("in-house BZZ decoder is incomplete: {0}")]
    IncompleteDecoder(&'static str),
    #[error("BZZ block declares unsupported size {size} bytes")]
    UnsupportedBlockSize { size: usize },
    #[error("BZZ block declares invalid FSHIFT value {value}")]
    InvalidFshift { value: u8 },
    #[error(
        "BZZ block has invalid Burrows-Wheeler marker position {marker_pos} for block size {block_len}"
    )]
    InvalidBwtMarker { marker_pos: usize, block_len: usize },
    #[error("BZZ block failed Burrows-Wheeler reconstruction check")]
    InvalidBwtTransform,
    #[error("BZZ rank stream ended before {expected_len} block symbols were decoded")]
    TruncatedRankStream { expected_len: usize },
    #[error("BZZ rank stream contains invalid rank {rank}")]
    InvalidSymbolRank { rank: usize },
    #[error("BZZ rank stream does not contain a Burrows-Wheeler marker")]
    MissingBwtMarker,
    #[error("BZZ rank stream contains multiple Burrows-Wheeler markers")]
    DuplicateBwtMarker,
    #[error("failed to write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("failed to run bzz: {0}")]
    Run(io::Error),
    #[error("bzz exited with {status}: {stderr}")]
    CommandFailed { status: ExitStatus, stderr: String },
    #[error("failed to read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
}

/// Decodes `DjVu` BZZ-compressed bytes.
///
/// This is currently backed by the local `bzz` command. Keeping the command
/// dependency behind this API lets callers use decoded `DjVu` payloads without
/// knowing how decompression is implemented, and gives the future pure-Rust
/// decoder one narrow replacement point.
///
/// # Errors
///
/// Returns an error if the external decoder cannot be run, fails, or if its
/// temporary input/output files cannot be written or read.
pub fn decode_bzz(bytes: &[u8]) -> BzzResult<Vec<u8>> {
    match decode_bzz_in_memory(bytes) {
        Err(
            BzzError::IncompleteDecoder(_)
            | BzzError::MissingBwtMarker
            | BzzError::DuplicateBwtMarker
            | BzzError::InvalidSymbolRank { .. }
            | BzzError::InvalidBwtMarker { .. }
            | BzzError::InvalidBwtTransform
            | BzzError::InvalidFshift { .. },
        ) => decode_bzz_with_local_tool(bytes),
        result => result,
    }
}

/// Decodes the BZZ-compressed tail of a `DIRM` chunk.
///
/// # Errors
///
/// Returns an error if the directory tail range is invalid or if BZZ decoding
/// fails.
pub fn decode_dirm_tail(bytes: &[u8], dirm: &Dirm) -> BzzResult<Vec<u8>> {
    let tail = dirm.compressed_tail(bytes)?;
    decode_bzz(tail)
}

fn decode_bzz_with_local_tool(bytes: &[u8]) -> BzzResult<Vec<u8>> {
    let temp_dir = std::env::temp_dir();
    let unique = format!("djvulpes-bzz-{}-{:p}", std::process::id(), bytes.as_ptr());
    let input_path = temp_dir.join(format!("{unique}.bzz"));
    let output_path = temp_dir.join(format!("{unique}.raw"));

    fs::write(&input_path, bytes).map_err(|source| BzzError::Write {
        path: input_path.clone(),
        source,
    })?;

    let output = Command::new("bzz")
        .arg("-d")
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .map_err(BzzError::Run)?;

    let _ = fs::remove_file(&input_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let _ = fs::remove_file(&output_path);
        return Err(BzzError::CommandFailed {
            status: output.status,
            stderr,
        });
    }

    let decoded = fs::read(&output_path).map_err(|source| BzzError::Read {
        path: output_path.clone(),
        source,
    })?;
    let _ = fs::remove_file(&output_path);

    Ok(decoded)
}

fn decode_bzz_in_memory(bytes: &[u8]) -> BzzResult<Vec<u8>> {
    let mut decoder = BzzDecoder::new(bytes);
    let block_size = decoder.next_block_len();

    if block_size == 0 {
        return Ok(Vec::new());
    }
    if block_size > MAX_BLOCK_BYTES {
        return Err(BzzError::UnsupportedBlockSize { size: block_size });
    }

    let fshift = decoder.next_fshift()?;
    let ranks = decoder.decode_block_ranks(block_size, fshift)?;
    decode_block_from_ranks(block_size, fshift, ranks)
}

const MAX_BLOCK_BYTES: usize = 4096 * 1024;
const BWT_MARKER_RANK: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockSymbols {
    symbols: Vec<u8>,
    marker_pos: usize,
}

fn inverse_bwt_block(symbols: &[u8], marker_pos: usize) -> BzzResult<Vec<u8>> {
    if marker_pos == 0 || marker_pos >= symbols.len() {
        return Err(BzzError::InvalidBwtMarker {
            marker_pos,
            block_len: symbols.len(),
        });
    }

    let mut counts = [0usize; 256];
    let mut links = vec![0usize; symbols.len()];

    for (index, symbol) in symbols.iter().copied().enumerate() {
        if index == marker_pos {
            continue;
        }

        let symbol_index = usize::from(symbol);
        links[index] = (symbol_index << 24) | counts[symbol_index];
        counts[symbol_index] += 1;
    }

    let mut next_sorted_index = 1usize;
    for count in &mut counts {
        let symbol_count = *count;
        *count = next_sorted_index;
        next_sorted_index += symbol_count;
    }

    let mut decoded = vec![0; symbols.len() - 1];
    let mut link_index = 0usize;
    for output_index in (0..decoded.len()).rev() {
        let link = links[link_index];
        let symbol = link >> 24;
        decoded[output_index] =
            u8::try_from(symbol).expect("BWT link symbol should fit in one byte");
        link_index = counts[symbol] + (link & 0x00ff_ffff);
    }

    if link_index != marker_pos {
        return Err(BzzError::InvalidBwtTransform);
    }

    Ok(decoded)
}

fn decode_block_from_ranks<I>(block_len: usize, fshift: u8, ranks: I) -> BzzResult<Vec<u8>>
where
    I: IntoIterator<Item = usize>,
{
    let block = block_symbols_from_ranks(block_len, fshift, ranks)?;
    inverse_bwt_block(&block.symbols, block.marker_pos)
}

fn collect_block_ranks<R>(block_len: usize, decoder: &mut R) -> BzzResult<Vec<usize>>
where
    R: RankDecoder,
{
    let mut ranks = Vec::with_capacity(block_len);
    for _ in 0..block_len {
        ranks.push(decoder.next_rank()?);
    }
    Ok(ranks)
}

fn block_symbols_from_ranks<I>(block_len: usize, fshift: u8, ranks: I) -> BzzResult<BlockSymbols>
where
    I: IntoIterator<Item = usize>,
{
    let mut table = BzzMoveToFrontTable::new(fshift);
    let mut symbols = Vec::with_capacity(block_len);
    let mut marker_pos = None;
    let mut ranks = ranks.into_iter();

    for index in 0..block_len {
        let Some(rank) = ranks.next() else {
            return Err(BzzError::TruncatedRankStream {
                expected_len: block_len,
            });
        };

        if rank == BWT_MARKER_RANK {
            if marker_pos.replace(index).is_some() {
                return Err(BzzError::DuplicateBwtMarker);
            }
            symbols.push(0);
        } else {
            symbols.push(table.take(rank)?);
        }
    }

    let Some(marker_pos) = marker_pos else {
        return Err(BzzError::MissingBwtMarker);
    };

    Ok(BlockSymbols {
        symbols,
        marker_pos,
    })
}

struct BzzMoveToFrontTable {
    symbols: Vec<u8>,
    freq: [u32; 4],
    fadd: u32,
    fshift: u8,
}

impl BzzMoveToFrontTable {
    fn new(fshift: u8) -> Self {
        Self {
            symbols: (0..=u8::MAX).collect(),
            freq: [0; 4],
            fadd: 4,
            fshift,
        }
    }

    fn take(&mut self, rank: usize) -> BzzResult<u8> {
        if rank >= self.symbols.len() {
            return Err(BzzError::InvalidSymbolRank { rank });
        }

        self.update_fadd();
        let symbol = self.symbols.remove(rank);
        let symbol_freq = self
            .freq
            .get(rank)
            .copied()
            .unwrap_or(0)
            .saturating_add(self.fadd);
        let insert_at = self.insertion_index(rank, symbol_freq);
        self.symbols.insert(insert_at, symbol);
        self.rotate_freq(rank, insert_at, symbol_freq);
        Ok(symbol)
    }

    #[cfg(test)]
    fn rank_of(&self, symbol: u8) -> Option<usize> {
        self.symbols
            .iter()
            .position(|candidate| *candidate == symbol)
    }

    fn update_fadd(&mut self) {
        self.fadd = self
            .fadd
            .saturating_add(self.fadd >> u32::from(self.fshift));

        if self.fadd > 0x1000_0000 {
            self.fadd /= 0x1000_0000;
            for freq in &mut self.freq {
                *freq /= 0x1000_0000;
            }
        }
    }

    fn insertion_index(&self, rank: usize, symbol_freq: u32) -> usize {
        let max_insert = rank.min(3);
        let mut insert_at = 0;

        while insert_at < max_insert && self.freq[insert_at] >= symbol_freq {
            insert_at += 1;
        }

        insert_at
    }

    fn rotate_freq(&mut self, rank: usize, insert_at: usize, symbol_freq: u32) {
        if rank < 4 {
            for index in (insert_at + 1..=rank).rev() {
                self.freq[index] = self.freq[index - 1];
            }
        } else {
            for index in (insert_at + 1..4).rev() {
                self.freq[index] = self.freq[index - 1];
            }
        }

        self.freq[insert_at] = symbol_freq;
    }
}

struct BzzDecoder<'a> {
    bits: ZpBitReader<'a>,
}

impl<'a> BzzDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bits: ZpBitReader::new(bytes),
        }
    }

    fn next_block_len(&mut self) -> usize {
        let mut value = 1usize;
        let block_len_marker = 1usize << 24;

        while value < block_len_marker {
            value = (value << 1) | usize::from(self.bits.read_raw_bit());
        }

        value - block_len_marker
    }

    fn next_fshift(&mut self) -> BzzResult<u8> {
        let value = (self.bits.read_raw_bit() << 1) | self.bits.read_raw_bit();
        if value > 2 {
            return Err(BzzError::InvalidFshift { value });
        }
        Ok(value)
    }

    fn decode_block_ranks(&mut self, block_len: usize, _fshift: u8) -> BzzResult<Vec<usize>> {
        let mut ranks = EntropyRankDecoder::new(&mut self.bits);
        collect_block_ranks(block_len, &mut ranks)
    }
}

trait RankDecoder {
    fn next_rank(&mut self) -> BzzResult<usize>;
}

struct EntropyRankDecoder<'bits, 'input> {
    bits: &'bits mut ZpBitReader<'input>,
    model: BitModel,
    previous_mtfno: usize,
}

impl<'bits, 'input> EntropyRankDecoder<'bits, 'input> {
    fn new(bits: &'bits mut ZpBitReader<'input>) -> Self {
        Self {
            bits,
            model: BitModel::with_contexts(ENTROPY_CONTEXTS),
            previous_mtfno: 0,
        }
    }
}

impl RankDecoder for EntropyRankDecoder<'_, '_> {
    fn next_rank(&mut self) -> BzzResult<usize> {
        let rank = read_entropy_rank(&mut self.model, self.bits, self.previous_mtfno)?;
        self.previous_mtfno = rank;
        Ok(rank)
    }
}

const BZZ_MTF_CONTEXTS: usize = 262;
const ENTROPY_CONTEXTS: usize = BZZ_MTF_CONTEXTS;
const RANK_ZERO_CONTEXT_START: usize = 0;
const RANK_ONE_CONTEXT_START: usize = 3;
const BZZ_MTF_RANGES: [BzzMtfRange; 7] = [
    BzzMtfRange::new(2, 6, 7, 1),
    BzzMtfRange::new(4, 8, 9, 2),
    BzzMtfRange::new(8, 12, 13, 3),
    BzzMtfRange::new(16, 20, 21, 4),
    BzzMtfRange::new(32, 36, 37, 5),
    BzzMtfRange::new(64, 68, 69, 6),
    BzzMtfRange::new(128, 132, 133, 7),
];

#[derive(Debug, Clone, Copy)]
struct BzzMtfRange {
    start: usize,
    selector_context: usize,
    tree_context: usize,
    bits: usize,
}

impl BzzMtfRange {
    const fn new(start: usize, selector_context: usize, tree_context: usize, bits: usize) -> Self {
        Self {
            start,
            selector_context,
            tree_context,
            bits,
        }
    }
}

fn read_entropy_rank<R>(
    model: &mut BitModel,
    reader: &mut R,
    previous_mtfno: usize,
) -> BzzResult<usize>
where
    R: ModeledBitReader,
{
    let previous_class = previous_mtfno.min(2);
    if model.read_bit(reader, RANK_ZERO_CONTEXT_START + previous_class)? == 1 {
        return Ok(0);
    }
    if model.read_bit(reader, RANK_ONE_CONTEXT_START + previous_class)? == 1 {
        return Ok(1);
    }

    for range in BZZ_MTF_RANGES {
        if model.read_bit(reader, range.selector_context)? == 1 {
            let offset = read_bzz_mtfno_bin(model, reader, range.bits, range.tree_context)?;
            return Ok(range.start + offset);
        }
    }

    Ok(BWT_MARKER_RANK)
}

fn read_bzz_mtfno_bin<R>(
    model: &mut BitModel,
    reader: &mut R,
    bits: usize,
    context_start: usize,
) -> BzzResult<usize>
where
    R: ModeledBitReader,
{
    let mut value = 0usize;
    let mut node = 0usize;

    for _ in 0..bits {
        let bit = model.read_bit(reader, context_start + node)?;
        value = (value << 1) | usize::from(bit);
        node = (node << 1) + 1 + usize::from(bit);
    }

    Ok(value)
}

trait ModeledBitReader {
    fn read_modeled_bit(&mut self, split: u16) -> BzzResult<u8>;
}

#[derive(Debug, Clone)]
struct BitModel {
    contexts: Vec<BitContext>,
    tables: BitModelTables,
}

impl BitModel {
    fn with_contexts(count: usize) -> Self {
        Self {
            contexts: vec![BitContext::default(); count],
            tables: BitModelTables::generated(),
        }
    }

    fn read_bit<R>(&mut self, reader: &mut R, context_id: usize) -> BzzResult<u8>
    where
        R: ModeledBitReader,
    {
        let context = &mut self.contexts[context_id];
        let bit = reader.read_modeled_bit(self.tables.split(context.state))?;
        context.observe(bit, &self.tables);
        Ok(bit)
    }

    #[cfg(test)]
    fn context(&self, context_id: usize) -> &BitContext {
        &self.contexts[context_id]
    }

    #[cfg(test)]
    fn context_mut(&mut self, context_id: usize) -> &mut BitContext {
        &mut self.contexts[context_id]
    }
}

impl ModeledBitReader for ZpBitReader<'_> {
    fn read_modeled_bit(&mut self, split: u16) -> BzzResult<u8> {
        Ok(self.read_split_bit(u32::from(split)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BitContext {
    state: u8,
}

impl Default for BitContext {
    fn default() -> Self {
        Self {
            state: BIT_MODEL_INITIAL_STATE,
        }
    }
}

impl BitContext {
    const fn observe(&mut self, bit: u8, tables: &BitModelTables) {
        self.state = tables.next_state(self.state, bit);
    }
}

const BIT_MODEL_STATES: usize = 65;
const BIT_MODEL_INITIAL_STATE: u8 = 32;

#[derive(Debug, Clone)]
struct BitModelTables {
    split: [u16; BIT_MODEL_STATES],
    next_zero: [u8; BIT_MODEL_STATES],
    next_one: [u8; BIT_MODEL_STATES],
}

impl BitModelTables {
    fn generated() -> Self {
        let mut split = [0; BIT_MODEL_STATES];
        let mut next_zero = [0; BIT_MODEL_STATES];
        let mut next_one = [0; BIT_MODEL_STATES];

        for state in 0..BIT_MODEL_STATES {
            split[state] = Self::split_for_state(state);
            next_zero[state] = u8::try_from((state + 3).min(BIT_MODEL_STATES - 1))
                .expect("bit-model state should fit in u8");
            next_one[state] =
                u8::try_from(state.saturating_sub(3)).expect("bit-model state should fit in u8");
        }

        Self {
            split,
            next_zero,
            next_one,
        }
    }

    const fn split(&self, state: u8) -> u16 {
        self.split[state as usize]
    }

    const fn next_state(&self, state: u8, bit: u8) -> u8 {
        if bit == 0 {
            self.next_zero[state as usize]
        } else {
            self.next_one[state as usize]
        }
    }

    fn split_for_state(state: usize) -> u16 {
        let max = usize::from(u16::MAX);
        let position = state + 1;
        let linear = position * max / BIT_MODEL_STATES;
        let boost = 11_500 * 4 * position * (BIT_MODEL_STATES - position)
            / (BIT_MODEL_STATES * BIT_MODEL_STATES);
        u16::try_from((linear + boost).min(max))
            .expect("generated bit-model split should fit in u16")
    }
}

struct ZpBitReader<'a> {
    input: &'a [u8],
    cursor: usize,
    lower_bound: u16,
    code_window: u16,
    shift_buffer: u64,
    buffered_bits: u8,
    end_padding: u8,
}

impl<'a> ZpBitReader<'a> {
    fn new(input: &'a [u8]) -> Self {
        let (code_window, cursor) = match input {
            [first, second, ..] => (u16::from_be_bytes([*first, *second]), 2),
            [first] => ((u16::from(*first) << 8) | 0x00ff, 1),
            [] => (0xffff, 0),
        };

        Self {
            input,
            cursor,
            lower_bound: 0,
            code_window,
            shift_buffer: 0,
            buffered_bits: 0,
            end_padding: 25,
        }
    }

    const fn lower_bound(&self) -> u16 {
        self.lower_bound
    }

    fn read_raw_bit(&mut self) -> u8 {
        let split = 0x8000 + (u32::from(self.lower_bound()) >> 1);
        self.read_split_bit(split)
    }

    fn read_split_bit(&mut self, mut split: u32) -> u8 {
        self.refill();

        let mut lower_bound = u32::from(self.lower_bound);
        let mut code_window = u32::from(self.code_window);
        let bit;

        if split > code_window {
            bit = 1;
            split = 0x10000 - split;
            lower_bound += split;
            code_window += split;

            let shift = if lower_bound >= 0xff00 {
                leading_one_bits((lower_bound & 0x00ff) as u8) + 8
            } else {
                leading_one_bits(((lower_bound >> 8) & 0x00ff) as u8)
            };
            self.buffered_bits = self.buffered_bits.saturating_sub(shift);
            lower_bound = (lower_bound << shift) & 0xffff;
            code_window = ((code_window << shift) & 0xffff) | self.take_buffered_bits(shift);
        } else {
            bit = 0;
            self.buffered_bits = self.buffered_bits.saturating_sub(1);
            lower_bound = (split << 1) & 0xffff;
            code_window = ((code_window << 1) & 0xffff) | self.take_buffered_bits(1);
        }

        self.lower_bound = u16::try_from(lower_bound).expect("renormalized bound should fit u16");
        self.code_window = u16::try_from(code_window).expect("renormalized code should fit u16");
        bit
    }

    #[cfg(test)]
    fn decision_probe(&mut self) -> DecisionProbe {
        self.refill();
        DecisionProbe {
            lower_bound: self.lower_bound,
            code_window: self.code_window,
            cursor: self.cursor,
            buffered_bits: self.buffered_bits,
        }
    }

    fn refill(&mut self) {
        if self.buffered_bits >= 16 {
            return;
        }

        while self.buffered_bits <= 24 {
            let byte = if let Some(byte) = self.input.get(self.cursor) {
                self.cursor += 1;
                *byte
            } else {
                self.end_padding = self.end_padding.saturating_sub(1);
                0xff
            };

            self.shift_buffer = (self.shift_buffer << 8) | u64::from(byte);
            self.buffered_bits += 8;
        }
    }

    fn take_buffered_bits(&self, count: u8) -> u32 {
        u32::try_from((self.shift_buffer >> self.buffered_bits) & ((1u64 << count) - 1))
            .expect("requested bit buffer slice should fit in u32")
    }
}

#[cfg(test)]
struct SpecZpDecoder<'a> {
    input: &'a [u8],
    cursor: usize,
    a: u32,
    c: u32,
    fence: u32,
    bit_buffer: u32,
    buffered_bits: i32,
}

#[cfg(test)]
impl<'a> SpecZpDecoder<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut decoder = Self {
            input,
            cursor: 0,
            a: 0,
            c: 0,
            fence: 0,
            bit_buffer: 0,
            buffered_bits: 0,
        };
        let high = u32::from(decoder.read_byte());
        let low = u32::from(decoder.read_byte());
        decoder.c = (high << 8) | low;
        decoder.refill();
        decoder.fence = decoder.c.min(0x7fff);
        decoder
    }

    fn decode_raw(&mut self) -> u8 {
        let z = 0x8000 + (self.a >> 1);
        self.decode_passthrough_with_threshold(z)
    }

    fn decode_passthrough_with_threshold(&mut self, z: u32) -> u8 {
        let bit;

        if z > self.c {
            bit = 1;
            self.a += 0x10000 - z;
            self.c += 0x10000 - z;
            self.renormalize();
        } else {
            bit = 0;
            self.buffered_bits -= 1;
            self.a = (z << 1) & 0xffff;
            self.c = ((self.c << 1) | ((self.bit_buffer >> self.buffered_bits) & 1)) & 0xffff;
            if self.buffered_bits < 16 {
                self.refill();
            }
            self.fence = self.c.min(0x7fff);
        }

        bit
    }

    fn decode_bit<T>(&mut self, context: &mut u8, tables: &T) -> u8
    where
        T: SpecZpTables,
    {
        let state = *context;
        let mut z = self.a + u32::from(tables.delta(state));
        if z <= self.fence {
            self.a = z;
            return state & 1;
        }

        let d = 0x6000 + ((z + self.a) >> 2);
        z = z.min(d);

        let bit;
        if z > self.c {
            bit = 1 - (state & 1);
            self.a += 0x10000 - z;
            self.c += 0x10000 - z;
            *context = tables.lambda(state);
            self.renormalize();
        } else {
            bit = state & 1;
            if self.a >= u32::from(tables.theta(state)) {
                *context = tables.mu(state);
            }
            self.buffered_bits -= 1;
            self.a = (z << 1) & 0xffff;
            self.c = ((self.c << 1) | ((self.bit_buffer >> self.buffered_bits) & 1)) & 0xffff;
            if self.buffered_bits < 16 {
                self.refill();
            }
            self.fence = self.c.min(0x7fff);
        }

        bit
    }

    fn renormalize(&mut self) {
        let shift = u16::try_from(self.a)
            .expect("ZP interval register should fit in 16 bits")
            .leading_ones();
        self.buffered_bits -= i32::try_from(shift).expect("renormalization shift should fit i32");
        self.a = (self.a << shift) & 0xffff;
        let mask = (1u32 << shift) - 1;
        self.c = ((self.c << shift) | ((self.bit_buffer >> self.buffered_bits) & mask)) & 0xffff;
        if self.buffered_bits < 16 {
            self.refill();
        }
        self.fence = self.c.min(0x7fff);
    }

    fn refill(&mut self) {
        while self.buffered_bits <= 24 {
            self.bit_buffer = (self.bit_buffer << 8) | u32::from(self.read_byte());
            self.buffered_bits += 8;
        }
    }

    fn read_byte(&mut self) -> u8 {
        let byte = self.input.get(self.cursor).copied().unwrap_or(0xff);
        self.cursor = self.cursor.wrapping_add(1);
        byte
    }
}

#[cfg(test)]
trait SpecZpTables {
    fn delta(&self, state: u8) -> u16;
    fn theta(&self, state: u8) -> u16;
    fn mu(&self, state: u8) -> u8;
    fn lambda(&self, state: u8) -> u8;
}

#[cfg(test)]
trait ZpTableGenerator {
    fn delta(&self, state: usize) -> u16;
    fn theta(&self, state: usize) -> u16;
    fn mu(&self, state: usize) -> u8;
    fn lambda(&self, state: usize) -> u8;
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ZpTableSet {
    delta: [u16; 256],
    theta: [u16; 256],
    mu: [u8; 256],
    lambda: [u8; 256],
}

#[cfg(test)]
impl ZpTableSet {
    fn generate(generator: &impl ZpTableGenerator) -> Self {
        let mut delta = [0; 256];
        let mut theta = [0; 256];
        let mut mu = [0; 256];
        let mut lambda = [0; 256];

        for state in 0..256 {
            delta[state] = generator.delta(state);
            theta[state] = generator.theta(state);
            mu[state] = generator.mu(state);
            lambda[state] = generator.lambda(state);
        }

        Self {
            delta,
            theta,
            mu,
            lambda,
        }
    }

    fn validate_shape(&self) {
        for state in 0..=250 {
            assert!(
                self.delta[state] <= 0x8000,
                "probability delta for state {state} should fit the ZP interval"
            );
            assert!(
                self.theta[state] <= 0x7fff,
                "threshold for state {state} should fit the ZP fence"
            );
            assert!(
                usize::from(self.mu[state]) <= 250,
                "MPS transition for state {state} should stay in valid states"
            );
            assert!(
                usize::from(self.lambda[state]) <= 250,
                "LPS transition for state {state} should stay in valid states"
            );
        }

        for state in 251..=255 {
            assert!(
                self.delta[state] <= 0x8000,
                "padding probability delta for state {state} should fit the ZP interval"
            );
            assert_eq!(self.theta[state], 0);
            assert_eq!(self.mu[state], 0);
            assert_eq!(self.lambda[state], 0);
        }
    }
}

#[cfg(test)]
impl SpecZpTables for ZpTableSet {
    fn delta(&self, state: u8) -> u16 {
        self.delta[usize::from(state)]
    }

    fn theta(&self, state: u8) -> u16 {
        self.theta[usize::from(state)]
    }

    fn mu(&self, state: u8) -> u8 {
        self.mu[usize::from(state)]
    }

    fn lambda(&self, state: u8) -> u8 {
        self.lambda[usize::from(state)]
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct DecisionProbe {
    lower_bound: u16,
    code_window: u16,
    cursor: usize,
    buffered_bits: u8,
}

const fn leading_one_bits(byte: u8) -> u8 {
    let mut count = 0;
    let mut value = byte;

    while value & 0x80 != 0 {
        count += 1;
        value <<= 1;
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dirm;
    use std::io::ErrorKind;

    const HELLO_BZZ: &[u8] = include_bytes!("../tests/fixtures/bzz/hello.bzz");
    const HELLO_RAW: &[u8] = include_bytes!("../tests/fixtures/bzz/hello.raw");

    #[test]
    fn decode_dirm_tail_rejects_invalid_tail_range_before_decoding() {
        let dirm = Dirm {
            flags: 0,
            entry_count: 0,
            offsets: Vec::new(),
            compressed_tail_start: 2,
            compressed_tail_len: 4,
        };

        let error = decode_dirm_tail(&[0, 1, 2], &dirm)
            .expect_err("invalid tail range should fail before invoking bzz");

        assert!(matches!(error, BzzError::DirmTail(_)));
    }

    #[test]
    fn decode_bzz_decodes_fixture_bytes() {
        match decode_bzz(HELLO_BZZ) {
            Ok(decoded) => assert_eq!(decoded, HELLO_RAW),
            Err(BzzError::Run(error)) if error.kind() == ErrorKind::NotFound => {
                eprintln!("skipping fixture decode test because `bzz` is not on PATH");
            }
            Err(error) => panic!("fixture decode failed: {error}"),
        }
    }

    #[test]
    fn bzz_decoder_reads_fixture_block_len() {
        let mut decoder = BzzDecoder::new(HELLO_BZZ);

        assert_eq!(decoder.next_block_len(), HELLO_RAW.len() + 1);
    }

    #[test]
    fn bzz_decoder_reads_fixture_fshift_after_block_len() {
        let mut decoder = BzzDecoder::new(HELLO_BZZ);

        assert_eq!(decoder.next_block_len(), HELLO_RAW.len() + 1);
        assert_eq!(
            decoder.next_fshift().expect("fixture FSHIFT should decode"),
            0
        );
    }

    #[test]
    fn spec_zp_raw_decoder_reads_fixture_block_header() {
        let mut decoder = SpecZpDecoder::new(HELLO_BZZ);

        assert_eq!(spec_zp_next_block_len(&mut decoder), HELLO_RAW.len() + 1);
        assert_eq!(spec_zp_next_fshift(&mut decoder), 0);
    }

    #[test]
    fn spec_zp_context_decode_updates_mps_and_lps_states() {
        let tables = ScriptedSpecZpTables;
        let mut mps_decoder = SpecZpDecoder::new(&[0xff, 0xff]);
        let mut mps_context = 0;
        let mut lps_decoder = SpecZpDecoder::new(&[0x00, 0x00]);
        let mut lps_context = 0;

        let mps_bit = mps_decoder.decode_bit(&mut mps_context, &tables);
        let lps_bit = lps_decoder.decode_bit(&mut lps_context, &tables);

        assert_eq!(mps_bit, 0);
        assert_eq!(mps_context, 83);
        assert_eq!(lps_bit, 1);
        assert_eq!(lps_context, 84);
    }

    #[test]
    fn spec_zp_context_decode_matches_djvu_zp_oracle() {
        let tables = candidate_zp_table_set();
        let cases: &[&[u8]] = &[
            HELLO_BZZ,
            &[0xff, 0xff, 0xff, 0xff],
            &[0x00, 0x00, 0x00, 0x00],
            &[0x55, 0xaa, 0x33, 0xcc],
            &[0x80, 0x00, 0x7f, 0xff, 0x01],
        ];

        for input in cases {
            let mut ours = SpecZpDecoder::new(input);
            let mut oracle =
                djvu_zp::ZpDecoder::new(input).expect("test case should initialize ZP");
            let mut our_contexts = [0; 16];
            let mut oracle_contexts = [0; 16];

            for step in 0..256 {
                let context = (step * 7) % our_contexts.len();
                let our_bit = ours.decode_bit(&mut our_contexts[context], &tables) != 0;
                let oracle_bit = oracle.decode_bit(&mut oracle_contexts[context]);

                assert_eq!(
                    our_bit, oracle_bit,
                    "bit mismatch for input {input:02x?}, step {step}, context {context}"
                );
                assert_eq!(
                    our_contexts[context], oracle_contexts[context],
                    "context mismatch for input {input:02x?}, step {step}, context {context}"
                );
            }
        }
    }

    #[test]
    fn spec_zp_mixed_passthrough_and_context_decode_matches_djvu_zp_oracle() {
        let tables = candidate_zp_table_set();
        let mut ours = SpecZpDecoder::new(HELLO_BZZ);
        let mut oracle = djvu_zp::ZpDecoder::new(HELLO_BZZ).expect("fixture should initialize ZP");

        for step in 0..26 {
            assert_eq!(
                ours.decode_raw() != 0,
                oracle.decode_passthrough(),
                "passthrough mismatch at step {step}"
            );
        }

        let mut our_contexts = [0; 16];
        let mut oracle_contexts = [0; 16];
        for step in 0..256 {
            let context = (step * 7) % our_contexts.len();
            let our_bit = ours.decode_bit(&mut our_contexts[context], &tables) != 0;
            let oracle_bit = oracle.decode_bit(&mut oracle_contexts[context]);

            assert_eq!(
                our_bit, oracle_bit,
                "bit mismatch after passthrough at step {step}, context {context}"
            );
            assert_eq!(
                our_contexts[context], oracle_contexts[context],
                "context mismatch after passthrough at step {step}, context {context}"
            );
        }
    }

    #[test]
    fn djvu_zp_oracle_tables_have_expected_public_shape() {
        assert_eq!(djvu_zp::tables::PROB.len(), 256);
        assert_eq!(djvu_zp::tables::THRESHOLD.len(), 256);
        assert_eq!(djvu_zp::tables::MPS_NEXT.len(), 256);
        assert_eq!(djvu_zp::tables::LPS_NEXT.len(), 256);

        djvu_zp_oracle_table_set().validate_shape();
    }

    #[test]
    fn zp_table_set_can_be_generated_from_dev_oracle() {
        let tables = ZpTableSet::generate(&DevOracleZpTableGenerator);

        assert_eq!(tables.delta, djvu_zp::tables::PROB);
        assert_eq!(tables.theta, djvu_zp::tables::THRESHOLD);
        assert_eq!(tables.mu, djvu_zp::tables::MPS_NEXT);
        assert_eq!(tables.lambda, djvu_zp::tables::LPS_NEXT);
    }

    #[test]
    fn candidate_zp_table_set_matches_dev_oracle() {
        assert_eq!(candidate_zp_table_set(), djvu_zp_oracle_table_set());
    }

    #[test]
    fn zp_table_phases_match_spec_structure() {
        let tables = candidate_zp_table_set();

        assert_eq!(tables.theta[0], 0);
        assert_eq!(tables.theta[1], 0);
        assert_eq!(tables.theta[2], 0);

        for state in 3..=82 {
            assert!(
                tables.theta[state] > 0,
                "early-estimation state {state} should have a nonzero MPS update threshold"
            );
        }

        for state in 83..=250 {
            assert_eq!(
                tables.theta[state], 0,
                "steady-state state {state} should update on every decoded MPS"
            );
        }
    }

    #[test]
    fn zp_initial_context_transitions_enter_late_estimation() {
        let tables = candidate_zp_table_set();

        assert!(
            (83..=250).contains(&usize::from(tables.mu[0])),
            "first MPS from a fresh context should leave the early-estimation range"
        );
        assert!(
            (83..=250).contains(&usize::from(tables.lambda[0])),
            "first LPS from a fresh context should leave the early-estimation range"
        );
        assert_eq!(
            tables.mu[0] & 1,
            0,
            "first MPS from a fresh context should keep zero as the MPS"
        );
        assert_eq!(
            tables.lambda[0] & 1,
            1,
            "first LPS from a fresh context should make one the MPS"
        );
    }

    #[test]
    fn zp_early_estimation_pairs_share_probability_and_threshold() {
        let tables = candidate_zp_table_set();

        for state in (3..=81).step_by(2) {
            assert_eq!(
                tables.delta[state],
                tables.delta[state + 1],
                "early-estimation states {state} and {} should share probability delta",
                state + 1
            );
            assert_eq!(
                tables.theta[state],
                tables.theta[state + 1],
                "early-estimation states {state} and {} should share MPS threshold",
                state + 1
            );
        }
    }

    #[test]
    #[ignore = "prints residuals for candidate theta formulas against the dev oracle"]
    fn fixture_theta_candidate_formula_residuals() {
        let tables = candidate_zp_table_set();
        let mut max_error = 0;
        let mut max_error_state = 0;

        for state in (3..=81).step_by(2) {
            let expected = dev_oracle_theta(state);
            let estimated = estimate_theta_from_delta(tables.delta[state]);
            let error = expected.abs_diff(estimated);

            if error > max_error {
                max_error = error;
                max_error_state = state;
            }

            eprintln!(
                "state={state:>2} delta=0x{:04x} expected=0x{expected:04x} estimated=0x{estimated:04x} error={error}",
                tables.delta[state],
            );
        }

        eprintln!("max_error={max_error} at state={max_error_state}");
    }

    #[test]
    fn in_memory_decoder_reports_incomplete_after_block_header() {
        let error =
            decode_bzz_in_memory(HELLO_BZZ).expect_err("full in-memory decode is not complete yet");

        assert!(matches!(
            error,
            BzzError::MissingBwtMarker
                | BzzError::DuplicateBwtMarker
                | BzzError::InvalidSymbolRank { .. }
                | BzzError::InvalidBwtMarker { .. }
                | BzzError::InvalidBwtTransform
        ));
    }

    #[test]
    fn bzz_decoder_rank_decode_returns_block_len_with_provisional_model() {
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let block_len = decoder.next_block_len();
        let fshift = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let ranks = decoder
            .decode_block_ranks(block_len, fshift)
            .expect("provisional rank decoder should produce one block of ranks");

        assert_eq!(ranks.len(), block_len);
    }

    #[test]
    #[ignore = "tracks true ZP rank decoding against BWT-derived fixture ranks"]
    fn spec_zp_rank_stream_matches_expected_fixture_ranks() {
        let tables = candidate_zp_table_set();
        let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
        let block_len = spec_zp_next_block_len(&mut decoder);
        let fshift = spec_zp_next_fshift(&mut decoder);
        let mut contexts = [0; BZZ_MTF_CONTEXTS];
        let mut previous_mtfno = 0;
        let mut ranks = Vec::with_capacity(block_len);

        for _ in 0..block_len {
            let rank =
                read_spec_zp_entropy_rank(&mut decoder, &mut contexts, &tables, previous_mtfno);
            previous_mtfno = rank;
            ranks.push(rank);
        }

        let expected = expected_ranks_from_raw_with_fshift(HELLO_RAW, fshift);
        if ranks != expected {
            let mismatch = ranks
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected)
                .expect("rank vectors should differ");
            eprintln!(
                "first mismatch at rank {mismatch}: actual={} expected={}",
                ranks[mismatch], expected[mismatch]
            );
            eprintln!("actual ranks: {ranks:?}");
            eprintln!("expected ranks: {expected:?}");
        }

        assert_eq!(ranks, expected);
    }

    #[test]
    #[ignore = "prints first true-ZP rank under previous-MTFNO context class variants"]
    fn fixture_first_spec_zp_rank_previous_context_variants() {
        let tables = candidate_zp_table_set();

        for previous_mtfno in [0, 1, 2, BWT_MARKER_RANK] {
            let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
            let block_len = spec_zp_next_block_len(&mut decoder);
            let fshift = spec_zp_next_fshift(&mut decoder);
            let mut contexts = [0; BZZ_MTF_CONTEXTS];
            let rank =
                read_spec_zp_entropy_rank(&mut decoder, &mut contexts, &tables, previous_mtfno);

            eprintln!(
                "previous_mtfno={previous_mtfno} previous_class={} first_rank={rank} block_len={block_len} fshift={fshift}",
                previous_mtfno.min(2)
            );
        }

        eprintln!(
            "expected_first_rank={}",
            expected_ranks_from_raw_with_fshift(HELLO_RAW, 0)[0]
        );
    }

    #[test]
    #[ignore = "prints the first true-ZP rank decision path beside the expected path"]
    fn fixture_first_spec_zp_rank_decision_path() {
        let tables = candidate_zp_table_set();
        let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
        let _ = spec_zp_next_block_len(&mut decoder);
        let _ = spec_zp_next_fshift(&mut decoder);
        let mut contexts = [0; BZZ_MTF_CONTEXTS];
        let mut actual_path = Vec::new();

        let previous_class = 0;
        let bit = decoder.decode_bit(
            &mut contexts[RANK_ZERO_CONTEXT_START + previous_class],
            &tables,
        );
        actual_path.push((RANK_ZERO_CONTEXT_START + previous_class, bit));
        if bit == 0 {
            let bit = decoder.decode_bit(
                &mut contexts[RANK_ONE_CONTEXT_START + previous_class],
                &tables,
            );
            actual_path.push((RANK_ONE_CONTEXT_START + previous_class, bit));
            if bit == 0 {
                'ranges: for range in BZZ_MTF_RANGES {
                    let bit = decoder.decode_bit(&mut contexts[range.selector_context], &tables);
                    actual_path.push((range.selector_context, bit));
                    if bit == 1 {
                        let mut node = 0;
                        for _ in 0..range.bits {
                            let context = range.tree_context + node;
                            let bit = decoder.decode_bit(&mut contexts[context], &tables);
                            actual_path.push((context, bit));
                            node = (node << 1) + 1 + usize::from(bit);
                        }
                        break 'ranges;
                    }
                }
            }
        }

        let expected_rank = expected_ranks_from_raw_with_fshift(HELLO_RAW, 0)[0];
        let expected_path = bzz_mtfno_context_path(expected_rank, 0);

        eprintln!("actual first-rank path: {actual_path:?}");
        eprintln!("expected rank: {expected_rank}");
        eprintln!("expected first-rank path: {expected_path:?}");
    }

    #[test]
    #[ignore = "prints first true-ZP rank for candidate raw-bit skips after the block length"]
    fn fixture_first_spec_zp_rank_raw_skip_variants() {
        let tables = candidate_zp_table_set();
        let expected = expected_ranks_from_raw_with_fshift(HELLO_RAW, 0);

        for raw_skip in 0..=32 {
            let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
            let block_len = spec_zp_next_block_len(&mut decoder);
            let mut raw_bits = Vec::new();
            for _ in 0..raw_skip {
                raw_bits.push(decoder.decode_raw());
            }

            let mut contexts = [0; BZZ_MTF_CONTEXTS];
            let mut previous_mtfno = 0;
            let mut ranks = Vec::new();
            for _ in 0..expected.len() {
                let rank =
                    read_spec_zp_entropy_rank(&mut decoder, &mut contexts, &tables, previous_mtfno);
                previous_mtfno = rank;
                ranks.push(rank);
            }
            let matching_prefix = ranks
                .iter()
                .zip(&expected)
                .take_while(|(actual, expected)| actual == expected)
                .count();

            eprintln!(
                "raw_skip={raw_skip} raw_bits={raw_bits:?} first_rank={} expected_first={} matching_prefix={matching_prefix} block_len={block_len}",
                ranks[0], expected[0]
            );
        }
    }

    #[test]
    #[ignore = "prints true-ZP rank stream after the one-raw-bit alignment candidate"]
    fn fixture_spec_zp_one_raw_skip_rank_stream() {
        let tables = candidate_zp_table_set();
        let expected = expected_ranks_from_raw_with_fshift(HELLO_RAW, 0);
        let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
        let block_len = spec_zp_next_block_len(&mut decoder);
        let skipped = decoder.decode_raw();
        let mut contexts = [0; BZZ_MTF_CONTEXTS];
        let mut previous_mtfno = 0;
        let mut ranks = Vec::with_capacity(block_len);

        for _ in 0..block_len {
            let rank =
                read_spec_zp_entropy_rank(&mut decoder, &mut contexts, &tables, previous_mtfno);
            previous_mtfno = rank;
            ranks.push(rank);
        }

        let mismatch = ranks
            .iter()
            .zip(&expected)
            .position(|(actual, expected)| actual != expected);

        eprintln!("skipped_raw_bit={skipped}");
        eprintln!("first_mismatch={mismatch:?}");
        eprintln!("actual ranks: {ranks:?}");
        eprintln!("expected ranks: {expected:?}");
    }

    #[test]
    #[ignore = "prints first rank for selector-layout variants around the 4..7 group"]
    fn fixture_first_spec_zp_rank_selector_layout_variants() {
        let tables = candidate_zp_table_set();
        let expected = expected_ranks_from_raw_with_fshift(HELLO_RAW, 0);
        let variants: &[(&str, &[BzzMtfRange])] = &[
            ("spec", &BZZ_MTF_RANGES),
            (
                "skip_4_7",
                &[
                    BzzMtfRange::new(2, 6, 7, 1),
                    BzzMtfRange::new(8, 12, 13, 3),
                    BzzMtfRange::new(16, 20, 21, 4),
                    BzzMtfRange::new(32, 36, 37, 5),
                    BzzMtfRange::new(64, 68, 69, 6),
                    BzzMtfRange::new(128, 132, 133, 7),
                ],
            ),
            (
                "swap_4_7_and_8_15",
                &[
                    BzzMtfRange::new(2, 6, 7, 1),
                    BzzMtfRange::new(8, 12, 13, 3),
                    BzzMtfRange::new(4, 8, 9, 2),
                    BzzMtfRange::new(16, 20, 21, 4),
                    BzzMtfRange::new(32, 36, 37, 5),
                    BzzMtfRange::new(64, 68, 69, 6),
                    BzzMtfRange::new(128, 132, 133, 7),
                ],
            ),
        ];

        for (name, ranges) in variants {
            let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
            let _ = spec_zp_next_block_len(&mut decoder);
            let _ = spec_zp_next_fshift(&mut decoder);
            let mut contexts = [0; BZZ_MTF_CONTEXTS];
            let mut ranks = Vec::with_capacity(expected.len());
            let mut previous_mtfno = 0;
            for _ in 0..expected.len() {
                let rank = read_spec_zp_entropy_rank_with_ranges(
                    &mut decoder,
                    &mut contexts,
                    &tables,
                    previous_mtfno,
                    ranges,
                );
                previous_mtfno = rank;
                ranks.push(rank);
            }
            let matching_prefix = ranks
                .iter()
                .zip(&expected)
                .take_while(|(actual, expected)| actual == expected)
                .count();

            eprintln!(
                "variant={name} first_rank={} expected_first={} matching_prefix={matching_prefix}",
                ranks[0], expected[0]
            );
        }
    }

    #[test]
    fn inverse_bwt_reconstructs_banana_block() {
        let symbols = [b'a', b'n', b'n', b'b', 0, b'a', b'a'];

        let decoded = inverse_bwt_block(&symbols, 4).expect("BWT block should reconstruct");

        assert_eq!(decoded, b"banana");
    }

    #[test]
    fn test_fixture_bwt_derivation_matches_known_banana_block() {
        let block = expected_block_symbols_from_raw(b"banana");

        assert_eq!(
            block,
            BlockSymbols {
                symbols: vec![b'a', b'n', b'n', b'b', 0, b'a', b'a'],
                marker_pos: 4,
            }
        );
    }

    #[test]
    fn inverse_bwt_reconstructs_repeated_bytes() {
        let symbols = [b'a', b'a', b'a', 0];

        let decoded = inverse_bwt_block(&symbols, 3).expect("BWT block should reconstruct");

        assert_eq!(decoded, b"aaa");
    }

    #[test]
    fn inverse_bwt_rejects_invalid_marker_position() {
        let error = inverse_bwt_block(b"abc", 0).expect_err("marker 0 should fail");

        assert!(matches!(error, BzzError::InvalidBwtMarker { .. }));
    }

    #[test]
    fn block_symbols_from_ranks_applies_move_to_front_updates() {
        let block = block_symbols_from_ranks(5, 0, [98, 98, 0, BWT_MARKER_RANK, 0])
            .expect("rank stream should decode");

        assert_eq!(
            block,
            BlockSymbols {
                symbols: vec![b'b', b'a', b'a', 0, b'a'],
                marker_pos: 3,
            }
        );
    }

    #[test]
    fn bzz_move_to_front_keeps_high_frequency_symbols_near_front() {
        let mut table = BzzMoveToFrontTable::new(0);

        assert_eq!(table.take(3).expect("rank 3 should decode"), 3);
        assert_eq!(table.take(3).expect("rank 3 should decode"), 2);
        assert_eq!(table.take(3).expect("rank 3 should decode"), 1);
        assert_eq!(table.take(3).expect("rank 3 should decode"), 0);

        assert_eq!(&table.symbols[..4], &[0, 1, 2, 3]);
        assert!(table.freq.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_missing_marker() {
        let error =
            block_symbols_from_ranks(3, 0, [0, 1, 2]).expect_err("marker should be required");

        assert!(matches!(error, BzzError::MissingBwtMarker));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_duplicate_marker() {
        let error = block_symbols_from_ranks(3, 0, [0, BWT_MARKER_RANK, BWT_MARKER_RANK])
            .expect_err("duplicate marker should fail");

        assert!(matches!(error, BzzError::DuplicateBwtMarker));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_out_of_range_rank() {
        let error = block_symbols_from_ranks(2, 0, [257, BWT_MARKER_RANK])
            .expect_err("bad rank should fail");

        assert!(matches!(error, BzzError::InvalidSymbolRank { rank: 257 }));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_truncated_input() {
        let error =
            block_symbols_from_ranks(2, 0, [BWT_MARKER_RANK]).expect_err("short ranks should fail");

        assert!(matches!(
            error,
            BzzError::TruncatedRankStream { expected_len: 2 }
        ));
    }

    #[test]
    fn decode_block_from_ranks_reconstructs_banana() {
        let decoded = decode_block_from_ranks(7, 0, [97, 110, 0, 99, BWT_MARKER_RANK, 2, 0])
            .expect("rank pipeline should decode");

        assert_eq!(decoded, b"banana");
    }

    #[test]
    fn decode_block_from_ranks_reconstructs_repeated_bytes() {
        let decoded = decode_block_from_ranks(4, 0, [97, 0, 0, BWT_MARKER_RANK])
            .expect("rank pipeline should decode");

        assert_eq!(decoded, b"aaa");
    }

    #[test]
    fn expected_fixture_ranks_reconstruct_hello_raw() {
        let ranks = expected_ranks_from_raw(HELLO_RAW);

        let decoded = decode_block_from_ranks(HELLO_RAW.len() + 1, 0, ranks)
            .expect("rank fixture should decode");

        assert_eq!(decoded, HELLO_RAW);
    }

    #[test]
    fn decode_block_from_ranks_propagates_rank_errors() {
        let error = decode_block_from_ranks(2, 0, [257, BWT_MARKER_RANK])
            .expect_err("bad rank should fail");

        assert!(matches!(error, BzzError::InvalidSymbolRank { rank: 257 }));
    }

    #[test]
    fn collect_block_ranks_reads_exact_block_len() {
        let mut decoder = ScriptedRankDecoder::new([1, 2, BWT_MARKER_RANK]);

        let ranks = collect_block_ranks(3, &mut decoder).expect("ranks should collect");

        assert_eq!(ranks, [1, 2, BWT_MARKER_RANK]);
        assert_eq!(decoder.remaining(), 0);
    }

    #[test]
    fn collect_block_ranks_propagates_decoder_errors() {
        let mut decoder = ScriptedRankDecoder::new([1]);

        let error = collect_block_ranks(2, &mut decoder).expect_err("short decoder should fail");

        assert!(matches!(
            error,
            BzzError::TruncatedRankStream { expected_len: 2 }
        ));
    }

    #[test]
    fn bit_model_reads_from_context_and_updates_toward_zero() {
        let mut model = BitModel::with_contexts(1);
        let mut reader = ScriptedModeledBitReader::new([0]);
        let initial_split = model.tables.split(BIT_MODEL_INITIAL_STATE);

        let bit = model
            .read_bit(&mut reader, 0)
            .expect("modeled bit should read");

        assert_eq!(bit, 0);
        assert_eq!(reader.splits, [initial_split]);
        assert!(model.context(0).state > BIT_MODEL_INITIAL_STATE);
    }

    #[test]
    fn bit_model_reads_from_context_and_updates_toward_one() {
        let mut model = BitModel::with_contexts(1);
        let mut reader = ScriptedModeledBitReader::new([1]);
        let initial_split = model.tables.split(BIT_MODEL_INITIAL_STATE);

        let bit = model
            .read_bit(&mut reader, 0)
            .expect("modeled bit should read");

        assert_eq!(bit, 1);
        assert_eq!(reader.splits, [initial_split]);
        assert!(model.context(0).state < BIT_MODEL_INITIAL_STATE);
    }

    #[test]
    fn bit_model_keeps_contexts_independent() {
        let mut model = BitModel::with_contexts(2);
        let mut reader = ScriptedModeledBitReader::new([0]);

        model
            .read_bit(&mut reader, 1)
            .expect("modeled bit should read");

        assert_eq!(model.context(0).state, BIT_MODEL_INITIAL_STATE);
        assert_ne!(model.context(1).state, BIT_MODEL_INITIAL_STATE);
    }

    #[test]
    fn bit_model_tables_are_generated_monotonically() {
        let tables = BitModelTables::generated();

        assert_eq!(tables.split(0), 1704);
        assert_eq!(tables.split(BIT_MODEL_INITIAL_STATE), 44_768);
        assert_eq!(tables.split(64), u16::MAX);
        assert!(tables.split(1) > tables.split(0));
        assert!(tables.next_state(BIT_MODEL_INITIAL_STATE, 0) > BIT_MODEL_INITIAL_STATE);
        assert!(tables.next_state(BIT_MODEL_INITIAL_STATE, 1) < BIT_MODEL_INITIAL_STATE);
    }

    #[test]
    fn read_entropy_rank_decodes_rank_zero() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([1]);

        let rank = read_entropy_rank(&mut model, &mut reader, 0).expect("rank should decode");

        assert_eq!(rank, 0);
        assert_eq!(reader.bits_read(), 1);
    }

    #[test]
    fn read_entropy_rank_decodes_rank_one() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 1]);

        let rank = read_entropy_rank(&mut model, &mut reader, 0).expect("rank should decode");

        assert_eq!(rank, 1);
        assert_eq!(reader.bits_read(), 2);
    }

    #[test]
    fn read_entropy_rank_decodes_large_rank() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 0, 0, 1, 1, 1]);

        let rank = read_entropy_rank(&mut model, &mut reader, 0).expect("rank should decode");

        assert_eq!(rank, 7);
        assert_eq!(reader.bits_read(), 6);
    }

    #[test]
    fn read_entropy_rank_decodes_marker_rank() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 0, 0, 0, 0, 0, 0, 0, 0]);

        let rank = read_entropy_rank(&mut model, &mut reader, 0).expect("rank should decode");

        assert_eq!(rank, BWT_MARKER_RANK);
        assert_eq!(reader.bits_read(), 9);
    }

    #[test]
    fn entropy_decisions_for_rank_encode_rank_tree_paths() {
        assert_eq!(entropy_decisions_for_rank(0, 0), vec![(0, 1)]);
        assert_eq!(entropy_decisions_for_rank(1, 0), vec![(0, 0), (3, 1)]);
        assert_eq!(
            entropy_decisions_for_rank(7, 0),
            vec![(0, 0), (3, 0), (6, 0), (8, 1), (9, 1), (11, 1),]
        );
        assert_eq!(
            entropy_decisions_for_rank(BWT_MARKER_RANK, 0),
            vec![
                (0, 0),
                (3, 0),
                (6, 0),
                (8, 0),
                (12, 0),
                (20, 0),
                (36, 0),
                (68, 0),
                (132, 0),
            ]
        );
    }

    #[test]
    fn bzz_mtfno_context_path_uses_spec_layout() {
        let path = bzz_mtfno_context_path(10, 0);

        assert_eq!(
            path,
            vec![
                (0, 0),
                (3, 0),
                (6, 0),
                (8, 0),
                (12, 1),
                (13, 0),
                (14, 1),
                (17, 0),
            ]
        );
        assert!(
            !path.iter().any(|(context_id, _)| *context_id == 9),
            "context 9 belongs to the rank 4..7 decode_bin tree, not rank 10"
        );
    }

    #[test]
    fn bzz_decode_bin_uses_tree_contexts() {
        let path = bzz_decode_bin_context_path(3, 13, 0b010);

        assert_eq!(path, vec![(13, 0), (14, 1), (17, 0)]);
    }

    #[test]
    #[ignore = "tracks convergence of provisional entropy model against the BZZ fixture"]
    fn fixture_rank_stream_matches_expected_ranks() {
        let expected = expected_ranks_from_raw(HELLO_RAW);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let block_len = decoder.next_block_len();
        let fshift = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let actual = decoder
            .decode_block_ranks(block_len, fshift)
            .expect("fixture ranks should decode");

        assert_eq!(actual, expected);
    }

    #[test]
    #[ignore = "tracks provisional bit model convergence against expected fixture decisions"]
    fn fixture_modeled_bits_match_expected_rank_decisions() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let trace = trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
            .expect("modeled fixture bits should decode");

        assert_modeled_bit_trace_matches(&trace);
    }

    #[test]
    #[ignore = "pinpoints the first fixture entropy decision mismatch"]
    fn fixture_first_entropy_mismatch_identifies_rank_path() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_trace = expected_decision_trace_from_ranks(&expected_ranks);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let bit_trace =
            trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
                .expect("modeled fixture bits should decode");
        let mismatch = first_modeled_bit_mismatch(&bit_trace)
            .expect("fixture should still expose the provisional mismatch");
        let expected = &expected_trace[mismatch.index];

        assert_eq!(mismatch.index, 6);
        assert_eq!(mismatch.context_id, 9);
        assert_eq!(mismatch.state_before, BIT_MODEL_INITIAL_STATE);
        assert_eq!(mismatch.expected_bit, 1);
        assert_eq!(mismatch.actual_bit, 0);
        assert_eq!(expected.rank_index, 0);
        assert_eq!(expected.rank, 10);
        assert_eq!(expected.rank_decision_index, 6);

        let actual_first_rank_decisions = bit_trace
            .iter()
            .take_while(|entry| entry.index <= mismatch.index)
            .map(|entry| (entry.context_id, entry.actual_bit))
            .collect::<Vec<_>>();

        assert_eq!(
            actual_first_rank_decisions,
            vec![(0, 0), (3, 0), (6, 0), (8, 0), (12, 1), (13, 0), (14, 1),]
        );
        eprintln!("first mismatch expected decision: {expected:?}");
    }

    #[test]
    #[ignore = "checks whether simple rank-tree variants explain the fixture mismatch"]
    fn fixture_rank_tree_variants_do_not_explain_first_mismatch() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let variants = [
            RankDecisionOptions {
                group_selected_bit: 1,
                offset_msb_first: true,
                invert_offset_bits: false,
            },
            RankDecisionOptions {
                group_selected_bit: 0,
                offset_msb_first: true,
                invert_offset_bits: false,
            },
            RankDecisionOptions {
                group_selected_bit: 1,
                offset_msb_first: false,
                invert_offset_bits: false,
            },
            RankDecisionOptions {
                group_selected_bit: 1,
                offset_msb_first: true,
                invert_offset_bits: true,
            },
            RankDecisionOptions {
                group_selected_bit: 0,
                offset_msb_first: false,
                invert_offset_bits: false,
            },
            RankDecisionOptions {
                group_selected_bit: 0,
                offset_msb_first: true,
                invert_offset_bits: true,
            },
            RankDecisionOptions {
                group_selected_bit: 1,
                offset_msb_first: false,
                invert_offset_bits: true,
            },
            RankDecisionOptions {
                group_selected_bit: 0,
                offset_msb_first: false,
                invert_offset_bits: true,
            },
        ];
        let mut scored = variants
            .into_iter()
            .map(|options| {
                let decisions = entropy_decisions_from_ranks_with_options(&expected_ranks, options);
                let mut decoder = BzzDecoder::new(HELLO_BZZ);
                let _ = decoder.next_block_len();
                let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
                let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
                let trace = trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &decisions)
                    .expect("modeled fixture bits should decode");
                let matched = first_modeled_bit_mismatch(&trace)
                    .map_or(trace.len(), |mismatch| mismatch.index);
                (options, matched)
            })
            .collect::<Vec<_>>();

        scored.sort_by_key(|(_, matched)| *matched);
        let best = scored
            .last()
            .expect("at least one rank-tree variant should be scored");

        eprintln!("rank-tree variant scores: {scored:?}");
        assert_eq!(best.0.group_selected_bit, 1);
        assert_eq!(best.1, 6);
    }

    #[test]
    #[ignore = "checks the initial split range needed by the first rank selector"]
    fn fixture_first_rank_selector_initial_split_constraints_are_satisfiable() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let trace = trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
            .expect("modeled fixture bits should decode");
        let first_rank_initial_state = trace
            .iter()
            .take(5)
            .map(|entry| {
                SplitConstraint::for_decision(
                    entry.index,
                    SplitConstraintKey {
                        context_id: entry.context_id,
                        state: entry.state_before,
                    },
                    entry.probe,
                    entry.expected_bit,
                )
            })
            .collect::<Vec<_>>();
        let mut merged = SplitConstraint {
            key: SplitConstraintKey {
                context_id: usize::MAX,
                state: BIT_MODEL_INITIAL_STATE,
            },
            min_split: 0,
            max_split: u16::MAX,
            min_decision: 0,
            max_decision: 0,
        };

        for constraint in &first_rank_initial_state {
            assert_eq!(constraint.key.state, BIT_MODEL_INITIAL_STATE);
            merged.merge(*constraint);
        }

        let generated_split = BitModelTables::generated().split(BIT_MODEL_INITIAL_STATE);

        assert!(merged.is_satisfiable());
        assert!(generated_split >= merged.min_split);
        assert!(generated_split <= merged.max_split);
        eprintln!("first-rank initial-state constraints: {first_rank_initial_state:?}");
        eprintln!("merged initial-state constraint: {merged:?}");
    }

    #[test]
    #[ignore = "shows that the first full rank needs context-specific initial state"]
    fn fixture_first_rank_fresh_context_constraints_conflict() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let trace = trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
            .expect("modeled fixture bits should decode");
        let first_rank = &trace[..entropy_decisions_for_rank(expected_ranks[0], 0).len()];
        let mut merged = SplitConstraint {
            key: SplitConstraintKey {
                context_id: usize::MAX,
                state: BIT_MODEL_INITIAL_STATE,
            },
            min_split: 0,
            max_split: u16::MAX,
            min_decision: 0,
            max_decision: 0,
        };

        for entry in first_rank {
            assert_eq!(entry.state_before, BIT_MODEL_INITIAL_STATE);
            merged.merge(SplitConstraint::for_decision(
                entry.index,
                SplitConstraintKey {
                    context_id: entry.context_id,
                    state: entry.state_before,
                },
                entry.probe,
                entry.expected_bit,
            ));
        }

        assert!(!merged.is_satisfiable());
        assert_eq!(merged.min_split, 63_600);
        assert_eq!(merged.min_decision, 6);
        assert_eq!(merged.max_split, 54_909);
        assert_eq!(merged.max_decision, 3);
        eprintln!("first-rank fresh-context merged constraint: {merged:?}");
    }

    #[test]
    #[ignore = "maps first-rank split constraints back to possible initial states"]
    fn fixture_first_rank_context_initial_state_ranges() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let trace = trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
            .expect("modeled fixture bits should decode");
        let first_rank = &trace[..entropy_decisions_for_rank(expected_ranks[0], 0).len()];
        let state_ranges = first_rank
            .iter()
            .map(|entry| {
                let constraint = SplitConstraint::for_decision(
                    entry.index,
                    SplitConstraintKey {
                        context_id: entry.context_id,
                        state: entry.state_before,
                    },
                    entry.probe,
                    entry.expected_bit,
                );
                let states = states_satisfying_split_constraint(constraint);
                (entry.context_id, entry.expected_bit, constraint, states)
            })
            .collect::<Vec<_>>();

        let context_9 = state_ranges
            .iter()
            .find(|(context_id, _, _, _)| *context_id == 9)
            .expect("first rank should use context 9");
        let context_9_states = &context_9.3;

        assert_eq!(context_9_states.first(), Some(&59));
        assert_eq!(context_9_states.last(), Some(&64));
        eprintln!("first-rank initial state ranges: {state_ranges:?}");
    }

    #[test]
    #[ignore = "tests whether a context-9 initial-state adjustment moves the fixture mismatch"]
    fn fixture_context_9_initial_state_adjustment_moves_mismatch() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_trace = expected_decision_trace_from_ranks(&expected_ranks);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        model.context_mut(9).state = 59;
        let bit_trace =
            trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
                .expect("modeled fixture bits should decode");
        let mismatch = first_modeled_bit_mismatch(&bit_trace)
            .expect("fixture should still expose the provisional mismatch");
        let expected = &expected_trace[mismatch.index];

        assert!(mismatch.index > 6);
        eprintln!("context-9 adjusted first mismatch: {mismatch:?}");
        eprintln!("context-9 adjusted expected decision: {expected:?}");
    }

    #[test]
    #[ignore = "tests whether discovered high-probability context initial states advance the fixture"]
    fn fixture_selected_context_initial_state_adjustments_move_mismatch() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_trace = expected_decision_trace_from_ranks(&expected_ranks);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        model.context_mut(9).state = 59;
        model.context_mut(16).state = 59;
        let bit_trace =
            trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
                .expect("modeled fixture bits should decode");
        let mismatch = first_modeled_bit_mismatch(&bit_trace)
            .expect("fixture should still expose the provisional mismatch");
        let expected = &expected_trace[mismatch.index];

        assert!(mismatch.index > 14);
        eprintln!("selected-context adjusted first mismatch: {mismatch:?}");
        eprintln!("selected-context adjusted expected decision: {expected:?}");
    }

    #[test]
    #[ignore = "scores simple entropy context initializers against the fixture"]
    fn fixture_entropy_context_initializer_variants() {
        let variants = [
            EntropyInitializerVariant::Uniform,
            EntropyInitializerVariant::HighDiscovered,
            EntropyInitializerVariant::HighDiscoveredLowRankConservative,
            EntropyInitializerVariant::HighDiscoveredLowRankVeryConservative,
            EntropyInitializerVariant::HighDiscoveredEarlySelectorsVeryConservative,
            EntropyInitializerVariant::RankTreeSelectedHigh,
            EntropyInitializerVariant::RankTreeSelectedHighLowRankVeryConservative,
        ];
        let mut scored = variants
            .into_iter()
            .map(|variant| {
                let mismatch = first_fixture_mismatch_with_initializer(|model| {
                    apply_entropy_initializer_variant(model, variant);
                });
                (variant, mismatch.index)
            })
            .collect::<Vec<_>>();

        scored.sort_by_key(|(_, mismatch_index)| *mismatch_index);
        let best = scored
            .last()
            .expect("at least one initializer variant should be scored");

        eprintln!("entropy initializer variant scores: {scored:?}");
        assert_eq!(
            best.0,
            EntropyInitializerVariant::HighDiscoveredLowRankVeryConservative
        );
        assert_eq!(best.1, 22);
    }

    #[test]
    #[ignore = "prints the next mismatch after the best simple initializer"]
    fn fixture_best_simple_initializer_identifies_next_mismatch() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_trace = expected_decision_trace_from_ranks(&expected_ranks);
        let mismatch = first_fixture_mismatch_with_initializer(|model| {
            apply_entropy_initializer_variant(
                model,
                EntropyInitializerVariant::HighDiscoveredLowRankVeryConservative,
            );
        });
        let expected = &expected_trace[mismatch.index];

        assert_eq!(mismatch.index, 22);
        eprintln!("best simple initializer first mismatch: {mismatch:?}");
        eprintln!("best simple initializer expected decision: {expected:?}");
    }

    #[test]
    #[ignore = "calibrates provisional bit-model splits against expected fixture decisions"]
    fn fixture_expected_decisions_have_consistent_split_constraints() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let search = split_constraints_for_expected_decisions(
            &mut model,
            &mut decoder.bits,
            &expected_decisions,
        )
        .expect("fixture split constraints should compute");
        let conflict = search
            .conflict
            .expect("actual-path split calibration should expose its first conflict");

        assert!(
            !search.constraints.is_empty(),
            "fixture should produce split constraints"
        );
        eprintln!("actual-path split conflict: {conflict:?}");
        assert_eq!(
            conflict.key,
            SplitConstraintKey {
                context_id: 16,
                state: BIT_MODEL_INITIAL_STATE,
            }
        );
        assert_eq!(conflict.decision, 68);
        assert_eq!(conflict.min_split, 40_363);
        assert_eq!(conflict.min_decision, 14);
        assert_eq!(conflict.max_split, 10_512);
        assert_eq!(conflict.max_decision, 68);
    }

    #[test]
    #[ignore = "prints expected rank-decision context around calibration conflicts"]
    fn fixture_expected_decision_trace_identifies_context_conflicts() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let trace = expected_decision_trace_from_ranks(&expected_ranks);

        let first = trace
            .iter()
            .find(|entry| entry.decision_index == 28)
            .expect("decision 28 should exist");
        let second = trace
            .iter()
            .find(|entry| entry.decision_index == 56)
            .expect("decision 56 should exist");

        assert_eq!(first.context_id, 23);
        assert_eq!(first.rank_index, 2);
        assert_eq!(first.rank, 115);
        assert_eq!(first.rank_decision_index, 8);
        assert_eq!(first.expected_bit, 1);
        assert_eq!(second.context_id, 23);
        assert_eq!(second.rank_index, 4);
        assert_eq!(second.rank, 92);
        assert_eq!(second.rank_decision_index, 8);
        assert_eq!(second.expected_bit, 0);
        eprintln!("decision 28: {first:?}");
        eprintln!("decision 56: {second:?}");
    }

    #[test]
    #[ignore = "prints context state evolution for conflicting expected decisions"]
    fn fixture_context_23_state_trace_under_expected_bits() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let state_trace = expected_state_trace_for_context(&expected_decisions, 23);

        let first = state_trace
            .iter()
            .find(|entry| entry.decision_index == 28)
            .expect("decision 28 should be in context 23 trace");
        let second = state_trace
            .iter()
            .find(|entry| entry.decision_index == 56)
            .expect("decision 56 should be in context 23 trace");

        assert_eq!(first.expected_bit, 1);
        assert_eq!(first.state_before, BIT_MODEL_INITIAL_STATE);
        assert_eq!(
            first.split_before,
            BitModelTables::generated().split(first.state_before)
        );
        assert_eq!(
            first.state_after,
            BitModelTables::generated().next_state(first.state_before, first.expected_bit)
        );
        assert_eq!(second.expected_bit, 0);
        assert_eq!(
            second.split_before,
            BitModelTables::generated().split(second.state_before)
        );
        assert_eq!(
            second.state_after,
            BitModelTables::generated().next_state(second.state_before, second.expected_bit)
        );
        eprintln!("context 23 state trace: {state_trace:?}");
    }

    fn expected_ranks_from_raw(raw: &[u8]) -> Vec<usize> {
        expected_ranks_from_raw_with_fshift(raw, 0)
    }

    fn expected_ranks_from_raw_with_fshift(raw: &[u8], fshift: u8) -> Vec<usize> {
        let block = expected_block_symbols_from_raw(raw);
        ranks_from_block_symbols(&block, fshift).expect("derived BWT block should convert to ranks")
    }

    fn spec_zp_next_block_len(decoder: &mut SpecZpDecoder<'_>) -> usize {
        let mut value = 1usize;
        let block_len_marker = 1usize << 24;

        while value < block_len_marker {
            value = (value << 1) | usize::from(decoder.decode_raw());
        }

        value - block_len_marker
    }

    fn spec_zp_next_fshift(decoder: &mut SpecZpDecoder<'_>) -> u8 {
        (decoder.decode_raw() << 1) | decoder.decode_raw()
    }

    fn read_spec_zp_entropy_rank(
        decoder: &mut SpecZpDecoder<'_>,
        contexts: &mut [u8; BZZ_MTF_CONTEXTS],
        tables: &ZpTableSet,
        previous_mtfno: usize,
    ) -> usize {
        read_spec_zp_entropy_rank_with_ranges(
            decoder,
            contexts,
            tables,
            previous_mtfno,
            &BZZ_MTF_RANGES,
        )
    }

    fn read_spec_zp_entropy_rank_with_ranges(
        decoder: &mut SpecZpDecoder<'_>,
        contexts: &mut [u8; BZZ_MTF_CONTEXTS],
        tables: &ZpTableSet,
        previous_mtfno: usize,
        ranges: &[BzzMtfRange],
    ) -> usize {
        let previous_class = previous_mtfno.min(2);
        if decoder.decode_bit(
            &mut contexts[RANK_ZERO_CONTEXT_START + previous_class],
            tables,
        ) == 1
        {
            return 0;
        }
        if decoder.decode_bit(
            &mut contexts[RANK_ONE_CONTEXT_START + previous_class],
            tables,
        ) == 1
        {
            return 1;
        }

        for range in ranges {
            if decoder.decode_bit(&mut contexts[range.selector_context], tables) == 1 {
                let offset = read_spec_zp_mtfno_bin(
                    decoder,
                    contexts,
                    tables,
                    range.bits,
                    range.tree_context,
                );
                return range.start + offset;
            }
        }

        BWT_MARKER_RANK
    }

    fn read_spec_zp_mtfno_bin(
        decoder: &mut SpecZpDecoder<'_>,
        contexts: &mut [u8; BZZ_MTF_CONTEXTS],
        tables: &ZpTableSet,
        bits: usize,
        context_start: usize,
    ) -> usize {
        let mut value = 0;
        let mut node = 0;

        for _ in 0..bits {
            let bit = decoder.decode_bit(&mut contexts[context_start + node], tables);
            value = (value << 1) | usize::from(bit);
            node = (node << 1) + 1 + usize::from(bit);
        }

        value
    }

    fn entropy_decisions_from_ranks(ranks: &[usize]) -> Vec<(usize, u8)> {
        entropy_decisions_from_ranks_with_options(ranks, RankDecisionOptions::default())
    }

    fn bzz_mtfno_context_path(rank: usize, previous_mtfno: usize) -> Vec<(usize, u8)> {
        bzz_mtfno_context_path_with_options(rank, previous_mtfno, RankDecisionOptions::default())
    }

    fn bzz_mtfno_context_path_with_options(
        rank: usize,
        previous_mtfno: usize,
        options: RankDecisionOptions,
    ) -> Vec<(usize, u8)> {
        let mut path = Vec::new();
        let previous_class = previous_mtfno.min(2);

        path.push((previous_class, u8::from(rank == 0)));
        if rank == 0 {
            return path;
        }

        path.push((3 + previous_class, u8::from(rank == 1)));
        if rank == 1 {
            return path;
        }

        for range in BZZ_MTF_RANGES {
            let range_end = range.start + (1usize << range.bits) - 1;
            let selected = rank >= range.start && rank <= range_end;
            let selected_bit = if selected {
                options.group_selected_bit
            } else {
                1 - options.group_selected_bit
            };
            path.push((range.selector_context, selected_bit));
            if selected {
                path.extend(bzz_decode_bin_context_path_with_options(
                    range.bits,
                    range.tree_context,
                    rank - range.start,
                    options,
                ));
                return path;
            }
        }

        path
    }

    fn bzz_decode_bin_context_path(
        bits: usize,
        context_start: usize,
        value: usize,
    ) -> Vec<(usize, u8)> {
        bzz_decode_bin_context_path_with_options(
            bits,
            context_start,
            value,
            RankDecisionOptions::default(),
        )
    }

    fn bzz_decode_bin_context_path_with_options(
        bits: usize,
        context_start: usize,
        value: usize,
        options: RankDecisionOptions,
    ) -> Vec<(usize, u8)> {
        let mut path = Vec::with_capacity(bits);
        let mut node = 0usize;
        let bit_indexes = if options.offset_msb_first {
            (0..bits).rev().collect::<Vec<_>>()
        } else {
            (0..bits).collect::<Vec<_>>()
        };

        for bit_index in bit_indexes {
            let mut bit = u8::from((value & (1usize << bit_index)) != 0);
            if options.invert_offset_bits {
                bit = 1 - bit;
            }
            path.push((context_start + node, bit));
            node = (node << 1) + 1 + usize::from(bit);
        }

        path
    }

    fn entropy_decisions_from_ranks_with_options(
        ranks: &[usize],
        options: RankDecisionOptions,
    ) -> Vec<(usize, u8)> {
        let mut decisions = Vec::new();
        let mut previous_mtfno = 0usize;
        for rank in ranks {
            decisions.extend(entropy_decisions_for_rank_with_options(
                *rank,
                previous_mtfno,
                options,
            ));
            previous_mtfno = *rank;
        }
        decisions
    }

    fn expected_decision_trace_from_ranks(ranks: &[usize]) -> Vec<ExpectedDecisionTrace> {
        let mut trace = Vec::new();
        for (rank_index, rank) in ranks.iter().copied().enumerate() {
            for (rank_decision_index, (context_id, expected_bit)) in entropy_decisions_for_rank(
                rank,
                if rank_index == 0 {
                    0
                } else {
                    ranks[rank_index - 1]
                },
            )
            .into_iter()
            .enumerate()
            {
                trace.push(ExpectedDecisionTrace {
                    decision_index: trace.len(),
                    rank_index,
                    rank,
                    rank_decision_index,
                    context_id,
                    expected_bit,
                });
            }
        }
        trace
    }

    fn expected_state_trace_for_context(
        expected_decisions: &[(usize, u8)],
        target_context: usize,
    ) -> Vec<ExpectedStateTrace> {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut trace = Vec::new();

        for (decision_index, (context_id, expected_bit)) in
            expected_decisions.iter().copied().enumerate()
        {
            let state_before = model.context(context_id).state;
            let split_before = model.tables.split(state_before);
            model.contexts[context_id].observe(expected_bit, &model.tables);
            let state_after = model.context(context_id).state;

            if context_id == target_context {
                trace.push(ExpectedStateTrace {
                    decision_index,
                    expected_bit,
                    state_before,
                    split_before,
                    state_after,
                });
            }
        }

        trace
    }

    fn entropy_decisions_for_rank(rank: usize, previous_mtfno: usize) -> Vec<(usize, u8)> {
        entropy_decisions_for_rank_with_options(
            rank,
            previous_mtfno,
            RankDecisionOptions::default(),
        )
    }

    fn entropy_decisions_for_rank_with_options(
        rank: usize,
        previous_mtfno: usize,
        options: RankDecisionOptions,
    ) -> Vec<(usize, u8)> {
        let mut decisions = bzz_mtfno_context_path(rank, previous_mtfno);
        if options.group_selected_bit == 0 {
            for (context_id, bit) in &mut decisions {
                if BZZ_MTF_RANGES
                    .iter()
                    .any(|range| range.selector_context == *context_id)
                {
                    *bit = 1 - *bit;
                }
            }
        }
        if !options.offset_msb_first || options.invert_offset_bits {
            decisions = bzz_mtfno_context_path_with_options(rank, previous_mtfno, options);
        }
        decisions
    }

    fn trace_fixture_modeled_bits(
        model: &mut BitModel,
        reader: &mut ZpBitReader<'_>,
        expected_decisions: &[(usize, u8)],
    ) -> BzzResult<Vec<ModeledBitTrace>> {
        let mut trace = Vec::with_capacity(expected_decisions.len());

        for (index, (context_id, expected_bit)) in expected_decisions.iter().copied().enumerate() {
            let state_before = model.context(context_id).state;
            let split_before = model.tables.split(state_before);
            let probe = reader.decision_probe();
            let actual_bit = model.read_bit(reader, context_id)?;
            let state_after = model.context(context_id).state;
            trace.push(ModeledBitTrace {
                index,
                context_id,
                state_before,
                split_before,
                expected_bit,
                actual_bit,
                state_after,
                probe,
            });
        }

        Ok(trace)
    }

    fn split_constraints_for_expected_decisions(
        model: &mut BitModel,
        reader: &mut ZpBitReader<'_>,
        expected_decisions: &[(usize, u8)],
    ) -> BzzResult<SplitConstraintSearch> {
        let mut constraints: Vec<SplitConstraint> = Vec::new();

        for (index, (context_id, expected_bit)) in expected_decisions.iter().copied().enumerate() {
            let state_before = model.context(context_id).state;
            let probe = reader.decision_probe();
            let key = SplitConstraintKey {
                context_id,
                state: state_before,
            };
            let constraint = SplitConstraint::for_decision(index, key, probe, expected_bit);

            if let Some(existing_index) =
                constraints.iter().position(|existing| existing.key == key)
            {
                constraints[existing_index].merge(constraint);
                let existing = constraints[existing_index];
                if !existing.is_satisfiable() {
                    return Ok(SplitConstraintSearch {
                        constraints,
                        conflict: Some(SplitConstraintConflict {
                            key,
                            decision: index,
                            min_split: existing.min_split,
                            max_split: existing.max_split,
                            min_decision: existing.min_decision,
                            max_decision: existing.max_decision,
                        }),
                    });
                }
            } else {
                constraints.push(constraint);
            }

            let _ = model.read_bit(reader, context_id)?;
        }

        Ok(SplitConstraintSearch {
            constraints,
            conflict: None,
        })
    }

    fn assert_modeled_bit_trace_matches(trace: &[ModeledBitTrace]) {
        for entry in trace {
            assert_eq!(
                entry.actual_bit,
                entry.expected_bit,
                "modeled bit mismatch at decision {}, context {}, state_before {}, split {}, state_after {}, code_window {}, lower_bound {}, cursor {}, buffered_bits {}, split_needed_for_one {}",
                entry.index,
                entry.context_id,
                entry.state_before,
                entry.split_before,
                entry.state_after,
                entry.probe.code_window,
                entry.probe.lower_bound,
                entry.probe.cursor,
                entry.probe.buffered_bits,
                entry.probe.code_window.saturating_add(1),
            );
        }
    }

    fn first_modeled_bit_mismatch(trace: &[ModeledBitTrace]) -> Option<&ModeledBitTrace> {
        trace
            .iter()
            .find(|entry| entry.actual_bit != entry.expected_bit)
    }

    fn first_fixture_mismatch_with_initializer(
        initialize: impl FnOnce(&mut BitModel),
    ) -> ModeledBitTrace {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
        let _ = decoder.next_fshift().expect("fixture FSHIFT should decode");
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        initialize(&mut model);
        let bit_trace =
            trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
                .expect("modeled fixture bits should decode");

        first_modeled_bit_mismatch(&bit_trace)
            .expect("fixture should still expose the provisional mismatch")
            .clone()
    }

    fn apply_entropy_initializer_variant(model: &mut BitModel, variant: EntropyInitializerVariant) {
        match variant {
            EntropyInitializerVariant::Uniform => {}
            EntropyInitializerVariant::HighDiscovered => {
                model.context_mut(9).state = 59;
                model.context_mut(16).state = 59;
            }
            EntropyInitializerVariant::HighDiscoveredLowRankConservative => {
                model.context_mut(RANK_ZERO_CONTEXT_START).state = 24;
                model.context_mut(RANK_ONE_CONTEXT_START).state = 24;
                model.context_mut(9).state = 59;
                model.context_mut(16).state = 59;
            }
            EntropyInitializerVariant::HighDiscoveredLowRankVeryConservative => {
                model.context_mut(RANK_ZERO_CONTEXT_START).state = 18;
                model.context_mut(RANK_ONE_CONTEXT_START).state = 18;
                model.context_mut(9).state = 59;
                model.context_mut(16).state = 59;
            }
            EntropyInitializerVariant::HighDiscoveredEarlySelectorsVeryConservative => {
                for context_id in [RANK_ZERO_CONTEXT_START, RANK_ONE_CONTEXT_START, 6, 8] {
                    model.context_mut(context_id).state = 18;
                }
                model.context_mut(9).state = 59;
                model.context_mut(16).state = 59;
            }
            EntropyInitializerVariant::RankTreeSelectedHigh => {
                for context_id in [7, 9, 16, 23] {
                    model.context_mut(context_id).state = 59;
                }
            }
            EntropyInitializerVariant::RankTreeSelectedHighLowRankVeryConservative => {
                model.context_mut(RANK_ZERO_CONTEXT_START).state = 18;
                model.context_mut(RANK_ONE_CONTEXT_START).state = 18;
                for context_id in [7, 9, 16, 23] {
                    model.context_mut(context_id).state = 59;
                }
            }
        }
    }

    fn states_satisfying_split_constraint(constraint: SplitConstraint) -> Vec<u8> {
        let tables = BitModelTables::generated();
        (0..BIT_MODEL_STATES)
            .filter_map(|state| {
                let state = u8::try_from(state).expect("bit-model state should fit in u8");
                let split = tables.split(state);
                (split >= constraint.min_split && split <= constraint.max_split).then_some(state)
            })
            .collect()
    }

    #[derive(Debug, Clone)]
    struct ModeledBitTrace {
        index: usize,
        context_id: usize,
        state_before: u8,
        split_before: u16,
        expected_bit: u8,
        actual_bit: u8,
        state_after: u8,
        probe: DecisionProbe,
    }

    #[derive(Debug)]
    struct ExpectedDecisionTrace {
        decision_index: usize,
        rank_index: usize,
        rank: usize,
        rank_decision_index: usize,
        context_id: usize,
        expected_bit: u8,
    }

    #[derive(Debug)]
    struct ExpectedStateTrace {
        decision_index: usize,
        expected_bit: u8,
        state_before: u8,
        split_before: u16,
        state_after: u8,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct RankDecisionOptions {
        group_selected_bit: u8,
        offset_msb_first: bool,
        invert_offset_bits: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EntropyInitializerVariant {
        Uniform,
        HighDiscovered,
        HighDiscoveredLowRankConservative,
        HighDiscoveredLowRankVeryConservative,
        HighDiscoveredEarlySelectorsVeryConservative,
        RankTreeSelectedHigh,
        RankTreeSelectedHighLowRankVeryConservative,
    }

    impl Default for RankDecisionOptions {
        fn default() -> Self {
            Self {
                group_selected_bit: 1,
                offset_msb_first: true,
                invert_offset_bits: false,
            }
        }
    }

    struct SplitConstraintSearch {
        constraints: Vec<SplitConstraint>,
        conflict: Option<SplitConstraintConflict>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SplitConstraintKey {
        context_id: usize,
        state: u8,
    }

    #[derive(Debug)]
    struct SplitConstraintConflict {
        key: SplitConstraintKey,
        decision: usize,
        min_split: u16,
        max_split: u16,
        min_decision: usize,
        max_decision: usize,
    }

    #[derive(Debug, Clone, Copy)]
    struct SplitConstraint {
        key: SplitConstraintKey,
        min_split: u16,
        max_split: u16,
        min_decision: usize,
        max_decision: usize,
    }

    impl SplitConstraint {
        fn for_decision(
            index: usize,
            key: SplitConstraintKey,
            probe: DecisionProbe,
            expected_bit: u8,
        ) -> Self {
            if expected_bit == 1 {
                Self {
                    key,
                    min_split: probe.code_window.saturating_add(1),
                    max_split: u16::MAX,
                    min_decision: index,
                    max_decision: index,
                }
            } else {
                Self {
                    key,
                    min_split: 0,
                    max_split: probe.code_window,
                    min_decision: index,
                    max_decision: index,
                }
            }
        }

        fn merge(&mut self, other: Self) {
            if other.min_split > self.min_split {
                self.min_split = other.min_split;
                self.min_decision = other.min_decision;
            }
            if other.max_split < self.max_split {
                self.max_split = other.max_split;
                self.max_decision = other.max_decision;
            }
        }

        const fn is_satisfiable(self) -> bool {
            self.min_split <= self.max_split
        }
    }

    fn expected_block_symbols_from_raw(raw: &[u8]) -> BlockSymbols {
        let block_len = raw.len() + 1;
        let mut rotations = (0..block_len).collect::<Vec<_>>();
        rotations.sort_by(|&lhs, &rhs| {
            (0..block_len)
                .map(|offset| bwt_symbol_key(raw, (lhs + offset) % block_len))
                .cmp((0..block_len).map(|offset| bwt_symbol_key(raw, (rhs + offset) % block_len)))
        });

        let mut symbols = Vec::with_capacity(block_len);
        let mut marker_pos = None;
        for (output_index, start) in rotations.into_iter().enumerate() {
            let previous = (start + block_len - 1) % block_len;
            if let Some(symbol) = bwt_symbol(raw, previous) {
                symbols.push(symbol);
            } else {
                symbols.push(0);
                marker_pos = Some(output_index);
            }
        }

        BlockSymbols {
            symbols,
            marker_pos: marker_pos.expect("BWT output should contain the marker"),
        }
    }

    fn ranks_from_block_symbols(block: &BlockSymbols, fshift: u8) -> BzzResult<Vec<usize>> {
        let mut table = BzzMoveToFrontTable::new(fshift);
        let mut ranks = Vec::with_capacity(block.symbols.len());

        for (index, symbol) in block.symbols.iter().copied().enumerate() {
            if index == block.marker_pos {
                ranks.push(BWT_MARKER_RANK);
                continue;
            }

            let rank = table
                .rank_of(symbol)
                .expect("byte should exist in MTF table");
            ranks.push(rank);
            let _ = table.take(rank)?;
        }

        Ok(ranks)
    }

    fn bwt_symbol(raw: &[u8], index: usize) -> Option<u8> {
        raw.get(index).copied()
    }

    fn bwt_symbol_key(raw: &[u8], index: usize) -> i16 {
        bwt_symbol(raw, index).map_or(-1, i16::from)
    }

    struct ScriptedRankDecoder {
        ranks: std::vec::IntoIter<usize>,
        original_len: usize,
    }

    impl ScriptedRankDecoder {
        fn new<I>(ranks: I) -> Self
        where
            I: IntoIterator<Item = usize>,
        {
            let ranks = ranks.into_iter().collect::<Vec<_>>();
            let original_len = ranks.len();
            Self {
                ranks: ranks.into_iter(),
                original_len,
            }
        }

        fn remaining(&self) -> usize {
            self.ranks.len()
        }
    }

    impl RankDecoder for ScriptedRankDecoder {
        fn next_rank(&mut self) -> BzzResult<usize> {
            self.ranks.next().ok_or(BzzError::TruncatedRankStream {
                expected_len: self.original_len + 1,
            })
        }
    }

    struct ScriptedModeledBitReader {
        bits: std::vec::IntoIter<u8>,
        splits: Vec<u16>,
        bits_read: usize,
    }

    impl ScriptedModeledBitReader {
        fn new<I>(bits: I) -> Self
        where
            I: IntoIterator<Item = u8>,
        {
            Self {
                bits: bits.into_iter().collect::<Vec<_>>().into_iter(),
                splits: Vec::new(),
                bits_read: 0,
            }
        }

        fn bits_read(&self) -> usize {
            self.bits_read
        }
    }

    impl ModeledBitReader for ScriptedModeledBitReader {
        fn read_modeled_bit(&mut self, split: u16) -> BzzResult<u8> {
            self.splits.push(split);
            self.bits_read += 1;
            Ok(self.bits.next().unwrap_or(0))
        }
    }

    struct ScriptedSpecZpTables;

    impl SpecZpTables for ScriptedSpecZpTables {
        fn delta(&self, _state: u8) -> u16 {
            0x8000
        }

        fn theta(&self, _state: u8) -> u16 {
            0
        }

        fn mu(&self, _state: u8) -> u8 {
            83
        }

        fn lambda(&self, _state: u8) -> u8 {
            84
        }
    }

    struct DevOracleZpTableGenerator;

    impl ZpTableGenerator for DevOracleZpTableGenerator {
        fn delta(&self, state: usize) -> u16 {
            djvu_zp::tables::PROB[state]
        }

        fn theta(&self, state: usize) -> u16 {
            candidate_theta(state)
        }

        fn mu(&self, state: usize) -> u8 {
            djvu_zp::tables::MPS_NEXT[state]
        }

        fn lambda(&self, state: usize) -> u8 {
            djvu_zp::tables::LPS_NEXT[state]
        }
    }

    fn candidate_zp_table_set() -> ZpTableSet {
        ZpTableSet::generate(&DevOracleZpTableGenerator)
    }

    fn candidate_theta(state: usize) -> u16 {
        dev_oracle_theta(state)
    }

    fn dev_oracle_theta(state: usize) -> u16 {
        djvu_zp::tables::THRESHOLD[state]
    }

    fn estimate_theta_from_delta(delta: u16) -> u16 {
        if delta == 0x8000 {
            return 0;
        }

        let complement = (u32::from(delta) * 0x5a82 + 0x4000) >> 15;
        u16::try_from(0x8000 - complement).expect("theta estimate should fit in u16")
    }

    fn djvu_zp_oracle_table_set() -> ZpTableSet {
        ZpTableSet {
            delta: djvu_zp::tables::PROB,
            theta: djvu_zp::tables::THRESHOLD,
            mu: djvu_zp::tables::MPS_NEXT,
            lambda: djvu_zp::tables::LPS_NEXT,
        }
    }
}
