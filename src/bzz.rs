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
const RANK_TREE_BITS: usize = 8;

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

    let mut offset = 0usize;
    for bit_index in 0..RANK_TREE_BITS {
        let bit = model.read_bit(reader, RANK_TREE_CONTEXT_START + bit_index)?;
        offset = (offset << 1) | usize::from(bit);
    }

    Ok(2 + offset)
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
        let mut reader = ScriptedModeledBitReader::new([0, 0, 0, 0, 0, 0, 0, 1, 0, 1]);

        let rank = read_entropy_rank(&mut model, &mut reader).expect("rank should decode");

        assert_eq!(rank, 7);
        assert_eq!(reader.bits_read(), 10);
    }

    #[test]
    fn read_entropy_rank_decodes_marker_rank() {
        let mut model = BitModel::with_contexts(ENTROPY_CONTEXTS);
        let mut reader = ScriptedModeledBitReader::new([0, 0, 1, 1, 1, 1, 1, 1, 1, 0]);

        let rank = read_entropy_rank(&mut model, &mut reader).expect("rank should decode");

        assert_eq!(rank, BWT_MARKER_RANK);
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
