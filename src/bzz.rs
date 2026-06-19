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
        Err(BzzError::IncompleteDecoder(_)) => decode_bzz_with_local_tool(bytes),
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
    let mut decoder = InMemoryDecoder::new(bytes);
    let block_size = decoder.decode_block_size();

    if block_size == 0 {
        return Ok(Vec::new());
    }
    if block_size > MAX_BLOCK_BYTES {
        return Err(BzzError::UnsupportedBlockSize { size: block_size });
    }

    Err(BzzError::IncompleteDecoder(
        "block symbol decoding and inverse BWT are not implemented yet",
    ))
}

const MAX_BLOCK_BYTES: usize = 4096 * 1024;

struct InMemoryDecoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    a: u16,
    buffer: u64,
    code: u16,
    fence: u16,
    byte: u8,
    delay: u8,
    scount: u8,
}

impl<'a> InMemoryDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let (code, pos) = match bytes {
            [first, second, ..] => (u16::from_be_bytes([*first, *second]), 2),
            [first] => ((u16::from(*first) << 8) | 0x00ff, 1),
            [] => (0xffff, 0),
        };
        let fence = if code >= 0x8000 { 0x7fff } else { code };

        Self {
            bytes,
            pos,
            a: 0,
            buffer: 0,
            code,
            fence,
            byte: (code & 0x00ff) as u8,
            delay: 25,
            scount: 0,
        }
    }

    fn decode_block_size(&mut self) -> usize {
        let mut n = 1usize;
        let marker = 1usize << 24;

        while n < marker {
            let z = 0x8000 + (u32::from(self.a) >> 1);
            let bit = self.decode_sub(z, None);
            n = (n << 1) | usize::from(bit);
        }

        n - marker
    }

    fn decode_sub(&mut self, mut z: u32, ctx: Option<u8>) -> u8 {
        self.ensure_code_bits();

        let mut a = u32::from(self.a);
        let mut bit = ctx.map_or(0, |ctx| ctx & 1);
        if ctx.is_some() {
            let d = 0x6000 + ((z + a) >> 2);
            z = z.min(d);
        }

        let mut code = u32::from(self.code);
        if z > code {
            bit ^= 1;
            z = 0x10000 - z;
            a += z;
            code += z;

            let shift = if a >= 0xff00 {
                first_zero_bit((a & 0x00ff) as u8) + 8
            } else {
                first_zero_bit(((a >> 8) & 0x00ff) as u8)
            };
            self.scount = self.scount.saturating_sub(shift);
            self.a = ((a << shift) & 0xffff) as u16;
            code = ((code << shift) & 0xffff) | self.buffer_bits(shift);
        } else {
            self.scount = self.scount.saturating_sub(1);
            self.a = ((z << 1) & 0xffff) as u16;
            code = ((code << 1) & 0xffff) | self.buffer_bits(1);
        }

        self.fence = if code >= 0x8000 {
            0x7fff
        } else {
            u16::try_from(code).expect("code below 0x8000 should fit in u16")
        };
        self.code = u16::try_from(code).expect("renormalized code should fit in u16");
        bit
    }

    fn ensure_code_bits(&mut self) {
        if self.scount >= 16 {
            return;
        }

        while self.scount <= 24 {
            if let Some(byte) = self.bytes.get(self.pos) {
                self.byte = *byte;
                self.pos += 1;
            } else {
                self.byte = 0xff;
                self.delay = self.delay.saturating_sub(1);
            }

            self.buffer = (self.buffer << 8) | u64::from(self.byte);
            self.scount += 8;
        }
    }

    fn buffer_bits(&self, shift: u8) -> u32 {
        u32::try_from((self.buffer >> self.scount) & ((1u64 << shift) - 1))
            .expect("requested bit buffer slice should fit in u32")
    }
}

const fn first_zero_bit(byte: u8) -> u8 {
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
    fn in_memory_decoder_reads_fixture_block_size() {
        let mut decoder = InMemoryDecoder::new(HELLO_BZZ);

        assert_eq!(decoder.decode_block_size(), HELLO_RAW.len() + 1);
    }

    #[test]
    fn in_memory_decoder_reports_incomplete_after_block_size() {
        let error =
            decode_bzz_in_memory(HELLO_BZZ).expect_err("full in-memory decode is not complete yet");

        assert!(matches!(error, BzzError::IncompleteDecoder(_)));
    }
}
