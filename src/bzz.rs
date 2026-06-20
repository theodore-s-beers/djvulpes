use crate::dirm::Dirm;
use crate::error::ParseError;

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
}

/// Decodes `DjVu` BZZ-compressed bytes.
///
/// This uses the in-memory Rust decoder.
///
/// # Errors
///
/// Returns an error if the input is malformed or uses BZZ features the in-memory
/// decoder does not support.
pub fn decode_bzz(bytes: &[u8]) -> BzzResult<Vec<u8>> {
    decode_bzz_in_memory(bytes)
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

fn decode_bzz_in_memory(bytes: &[u8]) -> BzzResult<Vec<u8>> {
    let mut decoder = VerifiedBzzDecoder::new(bytes)?;
    let mut output = Vec::new();

    loop {
        let block_size = decoder.next_block_len();

        if block_size == 0 {
            return Ok(output);
        }
        if block_size > MAX_BLOCK_BYTES {
            return Err(BzzError::UnsupportedBlockSize { size: block_size });
        }

        let fshift = decoder.next_fshift();
        let ranks = decoder.decode_block_ranks(block_size, fshift);
        output.extend(decode_block_from_ranks(block_size, fshift, ranks)?);
    }
}

const MAX_BLOCK_BYTES: usize = 4096 * 1024;
const BWT_MARKER_RANK: usize = 256;
const BZZ_FREQ_SLOTS: usize = 4;
const INITIAL_PREVIOUS_MTFNO: usize = 3;

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
            freq: [0; BZZ_FREQ_SLOTS],
            fadd: 4,
            fshift,
        }
    }

    fn take(&mut self, rank: usize) -> BzzResult<u8> {
        if rank >= self.symbols.len() {
            return Err(BzzError::InvalidSymbolRank { rank });
        }

        let symbol = self.symbols[rank];
        self.update_fadd();
        let symbol_freq = self
            .freq
            .get(rank)
            .copied()
            .unwrap_or(0)
            .saturating_add(self.fadd);

        let mut insert_at = rank;
        if insert_at >= BZZ_FREQ_SLOTS {
            for index in (BZZ_FREQ_SLOTS..=rank).rev() {
                self.symbols[index] = self.symbols[index - 1];
            }
            insert_at = BZZ_FREQ_SLOTS - 1;
        }

        while insert_at > 0 && symbol_freq >= self.freq[insert_at - 1] {
            self.symbols[insert_at] = self.symbols[insert_at - 1];
            self.freq[insert_at] = self.freq[insert_at - 1];
            insert_at -= 1;
        }

        self.symbols[insert_at] = symbol;
        self.freq[insert_at] = symbol_freq;
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
            self.fadd >>= 24;
            for freq in &mut self.freq {
                *freq >>= 24;
            }
        }
    }
}

struct VerifiedBzzDecoder<'a> {
    zp: SpecZpDecoder<'a>,
    contexts: [u8; BZZ_MTF_CONTEXTS],
    tables: &'static ZpTableSet,
}

impl<'a> VerifiedBzzDecoder<'a> {
    fn new(bytes: &'a [u8]) -> BzzResult<Self> {
        if bytes.len() < 2 {
            return Err(BzzError::IncompleteDecoder("ZP input is too short"));
        }

        Ok(Self {
            zp: SpecZpDecoder::new(bytes),
            contexts: [0; BZZ_MTF_CONTEXTS],
            tables: &DJVU_ZP_TABLES,
        })
    }

    fn next_block_len(&mut self) -> usize {
        let mut value = 1usize;
        let block_len_marker = 1usize << 24;

        while value < block_len_marker {
            value = (value << 1) | usize::from(self.zp.decode_raw());
        }

        value - block_len_marker
    }

    fn next_fshift(&mut self) -> u8 {
        if self.zp.decode_raw() == 0 {
            return 0;
        }
        1 + self.zp.decode_raw()
    }

    fn decode_block_ranks(&mut self, block_len: usize, _fshift: u8) -> Vec<usize> {
        let mut ranks = Vec::with_capacity(block_len);
        let mut previous_mtfno = INITIAL_PREVIOUS_MTFNO;

        for _ in 0..block_len {
            let rank = read_verified_entropy_rank(
                &mut self.zp,
                &mut self.contexts,
                self.tables,
                previous_mtfno,
            );
            previous_mtfno = rank;
            ranks.push(rank);
        }

        ranks
    }
}

fn read_verified_entropy_rank(
    zp: &mut SpecZpDecoder<'_>,
    contexts: &mut [u8; BZZ_MTF_CONTEXTS],
    tables: &ZpTableSet,
    previous_mtfno: usize,
) -> usize {
    let previous_class = previous_mtfno.min(2);
    if zp.decode_bit(
        &mut contexts[RANK_ZERO_CONTEXT_START + previous_class],
        tables,
    ) == 1
    {
        return 0;
    }
    if zp.decode_bit(
        &mut contexts[RANK_ONE_CONTEXT_START + previous_class],
        tables,
    ) == 1
    {
        return 1;
    }

    for range in BZZ_MTF_RANGES {
        if zp.decode_bit(&mut contexts[range.selector_context], tables) == 1 {
            let offset =
                read_verified_mtfno_bin(zp, contexts, tables, range.bits, range.tree_context);
            return range.start + offset;
        }
    }

    BWT_MARKER_RANK
}

fn read_verified_mtfno_bin(
    zp: &mut SpecZpDecoder<'_>,
    contexts: &mut [u8; BZZ_MTF_CONTEXTS],
    tables: &ZpTableSet,
    bits: usize,
    context_start: usize,
) -> usize {
    let mut value = 0;
    let mut node = 0;

    for _ in 0..bits {
        let bit = usize::from(zp.decode_bit(&mut contexts[context_start + node], tables));
        value = (value << 1) | bit;
        node = (node << 1) + 1 + bit;
    }

    value
}

const BZZ_MTF_CONTEXTS: usize = 262;
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

struct SpecZpDecoder<'a> {
    input: &'a [u8],
    cursor: usize,
    a: u32,
    c: u32,
    fence: u32,
    bit_buffer: u32,
    buffered_bits: i32,
}

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

trait SpecZpTables {
    fn delta(&self, state: u8) -> u16;
    fn theta(&self, state: u8) -> u16;
    fn mu(&self, state: u8) -> u8;
    fn lambda(&self, state: u8) -> u8;
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ZpTableSet {
    delta: [u16; 256],
    theta: [u16; 256],
    mu: [u8; 256],
    lambda: [u8; 256],
}

impl ZpTableSet {
    #[cfg(test)]
    fn validate_shape(&self) {
        for state in 0..ZP_VALID_STATES {
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

        for state in ZP_VALID_STATES..ZP_TABLE_LEN {
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

const ZP_TABLE_LEN: usize = 256;
const ZP_VALID_STATES: usize = 251;
const ZP_EARLY_STATES: usize = 83;

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

const fn zp_u16_state_table(values: &[u16; ZP_VALID_STATES], padding: u16) -> [u16; ZP_TABLE_LEN] {
    let mut table = [padding; ZP_TABLE_LEN];
    let mut index = 0;
    while index < values.len() {
        table[index] = values[index];
        index += 1;
    }
    table
}

const fn zp_u8_state_table(values: &[u8; ZP_VALID_STATES], padding: u8) -> [u8; ZP_TABLE_LEN] {
    let mut table = [padding; ZP_TABLE_LEN];
    let mut index = 0;
    while index < values.len() {
        table[index] = values[index];
        index += 1;
    }
    table
}

const fn zp_theta_table() -> [u16; ZP_TABLE_LEN] {
    let initial: [u16; ZP_EARLY_STATES] = [
        0x0000, 0x0000, 0x0000, 0x10a5, 0x10a5, 0x1f28, 0x1f28, 0x2bd3, 0x2bd3, 0x36e3, 0x36e3,
        0x408c, 0x408c, 0x48fd, 0x48fd, 0x505d, 0x505d, 0x56d0, 0x56d0, 0x5c71, 0x5c71, 0x615b,
        0x615b, 0x65a5, 0x65a5, 0x6962, 0x6962, 0x6ca2, 0x6ca2, 0x6f74, 0x6f74, 0x71e6, 0x71e6,
        0x7404, 0x7404, 0x75d6, 0x75d6, 0x7768, 0x7768, 0x78c2, 0x78c2, 0x79ea, 0x79ea, 0x7ae7,
        0x7ae7, 0x7bbe, 0x7bbe, 0x7c75, 0x7c75, 0x7d0f, 0x7d0f, 0x7d91, 0x7d91, 0x7dfe, 0x7dfe,
        0x7e5a, 0x7e5a, 0x7ea6, 0x7ea6, 0x7ee6, 0x7ee6, 0x7f1a, 0x7f1a, 0x7f45, 0x7f45, 0x7f6b,
        0x7f6b, 0x7f8d, 0x7f8d, 0x7faa, 0x7faa, 0x7fc3, 0x7fc3, 0x7fd7, 0x7fd7, 0x7fe7, 0x7fe7,
        0x7ff2, 0x7ff2, 0x7ffa, 0x7ffa, 0x7fff, 0x7fff,
    ];
    let mut theta = [0; ZP_TABLE_LEN];
    let mut index = 0;
    while index < initial.len() {
        theta[index] = initial[index];
        index += 1;
    }
    theta
}

// DjVu ZP coder format constants from the public DjVu v3 specification.
//
// The first 251 entries are valid ZP states. The final five entries are padding
// for total `u8` index coverage and must not be reached by well-formed state
// transitions. Tests validate these values against the MIT `djvu-zp` dev oracle
// and separately check the structural invariants we depend on at runtime.
const DJVU_ZP_TABLES: ZpTableSet = ZpTableSet {
    delta: zp_u16_state_table(
        &[
            0x8000, 0x8000, 0x8000, 0x6bbd, 0x6bbd, 0x5d45, 0x5d45, 0x51b9, 0x51b9, 0x4813, 0x4813,
            0x3fd5, 0x3fd5, 0x38b1, 0x38b1, 0x3275, 0x3275, 0x2cfd, 0x2cfd, 0x2825, 0x2825, 0x23ab,
            0x23ab, 0x1f87, 0x1f87, 0x1bbb, 0x1bbb, 0x1845, 0x1845, 0x1523, 0x1523, 0x1253, 0x1253,
            0x0fcf, 0x0fcf, 0x0d95, 0x0d95, 0x0b9d, 0x0b9d, 0x09e3, 0x09e3, 0x0861, 0x0861, 0x0711,
            0x0711, 0x05f1, 0x05f1, 0x04f9, 0x04f9, 0x0425, 0x0425, 0x0371, 0x0371, 0x02d9, 0x02d9,
            0x0259, 0x0259, 0x01ed, 0x01ed, 0x0193, 0x0193, 0x0149, 0x0149, 0x010b, 0x010b, 0x00d5,
            0x00d5, 0x00a5, 0x00a5, 0x007b, 0x007b, 0x0057, 0x0057, 0x003b, 0x003b, 0x0023, 0x0023,
            0x0013, 0x0013, 0x0007, 0x0007, 0x0001, 0x0001, 0x5695, 0x24ee, 0x8000, 0x0d30, 0x481a,
            0x0481, 0x3579, 0x017a, 0x24ef, 0x007b, 0x1978, 0x0028, 0x10ca, 0x000d, 0x0b5d, 0x0034,
            0x078a, 0x00a0, 0x050f, 0x0117, 0x0358, 0x01ea, 0x0234, 0x0144, 0x0173, 0x0234, 0x00f5,
            0x0353, 0x00a1, 0x05c5, 0x011a, 0x03cf, 0x01aa, 0x0285, 0x0286, 0x01ab, 0x03d3, 0x011a,
            0x05c5, 0x00ba, 0x08ad, 0x007a, 0x0ccc, 0x01eb, 0x1302, 0x02e6, 0x1b81, 0x045e, 0x24ef,
            0x0690, 0x2865, 0x09de, 0x3987, 0x0dc8, 0x2c99, 0x10ca, 0x3b5f, 0x0b5d, 0x5695, 0x078a,
            0x8000, 0x050f, 0x24ee, 0x0358, 0x0d30, 0x0234, 0x0481, 0x0173, 0x017a, 0x00f5, 0x007b,
            0x00a1, 0x0028, 0x011a, 0x000d, 0x01aa, 0x0034, 0x0286, 0x00a0, 0x03d3, 0x0117, 0x05c5,
            0x01ea, 0x08ad, 0x0144, 0x0ccc, 0x0234, 0x1302, 0x0353, 0x1b81, 0x05c5, 0x24ef, 0x03cf,
            0x2b74, 0x0285, 0x201d, 0x01ab, 0x1715, 0x011a, 0x0fb7, 0x00ba, 0x0a67, 0x01eb, 0x06e7,
            0x02e6, 0x0496, 0x045e, 0x030d, 0x0690, 0x0206, 0x09de, 0x0155, 0x0dc8, 0x00e1, 0x2b74,
            0x0094, 0x201d, 0x0188, 0x1715, 0x0252, 0x0fb7, 0x0383, 0x0a67, 0x0547, 0x06e7, 0x07e2,
            0x0496, 0x0bc0, 0x030d, 0x1178, 0x0206, 0x19da, 0x0155, 0x24ef, 0x00e1, 0x320e, 0x0094,
            0x432a, 0x0188, 0x447d, 0x0252, 0x5ece, 0x0383, 0x8000, 0x0547, 0x481a, 0x07e2, 0x3579,
            0x0bc0, 0x24ef, 0x1178, 0x1978, 0x19da, 0x2865, 0x24ef, 0x3987, 0x320e, 0x2c99, 0x432a,
            0x3b5f, 0x447d, 0x5695, 0x5ece, 0x8000, 0x8000, 0x5695, 0x481a, 0x481a,
        ],
        0x8000,
    ),
    theta: zp_theta_table(),
    mu: zp_u8_state_table(
        &[
            84, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46,
            47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68,
            69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 81, 82, 9, 86, 5, 88, 89, 90,
            91, 92, 93, 94, 95, 96, 97, 82, 99, 76, 101, 70, 103, 66, 105, 106, 107, 66, 109, 60,
            111, 56, 69, 114, 65, 116, 61, 118, 57, 120, 53, 122, 49, 124, 43, 72, 39, 60, 33, 56,
            29, 52, 23, 48, 23, 42, 137, 38, 21, 140, 15, 142, 9, 144, 141, 146, 147, 148, 149,
            150, 151, 152, 153, 154, 155, 70, 157, 66, 81, 62, 75, 58, 69, 54, 65, 50, 167, 44, 65,
            40, 59, 34, 55, 30, 175, 24, 177, 178, 179, 180, 181, 182, 183, 184, 69, 186, 59, 188,
            55, 190, 51, 192, 47, 194, 41, 196, 37, 198, 199, 72, 201, 62, 203, 58, 205, 54, 207,
            50, 209, 46, 211, 40, 213, 36, 215, 30, 217, 26, 219, 20, 71, 14, 61, 14, 57, 8, 53,
            228, 49, 230, 45, 232, 39, 234, 35, 138, 29, 24, 25, 240, 19, 22, 13, 16, 13, 10, 7,
            244, 249, 10, 89, 230,
        ],
        0,
    ),
    lambda: zp_u8_state_table(
        &[
            145, 4, 3, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
            22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43,
            44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65,
            66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 85, 226, 6, 176, 143, 138,
            141, 112, 135, 104, 133, 100, 129, 98, 127, 72, 125, 102, 123, 60, 121, 110, 119, 108,
            117, 54, 115, 48, 113, 134, 59, 132, 55, 130, 51, 128, 47, 126, 41, 62, 37, 66, 31, 54,
            25, 50, 131, 46, 17, 40, 15, 136, 7, 32, 139, 172, 9, 170, 85, 168, 248, 166, 247, 164,
            197, 162, 95, 160, 173, 158, 165, 156, 161, 60, 159, 56, 71, 52, 163, 48, 59, 42, 171,
            38, 169, 32, 53, 26, 47, 174, 193, 18, 191, 222, 189, 218, 187, 216, 185, 214, 61, 212,
            53, 210, 49, 208, 45, 206, 39, 204, 195, 202, 31, 200, 243, 64, 239, 56, 237, 52, 235,
            48, 233, 44, 231, 38, 229, 34, 227, 28, 225, 22, 223, 16, 221, 220, 63, 8, 55, 224, 51,
            2, 47, 87, 43, 246, 37, 244, 33, 238, 27, 236, 21, 16, 15, 8, 241, 242, 7, 10, 245, 2,
            1, 83, 250, 2, 143, 246,
        ],
        0,
    ),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dirm;

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
        let decoded = decode_bzz(HELLO_BZZ).expect("fixture should decode without external tools");

        assert_eq!(decoded, HELLO_RAW);
    }

    #[test]
    fn djvu_bzz_oracle_decodes_fixture_bytes() {
        let decoded = djvu_bzz::bzz_decode(HELLO_BZZ).expect("djvu-bzz should decode fixture");

        assert_eq!(decoded, HELLO_RAW);
    }

    #[test]
    fn bzz_decoder_reads_fixture_block_len() {
        let mut decoder = VerifiedBzzDecoder::new(HELLO_BZZ).expect("fixture should initialize");

        assert_eq!(decoder.next_block_len(), HELLO_RAW.len() + 1);
    }

    #[test]
    fn bzz_decoder_reads_fixture_fshift_after_block_len() {
        let mut decoder = VerifiedBzzDecoder::new(HELLO_BZZ).expect("fixture should initialize");

        assert_eq!(decoder.next_block_len(), HELLO_RAW.len() + 1);
        assert_eq!(decoder.next_fshift(), 0);
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
        let tables = &DJVU_ZP_TABLES;
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
                let our_bit = ours.decode_bit(&mut our_contexts[context], tables) != 0;
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
        let tables = &DJVU_ZP_TABLES;
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
            let our_bit = ours.decode_bit(&mut our_contexts[context], tables) != 0;
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
    fn runtime_zp_table_set_has_valid_shape() {
        DJVU_ZP_TABLES.validate_shape();
    }

    #[test]
    fn runtime_zp_table_padding_is_generated() {
        for state in ZP_VALID_STATES..ZP_TABLE_LEN {
            assert_eq!(DJVU_ZP_TABLES.delta[state], 0x8000);
            assert_eq!(DJVU_ZP_TABLES.theta[state], 0);
            assert_eq!(DJVU_ZP_TABLES.mu[state], 0);
            assert_eq!(DJVU_ZP_TABLES.lambda[state], 0);
        }
    }

    #[test]
    fn runtime_zp_table_set_matches_dev_oracle() {
        assert_eq!(DJVU_ZP_TABLES, djvu_zp_oracle_table_set());
    }

    #[test]
    fn zp_table_phases_match_spec_structure() {
        let tables = &DJVU_ZP_TABLES;

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
        let tables = &DJVU_ZP_TABLES;

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
        let tables = &DJVU_ZP_TABLES;

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
    fn in_memory_decoder_decodes_fixture_bytes() {
        let decoded = decode_bzz_in_memory(HELLO_BZZ).expect("in-memory BZZ should decode fixture");

        assert_eq!(decoded, HELLO_RAW);
    }

    #[test]
    fn in_memory_decoder_decodes_generated_oracle_cases() {
        let mut patterned = Vec::with_capacity(1024);
        for index in 0..1024u32 {
            patterned.push(index.wrapping_mul(7).wrapping_add(13).to_le_bytes()[0]);
        }

        let cases = [
            Vec::new(),
            b"A".to_vec(),
            b"aaaaaaaaaa".to_vec(),
            (0..=u8::MAX).collect::<Vec<_>>(),
            patterned,
        ];

        for raw in &cases {
            let compressed = djvu_bzz::bzz_encode(raw);
            let decoded =
                decode_bzz_in_memory(&compressed).expect("generated BZZ should decode in memory");

            assert_eq!(&decoded, raw, "roundtrip failed for {raw:02x?}");
        }
    }

    #[test]
    fn spec_zp_rank_stream_matches_expected_fixture_ranks() {
        let tables = &DJVU_ZP_TABLES;
        let mut decoder = SpecZpDecoder::new(HELLO_BZZ);
        let block_len = spec_zp_next_block_len(&mut decoder);
        let fshift = spec_zp_next_fshift(&mut decoder);
        let mut contexts = [0; BZZ_MTF_CONTEXTS];
        let mut previous_mtfno = INITIAL_PREVIOUS_MTFNO;
        let mut ranks = Vec::with_capacity(block_len);

        for _ in 0..block_len {
            let rank =
                read_spec_zp_entropy_rank(&mut decoder, &mut contexts, tables, previous_mtfno);
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
        if decoder.decode_raw() == 0 {
            0
        } else {
            1 + decoder.decode_raw()
        }
    }

    fn read_spec_zp_entropy_rank(
        decoder: &mut SpecZpDecoder<'_>,
        contexts: &mut [u8; BZZ_MTF_CONTEXTS],
        tables: &ZpTableSet,
        previous_mtfno: usize,
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

        for range in BZZ_MTF_RANGES {
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

    fn bzz_mtfno_context_path(rank: usize, previous_mtfno: usize) -> Vec<(usize, u8)> {
        let mut path = Vec::new();
        let previous_class = previous_mtfno.min(2);

        path.push((previous_class, u8::from(rank == 0)));
        if rank == 0 {
            return path;
        }

        path.push((RANK_ONE_CONTEXT_START + previous_class, u8::from(rank == 1)));
        if rank == 1 {
            return path;
        }

        for range in BZZ_MTF_RANGES {
            let range_end = range.start + (1usize << range.bits) - 1;
            let selected = rank >= range.start && rank <= range_end;
            path.push((range.selector_context, u8::from(selected)));
            if selected {
                path.extend(bzz_decode_bin_context_path(
                    range.bits,
                    range.tree_context,
                    rank - range.start,
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
        let mut path = Vec::with_capacity(bits);
        let mut node = 0usize;

        for bit_index in (0..bits).rev() {
            let bit = u8::from((value & (1usize << bit_index)) != 0);
            path.push((context_start + node, bit));
            node = (node << 1) + 1 + usize::from(bit);
        }

        path
    }

    fn entropy_decisions_for_rank(rank: usize, previous_mtfno: usize) -> Vec<(usize, u8)> {
        bzz_mtfno_context_path(rank, previous_mtfno)
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

    fn djvu_zp_oracle_table_set() -> ZpTableSet {
        ZpTableSet {
            delta: djvu_zp::tables::PROB,
            theta: djvu_zp::tables::THRESHOLD,
            mu: djvu_zp::tables::MPS_NEXT,
            lambda: djvu_zp::tables::LPS_NEXT,
        }
    }
}
