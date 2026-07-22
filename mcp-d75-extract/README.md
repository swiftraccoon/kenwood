# mcp-d75-extract

Deterministic extractor for the TH-D75 menu memory map. Parses the menu
serializers of Kenwood's official MCP-D75 programming software (as
decompiled by ILSpy) into two committed artifacts in
[`kenwood-thd75`](../thd75/): a JSON manifest of every menu write and the
generated Rust menu-field registry that powers the crate's verified menu
patching.

## What it extracts

- Every direct memory-map write of the four menu serializers: byte,
  boolean, bit-field, signed/unsigned little-endian, fixed string,
  raw-byte, and clear-range operations, in serializer order.
- Raw enum values from the nested C# enum declarations, joined to the
  combo-box display expressions and, optionally, resolved to English
  labels through the official language file.
- The seven public, statically sized repeated-record serializers, expanded
  to indexed fields with each checked base/stride formula retained.
- Audited numeric ranges and non-contiguous allowed-value domains, kept as
  separate domain objects with evidence provenance.

## Usage

Decompile and extract in one step (requires ilspycmd 10.1):

```
cargo run -p mcp-d75-extract -- \
  --assembly /path/to/MCP-D75.exe \
  --language-file /path/to/MCP-D75/Language/English.lng \
  --output thd75/data/mcp_d75_menu_schema.json \
  --rust-output thd75/src/memory/menu_fields.rs \
  --strict-known-layout
```

An existing ILSpy project directory can be used instead via
`--source-dir`. `--check` verifies that both committed outputs are current
instead of writing them. Output contains no timestamps and no
machine-local paths.

## Design

The parser is deliberately narrow: it expects ILSpy's one-line write-call
shape and discovers the obfuscated serializer classes from their public
properties. If a new MCP-D75 or ILSpy version changes that shape,
extraction fails loudly instead of silently omitting a writer, and
`--strict-known-layout` additionally pins the reviewed operation counts.

No Kenwood executable and no verbatim decompiler output is committed. The
test fixtures reproduce structural facts from the reviewed decompilation
(obfuscated identifiers, offsets, layout formulas) so the parser's
expected shape stays pinned. The constants are extracted for
interoperability with the radio's own programming format.

## Status

Rust port (July 2026) of the original extractor; output verified
byte-identical against the reviewed application before the port replaced
it.

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
