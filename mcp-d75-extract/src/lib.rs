//! MCP memory-map extractor for the Kenwood TH-D75 and TM-D750.
//!
//! The extractor decompiles Kenwood's Memory Control Programs with `ILSpy`
//! and turns their serializer write methods into a deterministic JSON
//! manifest. Every write becomes an affine address (`base + stride * index`
//! over declared dimensions; the TM-D750's Programmable-Memory slots are one
//! such dimension), a codec, and a name. Obfuscated identifiers (the memory
//! writer class, the write methods, the resource singleton, per-slot detail
//! classes, record list backing fields) are discovered structurally with an
//! exactly-one-candidate check and recorded only as provenance, so a new
//! program build re-extracts without table edits.
//!
//! No Kenwood executable and no verbatim decompiler output is committed. The
//! test fixtures reproduce structural facts from the reviewed decompilations
//! (obfuscated identifiers, offsets, layout formulas) so the parser's expected
//! shape stays pinned.
//!
//! ```text
//! cargo run -p mcp-d75-extract -- extract --model thd75 \
//!   --assembly /path/to/MCP-D75.exe --mcp-version 1.03 --firmware 1.03 \
//!   --language-file /path/to/English.lng \
//!   --output thd75/data/mcp_d75_menu_schema.json \
//!   --rust-output thd75/src/memory/menu_fields.rs --strict-known-layout
//! ```
//!
//! An existing `ILSpy` project can be used without decompiling again by
//! passing `--source-dir /path/to/ilspy-output` instead of `--assembly`.
//! `--check` verifies that the committed outputs are current without writing.
//!
//! ```text
//! cargo run -p mcp-d75-extract -- diff old_manifest.json new_manifest.json
//! ```
//!
//! `diff` reports added, removed, and changed fields, record layout changes,
//! and summary deltas between two manifests of one radio; it exits 0 when
//! they are identical, 1 when differences were reported, and 2 on error.

mod address;
mod class_index;
mod cli;
mod codecs;
mod csharp;
mod diff;
mod discovery;
mod enums;
mod error;
mod extract;
mod language;
mod manifest;
pub mod model;
mod operations;
mod records;
mod rustgen;
mod sources;

pub use address::{
    Address, Affine, RecordBase, SlotSymbol, SymbolScope, Term, parse_affine, resolve_offset,
    resolve_record_base,
};
pub use cli::{
    Cli, Command, DiffArgs, ExtractArgs, main_with_args, run_diff, run_extract, write_or_check,
};
pub use diff::{DiffReport, diff_manifests};
pub use discovery::{DiscoveredMenu, DiscoveredSummary, discover_project};
pub use error::{ExtractError, Result};
pub use extract::{BuildOptions, build_manifest};
pub use manifest::{
    Anchor, Codec, Dimension, Domain, EnumCatalog, EnumOption, ExpandedField, LanguageFileInfo,
    Manifest, Menu, ModelInfo, NestedSerializer, OffsetLayout, Operation, PrivateRecord, Record,
    RecordEntry, RecordField, ReleaseInfo, Role, SCHEMA_VERSION, SourceInfo, StorageTransform,
    Summary, WriteMethodRef, json_text, offset_hex, parse_manifest,
};
pub use model::{ModelSpec, THD75, TMD750, model_by_id};
pub use rustgen::rust_text;
