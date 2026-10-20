#![doc = include_str!("../README.md")]

pub use address::{
    Address, Affine, RecordBase, SlotSymbol, SymbolScope, Term, resolve_offset, resolve_record_base,
};
pub use cli::{OutputMode, main_with_args, write_output};
pub use diff::{DiffReport, diff_manifests};
pub use discovery::{DiscoveredMenu, DiscoveredSummary, discover_project};
pub use error::{ExtractError, Result};
pub use extract::{BuildOptions, build_manifest};
pub use manifest::{
    Anchor, Codec, Dimension, Domain, EnumCatalog, EnumOption, ExpandedField, LanguageFileInfo,
    Manifest, ManifestCandidate, Menu, ModelInfo, NestedSerializer, OffsetLayout, Operation,
    PrivateRecord, Record, RecordEntry, RecordField, ReleaseInfo, Role, SCHEMA_VERSION, SourceInfo,
    StorageTransform, Summary, WriteMethodRef, offset_hex,
};
pub use model::{ModelSpec, THD75, TMD750};
pub use rustgen::rust_text;
pub use tags::{
    FieldCategory, LanguageEncoding, RadioModel, RawEncoding, RecordLayout, SourceKind,
    TextEncoding, TransformKind, ValueType,
};

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
mod output;
mod records;
mod rustgen;
mod sources;
mod tags;
mod validation;
