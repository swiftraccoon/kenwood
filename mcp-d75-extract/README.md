# mcp-d75-extract

Extracts the MCP memory-map layout of Kenwood's TH-D75 and TM-D750 from the
official Memory Control Programs (MCP-D75, MCP-D750) into a deterministic JSON
manifest, and diffs manifests across firmware releases. It also generates the
Rust menu-field registries that power the verified menu patching in
[`kenwood-thd75`](https://github.com/swiftraccoon/kenwood/tree/main/thd75)
and [`kenwood-tmd750`](https://github.com/swiftraccoon/kenwood/tree/main/tmd750).

The programs are .NET applications. `ilspycmd` decompiles them; the extractor
reads the serializer write methods and records every write as a name, an
on-image codec, and an affine address: `base + stride * index` over declared
dimensions. The TM-D750 stores its menu data once per Programmable-Memory
slot, so its fields carry a `pm_slot` term (six slots, 8192 bytes apart; the
power-on bitmap uses a 256000-byte stride); the TH-D75 has no dimensions and
every address is absolute.

Obfuscated identifiers (the memory writer class, the write methods, the
language-resource singleton, per-slot detail classes, record list backing
fields) are discovered structurally with an exactly-one-candidate check and
recorded only as provenance, so a new program build re-extracts without table
edits. The per-radio specs pin only stable facts: public container property
names, dimension anchors, record list names and reviewed counts, audited value
domains.

## Usage

```text
mcp-d75-extract extract --model thd75 \
  --assembly /path/to/MCP-D75.exe --mcp-version 1.03 --firmware 1.03 \
  --language-file /path/to/English.lng \
  --output thd75/data/mcp_d75_menu_schema.json \
  --rust-output thd75/src/memory/menu_fields.rs --strict-known-layout

mcp-d75-extract extract --model tmd750 \
  --assembly /path/to/mcp_d750.exe --mcp-version 1.00 --firmware 1.00 \
  --language-file /path/to/english.lng \
  --output tmd750/data/mcp_d750_menu_schema.json \
  --rust-output tmd750/src/memory/menu_fields.rs --strict-known-layout

mcp-d75-extract diff old.json new.json
```

Pass `--source-dir` with an existing `ILSpy` project instead of `--assembly`.
`--check` verifies that the committed outputs are current without writing.
`--strict-known-layout` additionally requires the spec's reviewed per-menu
operation counts and combo-box mapping counts. The MCP release version and
the firmware target are declared on the command line because the assembly
records neither; the assembly version and product name are read from the
decompiled `Properties/AssemblyInfo.cs`, and the product must match the
model.

`diff` prints added, removed, and changed fields (address, codec, domain,
writability, enum options), record layout changes, and summary deltas. It
exits 0 when the manifests are identical, 1 when differences were reported,
and 2 on error. It refuses manifests of different radios.

## Manifest (schema version 4)

Top level: `schema_version`, `generator`, `model` (radio, product, image
length), `release` (MCP version, assembly version, firmware target),
`dimensions`, `source` (normalized source digest and the discovered
provenance), `summary`, and `menus`. Each menu lists its direct writes in
serializer-then-detail order; every write and every expanded record field has
`offset`/`offset_hex` (the all-indices-zero address) and `address {base,
terms}`. Record lists keep their base layout (`linear`,
`linear_with_override`) and inherit the slot term; private sub-writers that
expose no public properties are cataloged, not extracted. Enum-valued codecs
name their enum's declaring class, and each menu carries the catalogs of the
enums it uses, joined to the combo-box labels of the language file.

The generated Rust registries differ per radio: the TH-D75 registry requires
absolute addresses and refuses a manifest with dimension terms, while the
TM-D750 registry carries each field's `pm_slot` term together with the image
length and slot constants read from the manifest's dimension anchor.

## Failure policy

Extraction fails, naming the class, method, and line, whenever an offset does
not resolve, a discovery has zero or several candidates, a pinned fact does
not verify against the source, a nested call reaches a class the spec does
not describe, or a written value has no resolvable type. There are no
warnings and no extraction output is published before extraction and rendering
both complete. Decompilation is synchronous and waits for the child process;
its unsuccessful exit is `ExtractError::Subprocess`.

Writing prepares every output in a temporary file in its destination directory
and syncs the file contents. Each final replacement is atomic, in JSON-then-Rust
order. `ExtractError::Publication` retains the destinations already replaced if
a later replacement fails. Retry the same request to complete the pair. Parent
directories may be created during preparation; this operation does not promise
directory-entry durability or an atomic transaction across both files. Check
mode performs no writes.

## Library workflows

Resolve artificial banked addresses without reading source files:

```rust
use std::collections::HashMap;
use mcp_d75_extract::{SlotSymbol, SymbolScope, resolve_offset};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let constants = HashMap::new();
let slots = [SlotSymbol {
    symbol: "bank_offset".to_owned(),
    anchor: "BankAddress".to_owned(),
    dimension: "bank".to_owned(),
    stride: 256,
}];
let scope = SymbolScope { constants: &constants, slots: &slots, overrides: &[] };
let address = resolve_offset("0x1000 + bank_offset", &scope)?;
assert_eq!(address.offset(&[("bank", 3)])?, 0x1300);
# Ok(())
# }
```

Decode complete checked manifests, inspect their fields, compare releases and
render a registry. Raw `ManifestCandidate` records are editable inputs;
`validate` checks their cross-record relationships before producing `Manifest`.
Its accessors borrow immutable views. `into_candidate` transfers allocations for
editing; `to_candidate` creates an independent copy. Both require validation
again before generation or comparison.
`Manifest::from_json`, `FromStr` and Serde decoding enforce the same boundary:
model identity, dimensions, source-map agreement, codec widths, domains, record
expansions and retained derived counts. Closed enums retain the schema's wire
spellings. The two combo-box counters retain extraction provenance because the
manifest does not store every source expression needed to recompute them.
`BuildOptions::new` checks version labels before source I/O; named methods select
the optional language file and strict-layout checks.

```rust,no_run
use mcp_d75_extract::{Manifest, OutputMode, diff_manifests, rust_text, write_output};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let old = Manifest::from_json(&std::fs::read_to_string("old.json")?)?;
let current: Manifest = std::fs::read_to_string("current.json")?.parse()?;
let report = diff_manifests(&old, &current)?;
println!("{report}");
for menu in current.menus() {
    println!("{}: {} fields", menu.menu, menu.field_count);
}
write_output(std::path::Path::new("registry.rs"), &rust_text(&current)?, OutputMode::Replace)?;
let json = current.to_json()?;
assert_eq!(Manifest::from_json(&json)?, current);
# Ok(())
# }
```

## Tests

`tests/fixtures/thd75/` and `tests/fixtures/tmd750/` are small `ILSpy`-shaped
projects that reproduce the structural facts the extractor depends on. They
are pinned inputs: do not regenerate or reformat them. No Kenwood executable
and no verbatim decompiler output is committed. The constants are extracted
for interoperability with the radios' own programming format.

Part of the [kenwood](https://github.com/swiftraccoon/kenwood) workspace.
License: GPL-2.0-or-later.
