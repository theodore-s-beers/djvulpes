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
    decode_bzz_with_local_tool(bytes)
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
}
