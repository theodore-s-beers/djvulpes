# djvulpes

This is an early-stage experimental Rust parser, renderer, PDF converter, and inspection CLI for [DjVu files](https://en.wikipedia.org/wiki/DjVu).

The current code parses document chunks, bundled document directories, page metadata, page forms, `INCL` references, hidden text payloads, `NAVM` outlines, JB2 bitonal masks, and IW44 image layers. It includes in-house BZZ/ZP, JB2, and IW44 paths for the currently supported content, can composite supported layers to RGB bitmaps, can write PDFs with bookmarks and an invisible text layer, and can compare rendered PPM output against an oracle.

## Usage

```sh
cargo run -- summary path/to/file.djvu
cargo run -- pages path/to/file.djvu
cargo run -- page 1 path/to/file.djvu
cargo run -- render-plan 1 path/to/file.djvu
cargo run -- render-page 1 page.ppm path/to/file.djvu
cargo run -- render-page-layer 1 background background.ppm path/to/file.djvu
cargo run -- render-page-pdf 1 page.pdf path/to/file.djvu
cargo run -- render-pdf document.pdf path/to/file.djvu
cargo run -- render-pdf pages-1-5.pdf --from-page 1 --to-page 5 path/to/file.djvu
cargo run -- compare-ppm actual.ppm expected.ppm
cargo run -- compare-render-pages oracles --from-page 1 --to-page 5 path/to/file.djvu
cargo run -- compare-render-pages background-oracles --mode background --from-page 1 --to-page 5 path/to/file.djvu
cargo run -- text 1 path/to/file.djvu
cargo run -- text 1 --zones path/to/file.djvu
cargo run -- extract-text --from-page 1 --to-page 5 path/to/file.djvu
cargo run -- extract-text --structured --from-page 1 --to-page 5 path/to/file.djvu
cargo run -- outline path/to/file.djvu
```

Available subcommands:

- `summary` prints top-level document and directory information.
- `pages` lists pages with basic metadata.
- `forms` lists forms referenced by the document directory.
- `form <offset>` inspects a form at an absolute byte offset.
- `dirm` inspects the bundled document directory.
- `page <number>` inspects one page form.
- `render-plan <number>` shows the renderer-facing page chunk plan.
- `render-page <number> <output.ppm>` renders supported page layers to binary RGB PPM/P6.
- `render-page-layer <number> <full|background|foreground|mask> <output.ppm>` renders one compositor mode to binary RGB PPM/P6.
- `render-page-pdf <number> <output.pdf>` renders one page into a PDF using the same image embedding choices as `render-pdf`.
- `render-pdf <output.pdf>` renders every supported page into one PDF, printing sparse status every 50 pages by default. Use `--from-page` and `--to-page` to render a page range, `--progress` for per-page status, `--quiet` for only the final summary, `--verbose` for detailed per-page summaries, and `--timings` for aggregate stage timings. Bitonal-only pages are embedded as 1-bit grayscale images; color/IW44 pages are embedded as RGB images. In-range `NAVM` outline entries are preserved as PDF bookmarks, and hidden text lines are embedded as invisible PDF text spans with Unicode `/ActualText`.
- `compare-ppm <actual.ppm> <expected.ppm>` compares two binary RGB PPM/P6 images using the same diff summary and tolerance flags as the render comparison commands.
- `compare-render-page <number> <oracle.ppm>` renders a page and compares it with a binary RGB PPM/P6 oracle.
- `compare-render-pages <oracle-dir>` compares a page range against `page-<number>.ppm` files in an oracle directory. Use `--mode full|background|foreground|mask` to validate one compositor mode across the range.
- `compare-render-page-layer <number> <full|background|foreground|mask> <oracle.ppm>` compares one compositor mode with a binary RGB PPM/P6 oracle.
- `dump-image-layers <number> <output-dir>` writes raw `FG44`/`BG44` payloads, decoded native IW44 RGB PPMs, and decoded IW44 coefficient/reconstruction summaries.
- `inspect-iw44-pixel <number> <background|foreground> <x> <y>` maps a page-space pixel to a decoded IW44 layer sample and prints RGB plus reconstructed Y/Cb/Cr values. Use `--radius <n>` to print a small Y-neighborhood grid around the target, `--coefficients <n>` to print the strongest luma coefficients in the containing 32x32 block, `--coefficient-index <n>` to include a specific local coefficient index, `--trace-coefficients` to show how those coefficients change after each progressive IW44 chunk or slice, `--trace-events` to show the bucket, activation, sign, and refinement decisions for those coefficients, and `--trace-reconstruction` to show how zeroing listed coefficients, bands, buckets, or the containing block changes the reconstructed sample and how the sample changes under diagnostic inverse-transform order/extent variants.
- `text <number>` extracts hidden text from one page.
- `extract-text` extracts hidden text from a page range as raw `djvutxt`-compatible output. Use `--structured` for `djvused print-txt`-style zone expressions, and `--from-page`/`--to-page` to select a range.
- `outline` prints the document outline/bookmarks as `djvused`-style bookmark syntax.

## Library API

The reusable render path is exposed through:

- `render_document_page(bytes, page_number, mode)` for one 1-based page.
- `render_document_pages(bytes, from_page, to_page, mode)` for a 1-based page range, returning `RenderedDocumentPage` values that preserve the source page number.
- `render_document_pages_with_events(...)` for page-range rendering with start/rendered callbacks.
- `render_document_pdf(bytes, from_page, to_page)` for direct DjVu-to-PDF conversion with bookmarks and a searchable text layer when the source document provides them.
- `render_document_pdf_with_events(...)` for PDF conversion with page-level callbacks.
- `render_document_pdf_to_writer(...)` and `render_document_pdf_to_writer_with_events(...)` for streaming PDF conversion to an existing writer.
- `extract_document_text(bytes, from_page, to_page)` for raw hidden-text extraction with `djvutxt`-compatible page separators.
- `extract_document_text_pages(bytes, from_page, to_page)` for per-page hidden-text extraction that preserves empty pages.
- `extract_document_text_zone_pages(bytes, from_page, to_page)` for per-page hidden text plus parsed zone trees.
- `format_document_text_zones(bytes, from_page, to_page)` for `djvused print-txt`-style zone expression output.
- `extract_document_bookmarks(bytes)` for decoded `NAVM` outline/bookmark extraction.

Use `PageRenderMode::Full`, `Background`, `Foreground`, or `Mask` to select the compositor view. `PartialPageRender` contains the RGB bitmap plus decoded IW44 layer summaries and JB2 bitonal masks used to produce it.

## Requirements

- No external runtime decoder is required for the in-tree BZZ/IW44/JB2 paths currently implemented.
- `ddjvu` is useful as an optional development oracle. For example:

```sh
ddjvu -format=ppm -page=1 path/to/file.djvu oracle.ppm
cargo run -- compare-render-page 1 oracle.ppm path/to/file.djvu

mkdir -p oracles
ddjvu -format=ppm -page=1 path/to/file.djvu oracles/page-1.ppm
ddjvu -format=ppm -page=2 path/to/file.djvu oracles/page-2.ppm
cargo run -- compare-render-pages oracles --from-page 1 --to-page 2 path/to/file.djvu

mkdir -p background-oracles
ddjvu -format=ppm -mode=background -page=1 path/to/file.djvu background-oracles/page-1.ppm
cargo run -- compare-render-pages background-oracles --mode background --from-page 1 --to-page 1 path/to/file.djvu

ddjvu -format=ppm -mode=background -page=1 path/to/file.djvu background-oracle.ppm
cargo run -- compare-render-page-layer 1 background background-oracle.ppm path/to/file.djvu
```

By default, the compare commands require an exact match. Use `--max-different-pixels`, `--max-abs-delta`, `--max-delta-pixels`, and `--max-mean-abs-delta` when evaluating approximate output during decoder development.

Current fixture baseline for `Rypka-HIL.djvu`: pages 1-961 match `ddjvu` exactly in full, background, foreground, and mask modes, raw hidden-text extraction for pages 1-961 matches `djvutxt` byte-for-byte, and structured hidden-text zone output for pages 1-961 matches `djvused print-txt` byte-for-byte. The `NAVM` outline matches `djvused print-outline` byte-for-byte and is preserved as PDF bookmarks for rendered page ranges. A full pages 1-961 PDF renders through the in-house path without external decoders, preserves 389 outline destinations, and embeds searchable line-level `/ActualText` spans matching the 41,755 structured hidden-text lines. `cpdf -error-on-malformed` parses that full PDF, reports 961 pages, and emits 389 bookmark JSON entries with Unicode titles intact; sampled PDF `MediaBox` values match rendered page pixels scaled by `72 / dpi`, and no PDF `/Rotate` is emitted because Rypka's `INFO` orientation flag is `1`, DjVuLibre's right-side-up default. The source file has no `ANTa`/`ANTz` annotation chunks, no shared annotation file, no `djvused print-meta` output, and no PDF page labels are synthesized from internal DIRM filenames. `pypdf` extracts normalized text from that full PDF with zero page-level mismatches against the structured hidden text; `pdfminer.six` preserves Unicode but currently reorders text on 212 layout-heavy pages. Selected non-standard bitonal pages 68, 164, 173, 378, and 900 match exactly in full/background/foreground modes as applicable. Page 961 now matches `ddjvu` exactly in full mode and converts to PDF; the former localized IW44 background artifact at page pixel `x=1167 y=834` is covered by a regression test that checks the reconstructed BG44 luma sample and RGB output.

## Development

```sh
cargo clippy
cargo test
```
