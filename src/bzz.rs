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
            | BzzError::InvalidBwtTransform,
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

    let ranks = decoder.decode_block_ranks(block_size)?;
    decode_block_from_ranks(block_size, ranks)
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

fn decode_block_from_ranks<I>(block_len: usize, ranks: I) -> BzzResult<Vec<u8>>
where
    I: IntoIterator<Item = usize>,
{
    let block = block_symbols_from_ranks(block_len, ranks)?;
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

fn block_symbols_from_ranks<I>(block_len: usize, ranks: I) -> BzzResult<BlockSymbols>
where
    I: IntoIterator<Item = usize>,
{
    let mut table = MoveToFrontTable::new();
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

struct MoveToFrontTable {
    symbols: Vec<u8>,
}

impl MoveToFrontTable {
    fn new() -> Self {
        Self {
            symbols: (0..=u8::MAX).collect(),
        }
    }

    fn take(&mut self, rank: usize) -> BzzResult<u8> {
        if rank >= self.symbols.len() {
            return Err(BzzError::InvalidSymbolRank { rank });
        }

        let symbol = self.symbols.remove(rank);
        self.symbols.insert(0, symbol);
        Ok(symbol)
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
            let split = 0x8000 + (u32::from(self.bits.lower_bound()) >> 1);
            value = (value << 1) | usize::from(self.bits.read_split_bit(split));
        }

        value - block_len_marker
    }

    fn decode_block_ranks(&mut self, block_len: usize) -> BzzResult<Vec<usize>> {
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
}

impl<'bits, 'input> EntropyRankDecoder<'bits, 'input> {
    fn new(bits: &'bits mut ZpBitReader<'input>) -> Self {
        Self {
            bits,
            model: BitModel::with_contexts(ENTROPY_CONTEXTS),
        }
    }
}

impl RankDecoder for EntropyRankDecoder<'_, '_> {
    fn next_rank(&mut self) -> BzzResult<usize> {
        read_entropy_rank(&mut self.model, self.bits)
    }
}

const ENTROPY_CONTEXTS: usize = 300;
const RANK_ZERO_CONTEXT: usize = 0;
const RANK_ONE_CONTEXT: usize = 1;
const RANK_TREE_CONTEXT_START: usize = 2;
const RANK_GROUPS: usize = 7;

fn read_entropy_rank<R>(model: &mut BitModel, reader: &mut R) -> BzzResult<usize>
where
    R: ModeledBitReader,
{
    if model.read_bit(reader, RANK_ZERO_CONTEXT)? == 1 {
        return Ok(0);
    }
    if model.read_bit(reader, RANK_ONE_CONTEXT)? == 1 {
        return Ok(1);
    }

    let mut group_start = 2usize;
    let mut context = RANK_TREE_CONTEXT_START;

    for group_bits in 1..=RANK_GROUPS {
        if model.read_bit(reader, context)? == 1 {
            let mut offset = 0usize;
            for bit_index in 0..group_bits {
                let bit = model.read_bit(reader, context + 1 + bit_index)?;
                offset = (offset << 1) | usize::from(bit);
            }
            return Ok(group_start + offset);
        }

        context += 1 + group_bits;
        group_start <<= 1;
    }

    Ok(BWT_MARKER_RANK)
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
            next_zero[state] = u8::try_from((state + 4).min(BIT_MODEL_STATES - 1))
                .expect("bit-model state should fit in u8");
            next_one[state] =
                u8::try_from(state.saturating_sub(4)).expect("bit-model state should fit in u8");
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
        let numerator = (state + 1) * usize::from(u16::MAX);
        u16::try_from(numerator / BIT_MODEL_STATES)
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
    fn in_memory_decoder_reports_incomplete_after_block_size() {
        let error =
            decode_bzz_in_memory(HELLO_BZZ).expect_err("full in-memory decode is not complete yet");

        assert!(matches!(error, BzzError::MissingBwtMarker));
    }

    #[test]
    fn bzz_decoder_rank_decode_returns_block_len_with_provisional_model() {
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let block_len = decoder.next_block_len();
        let ranks = decoder
            .decode_block_ranks(block_len)
            .expect("provisional rank decoder should produce one block of ranks");

        assert_eq!(ranks.len(), block_len);
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
        let block = block_symbols_from_ranks(5, [98, 98, 0, BWT_MARKER_RANK, 0])
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
    fn block_symbols_from_ranks_rejects_missing_marker() {
        let error = block_symbols_from_ranks(3, [0, 1, 2]).expect_err("marker should be required");

        assert!(matches!(error, BzzError::MissingBwtMarker));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_duplicate_marker() {
        let error = block_symbols_from_ranks(3, [0, BWT_MARKER_RANK, BWT_MARKER_RANK])
            .expect_err("duplicate marker should fail");

        assert!(matches!(error, BzzError::DuplicateBwtMarker));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_out_of_range_rank() {
        let error =
            block_symbols_from_ranks(2, [257, BWT_MARKER_RANK]).expect_err("bad rank should fail");

        assert!(matches!(error, BzzError::InvalidSymbolRank { rank: 257 }));
    }

    #[test]
    fn block_symbols_from_ranks_rejects_truncated_input() {
        let error =
            block_symbols_from_ranks(2, [BWT_MARKER_RANK]).expect_err("short ranks should fail");

        assert!(matches!(
            error,
            BzzError::TruncatedRankStream { expected_len: 2 }
        ));
    }

    #[test]
    fn decode_block_from_ranks_reconstructs_banana() {
        let decoded = decode_block_from_ranks(7, [97, 110, 0, 99, BWT_MARKER_RANK, 2, 0])
            .expect("rank pipeline should decode");

        assert_eq!(decoded, b"banana");
    }

    #[test]
    fn decode_block_from_ranks_reconstructs_repeated_bytes() {
        let decoded = decode_block_from_ranks(4, [97, 0, 0, BWT_MARKER_RANK])
            .expect("rank pipeline should decode");

        assert_eq!(decoded, b"aaa");
    }

    #[test]
    fn expected_fixture_ranks_reconstruct_hello_raw() {
        let ranks = expected_ranks_from_raw(HELLO_RAW);

        let decoded = decode_block_from_ranks(HELLO_RAW.len() + 1, ranks)
            .expect("rank fixture should decode");

        assert_eq!(decoded, HELLO_RAW);
    }

    #[test]
    fn decode_block_from_ranks_propagates_rank_errors() {
        let error =
            decode_block_from_ranks(2, [257, BWT_MARKER_RANK]).expect_err("bad rank should fail");

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

        assert_eq!(tables.split(0), 1008);
        assert_eq!(tables.split(BIT_MODEL_INITIAL_STATE), 33_271);
        assert_eq!(tables.split(64), u16::MAX);
        assert!(tables.split(1) > tables.split(0));
        assert!(tables.next_state(BIT_MODEL_INITIAL_STATE, 0) > BIT_MODEL_INITIAL_STATE);
        assert!(tables.next_state(BIT_MODEL_INITIAL_STATE, 1) < BIT_MODEL_INITIAL_STATE);
    }

    #[test]
    fn read_entropy_rank_decodes_rank_zero() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([1]);

        let rank = read_entropy_rank(&mut model, &mut reader).expect("rank should decode");

        assert_eq!(rank, 0);
        assert_eq!(reader.bits_read(), 1);
    }

    #[test]
    fn read_entropy_rank_decodes_rank_one() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 1]);

        let rank = read_entropy_rank(&mut model, &mut reader).expect("rank should decode");

        assert_eq!(rank, 1);
        assert_eq!(reader.bits_read(), 2);
    }

    #[test]
    fn read_entropy_rank_decodes_large_rank() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 0, 0, 1, 1, 1]);

        let rank = read_entropy_rank(&mut model, &mut reader).expect("rank should decode");

        assert_eq!(rank, 7);
        assert_eq!(reader.bits_read(), 6);
    }

    #[test]
    fn read_entropy_rank_decodes_marker_rank() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 0, 0, 0, 0, 0, 0, 0, 0]);

        let rank = read_entropy_rank(&mut model, &mut reader).expect("rank should decode");

        assert_eq!(rank, BWT_MARKER_RANK);
        assert_eq!(reader.bits_read(), 9);
    }

    #[test]
    fn entropy_decisions_for_rank_encode_rank_tree_paths() {
        assert_eq!(entropy_decisions_for_rank(0), vec![(RANK_ZERO_CONTEXT, 1)]);
        assert_eq!(
            entropy_decisions_for_rank(1),
            vec![(RANK_ZERO_CONTEXT, 0), (RANK_ONE_CONTEXT, 1)]
        );
        assert_eq!(
            entropy_decisions_for_rank(7),
            vec![
                (RANK_ZERO_CONTEXT, 0),
                (RANK_ONE_CONTEXT, 0),
                (RANK_TREE_CONTEXT_START, 0),
                (RANK_TREE_CONTEXT_START + 2, 1),
                (RANK_TREE_CONTEXT_START + 3, 1),
                (RANK_TREE_CONTEXT_START + 4, 1),
            ]
        );
        assert_eq!(
            entropy_decisions_for_rank(BWT_MARKER_RANK),
            vec![
                (RANK_ZERO_CONTEXT, 0),
                (RANK_ONE_CONTEXT, 0),
                (RANK_TREE_CONTEXT_START, 0),
                (RANK_TREE_CONTEXT_START + 2, 0),
                (RANK_TREE_CONTEXT_START + 5, 0),
                (RANK_TREE_CONTEXT_START + 9, 0),
                (RANK_TREE_CONTEXT_START + 14, 0),
                (RANK_TREE_CONTEXT_START + 20, 0),
                (RANK_TREE_CONTEXT_START + 27, 0),
            ]
        );
    }

    #[test]
    #[ignore = "tracks convergence of provisional entropy model against the BZZ fixture"]
    fn fixture_rank_stream_matches_expected_ranks() {
        let expected = expected_ranks_from_raw(HELLO_RAW);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let block_len = decoder.next_block_len();
        let actual = decoder
            .decode_block_ranks(block_len)
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
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let bit_trace =
            trace_fixture_modeled_bits(&mut model, &mut decoder.bits, &expected_decisions)
                .expect("modeled fixture bits should decode");
        let mismatch = first_modeled_bit_mismatch(&bit_trace)
            .expect("fixture should still expose the provisional mismatch");
        let expected = &expected_trace[mismatch.index];

        assert_eq!(mismatch.index, 4);
        assert_eq!(mismatch.context_id, 7);
        assert_eq!(mismatch.state_before, BIT_MODEL_INITIAL_STATE);
        assert_eq!(mismatch.expected_bit, 1);
        assert_eq!(mismatch.actual_bit, 0);
        assert_eq!(expected.rank_index, 0);
        assert_eq!(expected.rank, 10);
        assert_eq!(expected.rank_decision_index, 4);

        let actual_first_rank_decisions = bit_trace
            .iter()
            .take_while(|entry| entry.index <= mismatch.index)
            .map(|entry| (entry.context_id, entry.actual_bit))
            .collect::<Vec<_>>();

        assert_eq!(
            actual_first_rank_decisions,
            vec![
                (RANK_ZERO_CONTEXT, 0),
                (RANK_ONE_CONTEXT, 0),
                (RANK_TREE_CONTEXT_START, 0),
                (RANK_TREE_CONTEXT_START + 2, 0),
                (RANK_TREE_CONTEXT_START + 5, 0),
            ]
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

        assert_eq!(best.0.group_selected_bit, 1);
        assert!(
            best.1 <= 4,
            "simple variant unexpectedly improved to {best:?}"
        );
        eprintln!("rank-tree variant scores: {scored:?}");
    }

    #[test]
    #[ignore = "checks the initial split range needed by the first rank"]
    fn fixture_first_rank_initial_split_constraints_are_satisfiable() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
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

        assert!(merged.is_satisfiable());
        assert!(merged.min_split > BitModelTables::generated().split(BIT_MODEL_INITIAL_STATE));
        eprintln!("first-rank initial-state constraints: {first_rank_initial_state:?}");
        eprintln!("merged initial-state constraint: {merged:?}");
    }

    #[test]
    #[ignore = "calibrates provisional bit-model splits against expected fixture decisions"]
    fn fixture_expected_decisions_have_consistent_split_constraints() {
        let expected_ranks = expected_ranks_from_raw(HELLO_RAW);
        let expected_decisions = entropy_decisions_from_ranks(&expected_ranks);
        let mut decoder = BzzDecoder::new(HELLO_BZZ);
        let _ = decoder.next_block_len();
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
        assert_eq!(
            conflict.key,
            SplitConstraintKey {
                context_id: 23,
                state: BIT_MODEL_INITIAL_STATE,
            }
        );
        assert_eq!(conflict.decision, 56);
        assert_eq!(conflict.min_split, 63_481);
        assert_eq!(conflict.min_decision, 28);
        assert_eq!(conflict.max_split, 30_494);
        assert_eq!(conflict.max_decision, 56);
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
        assert_eq!(first.split_before, 33_271);
        assert_eq!(first.state_after, 28);
        assert_eq!(second.expected_bit, 0);
        assert_eq!(second.state_before, 24);
        assert_eq!(second.split_before, 25_205);
        assert_eq!(second.state_after, 28);
        eprintln!("context 23 state trace: {state_trace:?}");
    }

    fn expected_ranks_from_raw(raw: &[u8]) -> Vec<usize> {
        let block = expected_block_symbols_from_raw(raw);
        ranks_from_block_symbols(&block).expect("derived BWT block should convert to ranks")
    }

    fn entropy_decisions_from_ranks(ranks: &[usize]) -> Vec<(usize, u8)> {
        entropy_decisions_from_ranks_with_options(ranks, RankDecisionOptions::default())
    }

    fn entropy_decisions_from_ranks_with_options(
        ranks: &[usize],
        options: RankDecisionOptions,
    ) -> Vec<(usize, u8)> {
        let mut decisions = Vec::new();
        for rank in ranks {
            decisions.extend(entropy_decisions_for_rank_with_options(*rank, options));
        }
        decisions
    }

    fn expected_decision_trace_from_ranks(ranks: &[usize]) -> Vec<ExpectedDecisionTrace> {
        let mut trace = Vec::new();
        for (rank_index, rank) in ranks.iter().copied().enumerate() {
            for (rank_decision_index, (context_id, expected_bit)) in
                entropy_decisions_for_rank(rank).into_iter().enumerate()
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

    fn entropy_decisions_for_rank(rank: usize) -> Vec<(usize, u8)> {
        entropy_decisions_for_rank_with_options(rank, RankDecisionOptions::default())
    }

    fn entropy_decisions_for_rank_with_options(
        rank: usize,
        options: RankDecisionOptions,
    ) -> Vec<(usize, u8)> {
        let mut decisions = vec![(RANK_ZERO_CONTEXT, u8::from(rank == 0))];
        if rank == 0 {
            return decisions;
        }

        decisions.push((RANK_ONE_CONTEXT, u8::from(rank == 1)));
        if rank == 1 {
            return decisions;
        }

        let mut group_start = 2usize;
        let mut context = RANK_TREE_CONTEXT_START;
        for group_bits in 1..=RANK_GROUPS {
            let group_end = (group_start << 1) - 1;
            let rank_is_in_group = rank >= group_start && rank <= group_end;
            decisions.push((
                context,
                if rank_is_in_group {
                    options.group_selected_bit
                } else {
                    1 - options.group_selected_bit
                },
            ));

            if rank_is_in_group {
                let offset = rank - group_start;
                let bit_indexes = if options.offset_msb_first {
                    (0..group_bits).rev().collect::<Vec<_>>()
                } else {
                    (0..group_bits).collect::<Vec<_>>()
                };
                for bit_index in bit_indexes {
                    let mut bit = u8::from((offset & (1usize << bit_index)) != 0);
                    if options.invert_offset_bits {
                        bit = 1 - bit;
                    }
                    decisions.push((context + 1 + (group_bits - 1 - bit_index), bit));
                }
                return decisions;
            }

            context += 1 + group_bits;
            group_start <<= 1;
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

    fn ranks_from_block_symbols(block: &BlockSymbols) -> BzzResult<Vec<usize>> {
        let mut table = MoveToFrontTable::new();
        let mut ranks = Vec::with_capacity(block.symbols.len());

        for (index, symbol) in block.symbols.iter().copied().enumerate() {
            if index == block.marker_pos {
                ranks.push(BWT_MARKER_RANK);
                continue;
            }

            let rank = table
                .symbols
                .iter()
                .position(|candidate| *candidate == symbol)
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
}
