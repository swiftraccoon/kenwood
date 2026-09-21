//! Complete registry metadata agrees with the committed serializer manifest.
//!
//! This compares two committed artifacts; no radio is involved.

use kenwood_schema as _;
use kenwood_transport as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use std::collections::BTreeMap;

use kenwood_tmd750::memory::{
    Endian, FieldCodec, MCP_D750_MENU_FIELDS, MCP_D750_SCHEMA_VERSION, MCP_D750_SLOT_COUNT,
    MCP_D750_SOURCE_SHA256, MenuField, StringEncoding,
};
use kenwood_tmd750::types::{IMAGE_LENGTH, SlotIndex};
use mcp_d75_extract::{
    Address, Codec, Domain, EnumOption, Manifest, Menu, RecordEntry, Role, StorageTransform,
    parse_manifest,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MANIFEST: &str = include_str!("../data/mcp_d750_menu_schema.json");

fn manifest() -> TestResult<Manifest> {
    Ok(parse_manifest(MANIFEST)?)
}

struct Expected<'a> {
    menu: &'a Menu,
    offset: u64,
    address: &'a Address,
    codec: &'a Codec,
    domain: Option<&'a Domain>,
    transform: Option<&'a StorageTransform>,
    blob: bool,
}

impl Expected<'_> {
    fn options(&self) -> TestResult<&[EnumOption]> {
        let Some(name) = self.codec.enum_type() else {
            return Ok(&[]);
        };
        self.menu
            .enum_types
            .iter()
            .find(|catalog| catalog.name == name)
            .map(|catalog| catalog.options.as_slice())
            .ok_or_else(|| format!("missing enum catalog {name}").into())
    }

    fn bounds(&self, capacity: (i128, i128)) -> TestResult<(i128, i128)> {
        let bounds = if self.codec.enum_type().is_some() {
            let options = self.options()?;
            let minimum = options.iter().map(|option| option.value).min();
            let maximum = options.iter().map(|option| option.value).max();
            (
                i128::from(minimum.ok_or("empty enum domain")?),
                i128::from(maximum.ok_or("empty enum domain")?),
            )
        } else {
            match self.domain {
                None => capacity,
                Some(Domain::Range { min, max, step, .. }) => {
                    assert_eq!(
                        *step, 1,
                        "a non-unit step requires an explicit registry representation"
                    );
                    (i128::from(*min), i128::from(*max))
                }
                Some(Domain::Choices { allowed_values, .. }) => (
                    i128::from(*allowed_values.iter().min().ok_or("empty choices")?),
                    i128::from(*allowed_values.iter().max().ok_or("empty choices")?),
                ),
            }
        };
        assert!(
            capacity.0 <= bounds.0 && bounds.0 <= bounds.1 && bounds.1 <= capacity.1,
            "manifest domain {bounds:?} must fit its storage {capacity:?}"
        );
        Ok(bounds)
    }

    fn byte_bounds(&self, maximum: u8) -> TestResult<(u8, u8)> {
        let (minimum, maximum) = self.bounds((0, i128::from(maximum)))?;
        Ok((u8::try_from(minimum)?, u8::try_from(maximum)?))
    }

    fn integer_codec(&self, width: u64, signed: bool) -> TestResult<FieldCodec> {
        assert!((1..=8).contains(&width), "integer width must fit a u64");
        let capacity = 1_i128 << (width * 8);
        let width = u8::try_from(width)?;
        if signed {
            let (minimum, maximum) = self.bounds((-capacity / 2, capacity / 2 - 1))?;
            Ok(FieldCodec::Signed {
                width,
                endian: Endian::Little,
                min: i64::try_from(minimum)?,
                max: i64::try_from(maximum)?,
            })
        } else {
            let (minimum, maximum) = self.bounds((0, capacity - 1))?;
            Ok(FieldCodec::Unsigned {
                width,
                endian: Endian::Little,
                min: u64::try_from(minimum)?,
                max: u64::try_from(maximum)?,
            })
        }
    }

    fn complete_codec(&self) -> TestResult<FieldCodec> {
        Ok(match self.codec {
            Codec::Byte { .. } => {
                let (min, max) = self.byte_bounds(u8::MAX)?;
                FieldCodec::Byte { min, max }
            }
            Codec::Bool { .. } => FieldCodec::Bool,
            Codec::BitField {
                bit,
                width,
                value_type,
                ..
            } => {
                assert!(
                    *bit < 8 && *width >= 1 && *width <= 8 - *bit,
                    "bit fields must remain within one byte"
                );
                let capacity = u8::try_from((1_u16 << *width) - 1)?;
                let mask = capacity << *bit;
                if value_type == "bool" {
                    assert_eq!(*width, 1, "a bit boolean owns exactly one bit");
                    FieldCodec::BitBool { mask }
                } else {
                    let (min, max) = self.byte_bounds(capacity)?;
                    FieldCodec::BitField {
                        mask,
                        shift: u8::try_from(*bit)?,
                        min,
                        max,
                    }
                }
            }
            Codec::UnsignedLe { width, .. } => self.integer_codec(*width, false)?,
            Codec::SignedLe { width, .. } => self.integer_codec(*width, true)?,
            Codec::FixedString {
                encoding,
                length,
                padding,
                ..
            } => {
                let encoding = match encoding.as_str() {
                    "utf8" => StringEncoding::Utf8,
                    "memory_map" => StringEncoding::MemoryMap,
                    other => return Err(format!("unsupported text encoding {other}").into()),
                };
                FieldCodec::FixedString {
                    len: usize::try_from(*length)?,
                    encoding,
                    padding: u8::try_from(*padding)?,
                }
            }
            Codec::RawBytes { length, .. } => FieldCodec::Bytes {
                len: usize::try_from(length.ok_or("raw-byte length missing")?)?,
            },
            Codec::ClearRange { .. } => {
                return Err("a clear operation must not become a registry field".into());
            }
        })
    }
}

fn expected_fields(manifest: &Manifest) -> TestResult<BTreeMap<String, Expected<'_>>> {
    let mut expected = BTreeMap::new();
    for menu in &manifest.menus {
        for operation in &menu.operations {
            if operation.role != Role::Field || operation.writable == Some(false) {
                continue;
            }
            let name = operation.name.as_deref().ok_or("field without a name")?;
            let previous = expected.insert(
                format!("{}.{name}", menu.menu),
                Expected {
                    menu,
                    offset: operation.offset,
                    address: &operation.address,
                    codec: &operation.codec,
                    domain: operation.domain.as_ref(),
                    transform: None,
                    blob: operation.category.as_deref() == Some("blob"),
                },
            );
            assert!(previous.is_none(), "duplicate manifest field {name}");
        }
        for entry in &menu.repeated_records {
            let RecordEntry::Extracted(record) = entry else {
                continue;
            };
            for field in &record.expanded_fields {
                if field.writable == Some(false) {
                    continue;
                }
                let previous = expected.insert(
                    format!("{}.{}", menu.menu, field.name),
                    Expected {
                        menu,
                        offset: field.offset,
                        address: &field.address,
                        codec: &field.codec,
                        domain: field.domain.as_ref(),
                        transform: field.storage_transform.as_ref(),
                        blob: false,
                    },
                );
                assert!(
                    previous.is_none(),
                    "duplicate manifest field {}",
                    field.name
                );
            }
        }
    }
    Ok(expected)
}

fn assert_options(name: &str, field: &MenuField, expected: &Expected<'_>) -> TestResult {
    let options = expected.options()?;
    assert_eq!(
        field.enum_type,
        expected.codec.enum_type(),
        "{name} enum type"
    );
    assert_eq!(field.options.len(), options.len(), "{name} option count");
    for (actual, expected) in field.options.iter().zip(options) {
        assert_eq!(
            actual.raw,
            u64::try_from(expected.value)?,
            "{name} option raw"
        );
        assert_eq!(actual.member, expected.member, "{name} option member");
        assert_eq!(
            actual.label,
            expected.label.as_deref(),
            "{name} option label"
        );
        assert_eq!(
            actual.resource_key,
            expected.resource_key.as_deref(),
            "{name} option resource"
        );
    }
    let choices = match expected.domain {
        Some(Domain::Choices { allowed_values, .. }) => allowed_values
            .iter()
            .copied()
            .map(u64::try_from)
            .collect::<Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    assert_eq!(
        field.allowed_values, choices,
        "{name} exact allowed choices"
    );
    Ok(())
}

fn assert_field(name: &str, field: &MenuField, expected: &Expected<'_>) -> TestResult {
    assert_eq!(field.menu, expected.menu.menu, "{name} menu group");
    assert_eq!(field.descriptor.name, name, "qualified descriptor name");
    assert_eq!(
        expected.offset, expected.address.base,
        "{name} manifest address"
    );
    assert_eq!(
        u64::from(field.descriptor.base),
        expected.offset,
        "{name} base"
    );
    let terms: Vec<(&str, u64)> = field
        .descriptor
        .terms
        .iter()
        .map(|term| (term.dimension, u64::from(term.stride)))
        .collect();
    let expected_terms: Vec<(&str, u64)> = expected
        .address
        .terms
        .iter()
        .map(|term| (term.dimension.as_str(), term.stride))
        .collect();
    assert_eq!(
        terms, expected_terms,
        "{name} complete ordered address terms"
    );
    assert_eq!(
        field.descriptor.codec,
        expected.complete_codec()?,
        "{name} complete codec, bounds, byte order, encoding, and padding"
    );
    assert_options(name, field, expected)?;
    let transform = field.storage_transform.map(|transform| {
        (
            transform.input_unit,
            transform.numerator,
            transform.denominator,
        )
    });
    let expected_transform = expected.transform.map(|transform| {
        assert_eq!(
            transform.kind, "scaled_integer",
            "{name} storage transform kind"
        );
        (
            transform.input_unit.as_str(),
            transform.numerator,
            transform.denominator,
        )
    });
    assert_eq!(
        transform, expected_transform,
        "{name} exact storage transform"
    );
    assert_eq!(
        field.is_blob, expected.blob,
        "{name} blob exclusion metadata"
    );
    Ok(())
}

#[test]
fn every_registered_codec_and_slot_address_passes_structural_validation() -> TestResult {
    for field in MCP_D750_MENU_FIELDS {
        let descriptor = &field.descriptor;
        if descriptor.is_per_slot() {
            for index in 0..MCP_D750_SLOT_COUNT {
                let address = descriptor.address(Some(SlotIndex::new(index)?));
                assert!(
                    address.is_ok(),
                    "registered field {} in slot {index} must remain structurally valid: {address:?}",
                    descriptor.name
                );
            }
        } else {
            let address = descriptor.address(None);
            assert!(
                address.is_ok(),
                "registered global field {} must remain structurally valid: {address:?}",
                descriptor.name
            );
        }
    }
    Ok(())
}

#[test]
fn every_manifest_field_matches_its_registry_entry() -> TestResult {
    let manifest = manifest()?;
    assert_eq!(
        u64::from(MCP_D750_SCHEMA_VERSION),
        manifest.schema_version,
        "registry schema version must match the manifest"
    );
    assert_eq!(
        MCP_D750_SOURCE_SHA256, manifest.source.normalized_source_sha256,
        "registry provenance must match the reviewed source manifest"
    );
    assert_eq!(
        u64::try_from(IMAGE_LENGTH)?,
        manifest.model.image_length,
        "registry and manifest image lengths must match"
    );
    let expected = expected_fields(&manifest)?;
    assert_eq!(
        u64::try_from(expected.len())?,
        manifest.summary.writable_registry_field_count,
        "all and only writable manifest fields must be represented"
    );
    let registry: BTreeMap<&str, &MenuField> = MCP_D750_MENU_FIELDS
        .iter()
        .map(|field| (field.descriptor.name, field))
        .collect();
    assert_eq!(
        registry.len(),
        MCP_D750_MENU_FIELDS.len(),
        "generated descriptor names must be unique"
    );
    assert_eq!(
        registry.len(),
        expected.len(),
        "registry and manifest field counts differ"
    );
    for (name, want) in &expected {
        let field = registry
            .get(name.as_str())
            .ok_or_else(|| format!("{name} missing from the registry"))?;
        assert_field(name, field, want)?;
    }
    Ok(())
}
