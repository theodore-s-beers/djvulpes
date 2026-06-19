# BZZ Decoder Plan

`djvulpes` should stay MIT-licensed. The in-house BZZ decoder must therefore be developed without copying or mechanically porting GPL-covered DjVuLibre code or ExifTool's BZZ implementation.

## Constraints

- Do not copy DjVuLibre or ExifTool BZZ source code.
- Do not copy large static probability/adaptation tables from GPL-covered implementations.
- Use compressed/decompressed fixture pairs as behavioral tests.
- Use the local `bzz` command only as a temporary test oracle and fallback.
- If the decoder needs generated constants, keep the generator in this repo and document the source formula or derivation.

## Implementation Stages

1. Decode BZZ block framing with an in-memory bit reader.
2. Implement the binary entropy model from independently derived constants.
3. Decode the rank/MTF symbol stream for one block.
4. Implement inverse Burrows-Wheeler reconstruction.
5. Decode all blocks and remove the external `bzz` fallback.

The current fixture target is `tests/fixtures/bzz/hello.bzz`, which should decode exactly to `tests/fixtures/bzz/hello.raw`.
