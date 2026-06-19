# djvulpes

This is an early-stage experimental Rust parser and inspection CLI for [DjVu files](https://en.wikipedia.org/wiki/DjVu).

The current code focuses on structural inspection: parsing document chunks, bundled document directories, page metadata, page forms, `INCL` references, and hidden text payloads. It's not yet a full DjVu renderer or converter.

## Usage

```sh
cargo run -- summary path/to/file.djvu
cargo run -- pages path/to/file.djvu
cargo run -- page 1 path/to/file.djvu
cargo run -- text 1 path/to/file.djvu
cargo run -- text 1 --zones path/to/file.djvu
```

Available subcommands:

- `summary` prints top-level document and directory information.
- `pages` lists pages with basic metadata.
- `forms` lists forms referenced by the document directory.
- `form <offset>` inspects a form at an absolute byte offset.
- `dirm` inspects the bundled document directory.
- `page <number>` inspects one page form.
- `text <number>` extracts hidden text from one page.

## Requirements

- `bzz` on `PATH` for commands that need to decompress DjVu BZZ data, such as directory tail decoding and `TXTz` hidden text extraction

## Development

```sh
cargo clippy
cargo test
```
