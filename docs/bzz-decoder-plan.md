# BZZ Decoder Plan

`djvulpes` should stay MIT-licensed. The in-house BZZ decoder must therefore be developed without copying or mechanically porting GPL-covered DjVuLibre code or ExifTool's BZZ implementation.

## Constraints

- Do not copy DjVuLibre or ExifTool BZZ source code.
- Do not copy large static probability/adaptation tables from GPL-covered implementations.
- Use compressed/decompressed fixture pairs as behavioral tests.
- Keep external implementations out of runtime and test dependencies; use checked-in fixtures and local structural tests for regression coverage.
- If the decoder needs generated constants, keep the generator in this repo and document the source formula, derivation, or fixed format source.
- The obsolete provisional bit-model scaffold and its fixture-tuning diagnostics have been removed. Runtime and active tests use the local Z′ register-machine decoder.
- The old ignored alignment diagnostics for raw-bit skips, previous-`MTFNO` variants, and selector-layout variants have been removed now that active regression tests cover the resolved behavior.
- Block decoding now consumes the unary-coded pass-through `FSHIFT` field after the block length and before the MTF-number stream.
- MTF-number entropy decoding now uses the spec-shaped 262-context layout with previous-`MTFNO` selectors and tree-shaped `decode_bin` context paths. The BZZ frequency-augmented MTF update is implemented for the fixture path, and the runtime Z′ state model uses the in-repo register-machine decoder and table set.
- The local Z′ register-machine decoder verifies pass-through decoding against the fixture header and exercises the MPS/LPS update recurrence with scripted table values. The fixed Z′ tables are owned in-repo with a source note, runtime shape tests, and const generation of the 256-entry runtime arrays from the 251 valid ZP states plus unreachable padding.
- `djvu-zp` and `djvu-bzz` have been removed from dev dependencies. Runtime and test coverage now use the local Z′ decoder, owned table constants, checked-in compressed/raw fixture pairs, and local structural tests.
- Table tests capture spec-level structure separately from exact oracle equality: early/steady threshold phases, the first fresh-context jump into late estimation, and paired early-estimation probability/threshold entries.
- Rank-to-symbol reconstruction now uses BZZ's frequency-augmented MTF rotation with per-block `FSHIFT`, rather than plain move-to-front. The MTF update shifts entries in place and rescales `FADD`/`FREQ` with the BZZ right-shift rule.
- The MIT `djvu-zp` source confirms that BZZ pass-through uses `0x8000 + A/2`; `0x8000 + 3A/8` is the separate IW44 pass-through mode. The MIT `djvu-bzz` source clarified two BZZ-layer details: `FSHIFT` is unary-coded, and the initial previous `MTFNO` is `3`.
- With unary `FSHIFT`, initial previous `MTFNO = 3`, the corrected BZZ MTF update, and the local Z′ path, the in-memory decoder now reaches the fixture through the BZZ block/rank/MTF/BWT pipeline. The remaining self-containment cleanup is broader fixture coverage and, if a reliable source is found, a documented derivation for the fixed Z′ state entries rather than treating them as format constants.
- The in-memory decoder now reads blocks until the zero block-size terminator instead of stopping after the first block. Static compressed/raw fixture pairs cover empty input, one byte, repeated bytes, all byte values, and a patterned 1 KiB payload.
- `decode_bzz` now calls the in-memory Rust decoder directly; the local `bzz` command fallback has been removed from the runtime path.

## Implementation Stages

1. Decode BZZ block framing with an in-memory bit reader.
2. Implement the binary entropy model from independently derived constants.
3. Decode the rank/MTF symbol stream for one block.
4. Implement inverse Burrows-Wheeler reconstruction.
5. Decode all blocks and remove the external `bzz` fallback. Done; `decode_bzz` now uses the in-memory decoder directly.

The current fixture target is `tests/fixtures/bzz/hello.bzz`, which should decode exactly to `tests/fixtures/bzz/hello.raw`.
