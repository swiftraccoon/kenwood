//! MCP-D75 menu schema extractor.
//!
//! This tool turns the four official MCP-D75 menu serializer write methods
//! into a deterministic JSON manifest. It extracts all direct memory-map
//! writes from `RadioMenuData`, `GpsMenuData`, `AprsMenuData`, and
//! `DvMenuData`, including byte, boolean, bit-field, signed/unsigned
//! little-endian, fixed string, raw-byte, and clear-range operations. It
//! derives raw enum values from the nested C# enum declarations, joins them
//! to combo-box display resources, and can resolve those resources through
//! Kenwood's English language file. It follows the seven public, statically
//! sized repeated-record serializers and expands their indexed fields while
//! retaining each checked base/stride formula in the manifest.
//!
//! No Kenwood executable and no verbatim decompiler output is committed.
//! The test fixtures and inline test sources reproduce structural facts from
//! the reviewed decompilation (obfuscated identifiers, offsets, and layout
//! formulas) so the parser's expected shape stays pinned. To decompile and
//! extract in one step, install ilspycmd 10.1 (the reviewed output used
//! 10.1.0.8386), then:
//!
//! ```text
//! cargo run -p mcp-d75-extract -- \
//!   --assembly /path/to/MCP-D75.exe \
//!   --language-file /path/to/MCP-D75/Language/English.lng \
//!   --output thd75/data/mcp_d75_menu_schema.json \
//!   --rust-output thd75/src/memory/menu_fields.rs \
//!   --strict-known-layout
//! ```
//!
//! An existing `ILSpy` project can be used without decompiling again by
//! passing `--source-dir /path/to/ilspy-output` instead of `--assembly`.
//! The optional `--rust-output` emits crate-native
//! `FieldDescriptor`/`FieldCodec` values, enum option metadata, exact
//! audited numeric/choice domains, repeated-record fields, source
//! provenance constants, and a case-insensitive `menu.name` lookup. It
//! consumes only public operations that are verified safe for sparse radio
//! writes. Use `--check` with the same arguments to verify that both
//! committed outputs are current. Output contains no timestamp or
//! machine-local absolute paths.
//!
//! The parser is deliberately narrow. It expects `ILSpy`'s one-line `A_0`
//! method calls and discovers obfuscated class names from the public
//! `MemoryMap` properties. If `ILSpy` changes that shape, extraction fails
//! instead of silently omitting a direct writer. `--strict-known-layout`
//! additionally detects a changed operation count.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

mod cli;
mod codecs;
mod csharp;
mod error;
mod language;
mod records;
mod rustgen;
mod schema;
mod sources;
mod tables;
mod value;

pub use cli::{Cli, main_with_args, run, write_or_check};
pub use error::{ExtractError, Result};
pub use rustgen::rust_text;
pub use schema::{BuildOptions, build_schema, json_text};
pub use tables::RecordSpec;

/// Extract and expand one statically sized public child serializer.
///
/// This is the library face of the repeated-record extraction used by the
/// integration tests; `source_dir` only affects the recorded relative
/// source path.
///
/// # Errors
///
/// Returns an error when the writer's shape deviates from the reviewed
/// decompilation: a missing or non-linear base-offset formula, an
/// unsupported writer call, or an unresolved offset or length.
pub fn extract_repeated_record<S: std::hash::BuildHasher>(
    spec: &RecordSpec,
    path: &Path,
    source: &str,
    source_dir: &Path,
    constants: &HashMap<String, i64, S>,
) -> Result<Value> {
    let patterns = csharp::Patterns::new()?;
    let constants: HashMap<String, i64> = constants
        .iter()
        .map(|(name, value)| (name.clone(), *value))
        .collect();
    records::extract_repeated_record_with(&patterns, spec, path, source, source_dir, &constants)
}
