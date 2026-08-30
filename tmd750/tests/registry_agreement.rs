//! The generated registry agrees with the committed manifest, field by field.

use kenwood_thd75 as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use std::collections::BTreeMap;

use kenwood_tmd750::memory::{
    MCP_D750_MENU_FIELDS, MCP_D750_SCHEMA_VERSION, MCP_D750_SOURCE_SHA256, MenuField,
};
use kenwood_tmd750::types::IMAGE_LENGTH;
use mcp_d75_extract::{Codec, Manifest, RecordEntry, Role, parse_manifest};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const MANIFEST: &str = include_str!("../data/mcp_d750_menu_schema.json");

fn manifest() -> Result<Manifest, Box<dyn std::error::Error>> {
    Ok(parse_manifest(MANIFEST)?)
}

fn codec_len(codec: &Codec) -> Option<usize> {
    match codec {
        Codec::Byte { .. } | Codec::Bool { .. } | Codec::BitField { .. } => Some(1),
        Codec::FixedString { length, .. } => usize::try_from(*length).ok(),
        Codec::UnsignedLe { width, .. } | Codec::SignedLe { width, .. } => {
            usize::try_from(*width).ok()
        }
        Codec::RawBytes { length, .. } => length.and_then(|length| usize::try_from(length).ok()),
        Codec::ClearRange { .. } => None,
    }
}

struct Expected {
    offset: u64,
    strides: Vec<u64>,
    len: Option<usize>,
    option_count: usize,
}

#[test]
fn every_manifest_field_matches_its_registry_entry() -> TestResult {
    let manifest = manifest()?;
    assert_eq!(u64::from(MCP_D750_SCHEMA_VERSION), manifest.schema_version);
    assert_eq!(
        MCP_D750_SOURCE_SHA256,
        manifest.source.normalized_source_sha256
    );
    assert_eq!(u64::try_from(IMAGE_LENGTH)?, manifest.model.image_length);
    let mut expected: BTreeMap<String, Expected> = BTreeMap::new();
    for menu in &manifest.menus {
        let catalog_len = |enum_type: Option<&str>| {
            enum_type
                .and_then(|name| menu.enum_types.iter().find(|catalog| catalog.name == name))
                .map_or(0, |catalog| catalog.options.len())
        };
        for operation in &menu.operations {
            if operation.role != Role::Field || operation.writable == Some(false) {
                continue;
            }
            let name = operation.name.clone().ok_or("field without a name")?;
            let _previous = expected.insert(
                format!("{}.{name}", menu.menu),
                Expected {
                    offset: operation.offset,
                    strides: operation
                        .address
                        .terms
                        .iter()
                        .map(|term| term.stride)
                        .collect(),
                    len: codec_len(&operation.codec),
                    option_count: catalog_len(operation.codec.enum_type()),
                },
            );
        }
        for entry in &menu.repeated_records {
            let RecordEntry::Extracted(record) = entry else {
                continue;
            };
            for field in &record.expanded_fields {
                if field.writable == Some(false) {
                    continue;
                }
                let _previous = expected.insert(
                    format!("{}.{}", menu.menu, field.name),
                    Expected {
                        offset: field.offset,
                        strides: field.address.terms.iter().map(|term| term.stride).collect(),
                        len: codec_len(&field.codec),
                        option_count: catalog_len(field.codec.enum_type()),
                    },
                );
            }
        }
    }
    assert_eq!(
        u64::try_from(expected.len())?,
        manifest.summary.writable_registry_field_count
    );
    let registry: BTreeMap<&str, &MenuField> = MCP_D750_MENU_FIELDS
        .iter()
        .map(|field| (field.descriptor.name, field))
        .collect();
    assert_eq!(
        registry.len(),
        expected.len(),
        "registry and manifest field counts differ"
    );
    for (name, want) in &expected {
        let field = registry
            .get(name.as_str())
            .ok_or_else(|| format!("{name} missing from the registry"))?;
        assert_eq!(u64::from(field.descriptor.base), want.offset, "{name} base");
        let strides: Vec<u64> = field
            .descriptor
            .terms
            .iter()
            .map(|term| u64::from(term.stride))
            .collect();
        assert_eq!(strides, want.strides, "{name} terms");
        if let Some(len) = want.len {
            assert_eq!(field.descriptor.codec.encoded_len(), len, "{name} length");
        }
        assert_eq!(field.options.len(), want.option_count, "{name} options");
    }
    Ok(())
}
