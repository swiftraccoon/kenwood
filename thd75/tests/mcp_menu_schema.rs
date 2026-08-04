// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Structural and hardware-fixture checks for the extracted MCP-D75 menu schema.
//!
//! These tests deliberately inspect the generated JSON and raw MCP image directly. They do not
//! use the legacy hand-written settings accessors, so a stale accessor cannot make an incorrect
//! schema mapping appear correct.

use std::collections::{HashMap, HashSet};

use kenwood_thd75::memory::{
    Endian, FieldCodec, FieldValue, MCP_D75_MENU_FIELDS, MCP_D75_SCHEMA_VERSION,
    MCP_D75_SOURCE_SHA256, MenuField, PatchPlanner, SchemaError, StringEncoding, menu_field,
};
use kenwood_thd75::protocol::programming::TOTAL_SIZE;
use serde_json::Value;

// Deps visible to every `kenwood-thd75` test target but unused here.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type BoxError = Box<dyn std::error::Error>;
type TestResult<T = ()> = Result<T, BoxError>;

const SCHEMA_JSON: &str = include_str!("../data/mcp_d75_menu_schema.json");

fn invalid_data(message: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

fn load_schema() -> TestResult<Value> {
    Ok(serde_json::from_str(SCHEMA_JSON)?)
}

fn required<'a>(value: &'a Value, key: &str) -> TestResult<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| invalid_data(format!("missing JSON property `{key}`")))
}

fn required_str<'a>(value: &'a Value, key: &str) -> TestResult<&'a str> {
    required(value, key)?
        .as_str()
        .ok_or_else(|| invalid_data(format!("JSON property `{key}` is not a string")))
}

fn required_usize(value: &Value, key: &str) -> TestResult<usize> {
    let number = required(value, key)?
        .as_u64()
        .ok_or_else(|| invalid_data(format!("JSON property `{key}` is not an unsigned integer")))?;
    usize::try_from(number)
        .map_err(|_| invalid_data(format!("JSON property `{key}` does not fit in usize")))
}

fn required_array<'a>(value: &'a Value, key: &str) -> TestResult<&'a [Value]> {
    required(value, key)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid_data(format!("JSON property `{key}` is not an array")))
}

fn menus(schema: &Value) -> TestResult<&[Value]> {
    required_array(schema, "menus")
}

fn operations(menu: &Value) -> TestResult<&[Value]> {
    required_array(menu, "operations")
}

fn codec(operation: &Value) -> TestResult<&Value> {
    required(operation, "codec")
}

fn find_field<'a>(
    schema: &'a Value,
    expected_menu: &str,
    expected_name: &str,
) -> TestResult<&'a Value> {
    for menu in menus(schema)? {
        if required_str(menu, "menu")? != expected_menu {
            continue;
        }
        for operation in operations(menu)? {
            if required_str(operation, "role")? == "field"
                && required_str(operation, "name")? == expected_name
            {
                return Ok(operation);
            }
        }
    }
    Err(invalid_data(format!(
        "missing public field `{expected_menu}.{expected_name}`"
    )))
}

fn find_expanded_field<'a>(
    schema: &'a Value,
    expected_menu: &str,
    expected_name: &str,
) -> TestResult<&'a Value> {
    for menu in menus(schema)? {
        if required_str(menu, "menu")? != expected_menu {
            continue;
        }
        for record in required_array(menu, "repeated_records")? {
            let Some(fields) = record.get("expanded_fields").and_then(Value::as_array) else {
                continue;
            };
            for field in fields {
                if required_str(field, "name")? == expected_name {
                    return Ok(field);
                }
            }
        }
    }
    Err(invalid_data(format!(
        "missing expanded field `{expected_menu}.{expected_name}`"
    )))
}

fn codec_span(operation: &Value) -> TestResult<usize> {
    let operation_codec = codec(operation)?;
    let kind = required_str(operation_codec, "kind")?;
    match kind {
        "byte" | "bool" => Ok(1),
        "bit_field" => {
            let bit = required_usize(operation_codec, "bit")?;
            let width = required_usize(operation_codec, "width")?;
            assert!(width > 0, "bit-field width must be non-zero");
            let end_bit = bit
                .checked_add(width)
                .ok_or_else(|| invalid_data("bit-field range overflowed usize"))?;
            assert!(
                end_bit <= u8::BITS as usize,
                "bit-field range {bit}..{end_bit} does not fit in one byte"
            );
            Ok(1)
        }
        "signed_le" | "unsigned_le" => {
            let width = required_usize(operation_codec, "width")?;
            assert!(width > 0, "integer width must be non-zero");
            assert!(
                width <= size_of::<i64>(),
                "integer width {width} exceeds the supported 64-bit range"
            );
            Ok(width)
        }
        "fixed_string" | "raw_bytes" | "clear_range" => {
            let length = required_usize(operation_codec, "length")?;
            assert!(length > 0, "{kind} length must be non-zero");
            Ok(length)
        }
        unexpected => Err(invalid_data(format!(
            "unsupported schema codec `{unexpected}`"
        ))),
    }
}

fn optional_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn required_u64(value: &Value, key: &str) -> TestResult<u64> {
    required(value, key)?
        .as_u64()
        .ok_or_else(|| invalid_data(format!("JSON property `{key}` is not an unsigned integer")))
}

fn required_i64(value: &Value, key: &str) -> TestResult<i64> {
    required(value, key)?
        .as_i64()
        .ok_or_else(|| invalid_data(format!("JSON property `{key}` is not a signed integer")))
}

fn required_u8(value: &Value, key: &str) -> TestResult<u8> {
    u8::try_from(required_u64(value, key)?)
        .map_err(|_| invalid_data(format!("JSON property `{key}` does not fit in u8")))
}

fn enum_catalog<'a>(menu: &'a Value, enum_type: &str) -> TestResult<&'a [Value]> {
    for catalog in required_array(menu, "enum_types")? {
        if required_str(catalog, "name")? == enum_type {
            return required_array(catalog, "options");
        }
    }
    Err(invalid_data(format!("missing enum catalog `{enum_type}`")))
}

/// Smallest and largest raw value of an enum catalog, mirroring the
/// generator's bounds derivation.
fn enum_bounds(menu: &Value, enum_type: &str) -> TestResult<(u64, u64)> {
    let mut bounds: Option<(u64, u64)> = None;
    for option in enum_catalog(menu, enum_type)? {
        let value = required_u64(option, "value")?;
        bounds = Some(match bounds {
            Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
            None => (value, value),
        });
    }
    bounds.ok_or_else(|| invalid_data(format!("enum catalog `{enum_type}` has no options")))
}

/// Unsigned bounds of a field's declared domain, mirroring the generator.
fn unsigned_domain_bounds(field: &Value) -> TestResult<Option<(u64, u64)>> {
    let Some(domain) = field.get("domain") else {
        return Ok(None);
    };
    match required_str(domain, "kind")? {
        "range" => Ok(Some((
            required_u64(domain, "min")?,
            required_u64(domain, "max")?,
        ))),
        "choices" => {
            let mut bounds: Option<(u64, u64)> = None;
            for value in required_array(domain, "allowed_values")? {
                let raw = value
                    .as_u64()
                    .ok_or_else(|| invalid_data("choice value is not an unsigned integer"))?;
                bounds = Some(match bounds {
                    Some((minimum, maximum)) => (minimum.min(raw), maximum.max(raw)),
                    None => (raw, raw),
                });
            }
            Ok(Some(bounds.ok_or_else(|| {
                invalid_data("choice domain has no allowed values")
            })?))
        }
        unexpected => Err(invalid_data(format!(
            "unsupported field domain kind `{unexpected}`"
        ))),
    }
}

/// Signed bounds of a field's declared domain.
fn signed_domain_bounds(field: &Value) -> TestResult<Option<(i64, i64)>> {
    let Some(domain) = field.get("domain") else {
        return Ok(None);
    };
    match required_str(domain, "kind")? {
        "range" => Ok(Some((
            required_i64(domain, "min")?,
            required_i64(domain, "max")?,
        ))),
        unexpected => Err(invalid_data(format!(
            "unsupported signed field domain kind `{unexpected}`"
        ))),
    }
}

/// Enum bounds first, then the declared domain, then the storage default:
/// the generator's resolution order for unsigned codec bounds.
fn resolved_unsigned_bounds(
    menu: &Value,
    field: &Value,
    enum_type: Option<&str>,
    default: (u64, u64),
) -> TestResult<(u64, u64)> {
    if let Some(enum_type) = enum_type {
        return enum_bounds(menu, enum_type);
    }
    Ok(unsigned_domain_bounds(field)?.unwrap_or(default))
}

fn bounds_as_u8(name: &str, bounds: (u64, u64)) -> TestResult<(u8, u8)> {
    let minimum = u8::try_from(bounds.0)
        .map_err(|_| invalid_data(format!("field `{name}` minimum does not fit in u8")))?;
    let maximum = u8::try_from(bounds.1)
        .map_err(|_| invalid_data(format!("field `{name}` maximum does not fit in u8")))?;
    Ok((minimum, maximum))
}

/// Rebuild the `FieldCodec` the generator derives from one manifest field.
fn codec_from_manifest(menu: &Value, name: &str, field: &Value) -> TestResult<FieldCodec> {
    let field_codec = codec(field)?;
    let enum_type = optional_str(field_codec, "enum_type");
    Ok(match required_str(field_codec, "kind")? {
        "byte" => {
            let bounds = resolved_unsigned_bounds(menu, field, enum_type, (0, 255))?;
            let (min, max) = bounds_as_u8(name, bounds)?;
            FieldCodec::Byte { min, max }
        }
        "bool" => FieldCodec::Bool,
        "bit_field" => {
            let bit = required_u8(field_codec, "bit")?;
            let width = required_u8(field_codec, "width")?;
            let low_bits = (1_u16 << width) - 1;
            let mask = u8::try_from(low_bits << bit)
                .map_err(|_| invalid_data(format!("field `{name}` bit range exceeds one byte")))?;
            if optional_str(field_codec, "value_type") == Some("bool") {
                FieldCodec::BitBool { mask }
            } else {
                let capacity = u64::from(low_bits);
                let bounds = resolved_unsigned_bounds(menu, field, enum_type, (0, capacity))?;
                let (min, max) = bounds_as_u8(name, bounds)?;
                FieldCodec::BitField {
                    mask,
                    shift: bit,
                    min,
                    max,
                }
            }
        }
        "fixed_string" => {
            let encoding = match required_str(field_codec, "encoding")? {
                "utf8" => StringEncoding::Utf8,
                "memory_map" => StringEncoding::MemoryMap,
                unexpected => {
                    return Err(invalid_data(format!(
                        "unsupported string encoding `{unexpected}`"
                    )));
                }
            };
            FieldCodec::FixedString {
                len: required_usize(field_codec, "length")?,
                encoding,
                padding: required_u8(field_codec, "padding")?,
            }
        }
        "unsigned_le" => {
            let width = required_u8(field_codec, "width")?;
            let default_max = if width >= 8 {
                u64::MAX
            } else {
                (1_u64 << (8 * u32::from(width))) - 1
            };
            let (min, max) = unsigned_domain_bounds(field)?.unwrap_or((0, default_max));
            FieldCodec::Unsigned {
                width,
                endian: Endian::Little,
                min,
                max,
            }
        }
        "signed_le" => {
            let width = required_u8(field_codec, "width")?;
            let defaults = if width >= 8 {
                (i64::MIN, i64::MAX)
            } else {
                let half = 1_i64 << (8 * u32::from(width) - 1);
                (-half, half - 1)
            };
            let (min, max) = signed_domain_bounds(field)?.unwrap_or(defaults);
            FieldCodec::Signed {
                width,
                endian: Endian::Little,
                min,
                max,
            }
        }
        "raw_bytes" => FieldCodec::Bytes {
            len: required_usize(field_codec, "length")?,
        },
        unexpected => {
            return Err(invalid_data(format!(
                "unsupported registry codec kind `{unexpected}`"
            )));
        }
    })
}

fn assert_options_match(menu: &Value, enum_type: &str, field: &MenuField) -> TestResult {
    let catalog = enum_catalog(menu, enum_type)?;
    let name = field.descriptor.name;
    assert_eq!(
        field.options.len(),
        catalog.len(),
        "enum option count for `{name}`"
    );
    for (actual, expected) in field.options.iter().zip(catalog) {
        assert_eq!(
            actual.raw,
            required_u64(expected, "value")?,
            "option raw value for `{name}`"
        );
        assert_eq!(
            actual.member,
            required_str(expected, "member")?,
            "option member for `{name}`"
        );
        assert_eq!(
            actual.label,
            optional_str(expected, "label"),
            "option label for `{name}`"
        );
        assert_eq!(
            actual.resource_key,
            optional_str(expected, "resource_key"),
            "option resource key for `{name}`"
        );
    }
    Ok(())
}

fn assert_allowed_values_match(json_field: &Value, field: &MenuField) -> TestResult {
    let name = field.descriptor.name;
    let choices = match json_field.get("domain") {
        Some(domain) if optional_str(domain, "kind") == Some("choices") => {
            Some(required_array(domain, "allowed_values")?)
        }
        _ => None,
    };
    if let Some(values) = choices {
        assert_eq!(
            field.allowed_values.len(),
            values.len(),
            "allowed-value count for `{name}`"
        );
        for (actual, expected) in field.allowed_values.iter().zip(values) {
            let expected = expected
                .as_u64()
                .ok_or_else(|| invalid_data("choice value is not an unsigned integer"))?;
            assert_eq!(*actual, expected, "allowed value for `{name}`");
        }
    } else {
        assert!(
            field.allowed_values.is_empty(),
            "field `{name}` must not carry allowed values"
        );
    }
    Ok(())
}

/// Compare one generated registry entry against its manifest field on every
/// dimension the write path consumes.
fn assert_registry_field_matches(
    menu: &Value,
    json_field: &Value,
    field: &MenuField,
) -> TestResult {
    let name = field.descriptor.name;
    let menu_name = required_str(menu, "menu")?;
    assert_eq!(field.menu, menu_name, "menu group for `{name}`");
    assert_eq!(
        field.descriptor.offset,
        required_usize(json_field, "offset")?,
        "offset for `{name}`"
    );
    assert_eq!(
        field.enum_type,
        optional_str(codec(json_field)?, "enum_type"),
        "enum type for `{name}`"
    );
    assert_eq!(
        field.descriptor.codec,
        codec_from_manifest(menu, name, json_field)?,
        "codec for `{name}`"
    );
    if let Some(enum_type) = field.enum_type {
        assert_options_match(menu, enum_type, field)?;
    } else {
        assert!(
            field.options.is_empty(),
            "non-enum field `{name}` must have no options"
        );
    }
    assert_allowed_values_match(json_field, field)?;
    assert_transform_matches(json_field, field)?;
    assert_eq!(
        field.is_blob,
        optional_str(json_field, "category") == Some("blob"),
        "blob flag for `{name}`"
    );
    if !field.options.is_empty() || !field.allowed_values.is_empty() {
        assert_eq!(
            field.descriptor.codec.value_kind(),
            "unsigned",
            "finite-domain field `{name}` must use an unsigned storage codec so \
             plan_value's domain gate applies"
        );
    }
    Ok(())
}

fn assert_transform_matches(json_field: &Value, field: &MenuField) -> TestResult {
    let name = field.descriptor.name;
    match (json_field.get("storage_transform"), field.storage_transform) {
        (Some(json), Some(actual)) => {
            assert_eq!(
                required_str(json, "kind")?,
                "scaled_integer",
                "storage transform kind for `{name}`"
            );
            assert_eq!(
                actual.input_unit,
                required_str(json, "input_unit")?,
                "storage transform unit for `{name}`"
            );
            assert_eq!(
                actual.numerator,
                required_i64(json, "numerator")?,
                "storage transform numerator for `{name}`"
            );
            assert_eq!(
                actual.denominator,
                required_i64(json, "denominator")?,
                "storage transform denominator for `{name}`"
            );
        }
        (None, None) => {}
        (json, registry) => {
            return Err(invalid_data(format!(
                "storage transform presence differs for `{name}`: manifest {}, registry {}",
                json.is_some(),
                registry.is_some()
            )));
        }
    }
    Ok(())
}

fn assert_pinned_summary(schema: &Value) -> TestResult {
    let summary = required(schema, "summary")?;
    let expected = [
        ("menu_count", 4, "menu"),
        ("operation_count", 267, "operation"),
        ("field_count", 257, "public field"),
        ("expanded_record_field_count", 149, "expanded record field"),
        ("total_public_field_count", 406, "total public field"),
        (
            "writable_registry_field_count",
            400,
            "writable registry field",
        ),
        ("constant_operation_count", 7, "constant"),
        ("internal_operation_count", 1, "internal operation"),
        ("clear_operation_count", 2, "clear operation"),
        ("nested_serializer_call_count", 10, "nested serializer"),
        ("repeated_record_type_count", 7, "repeated record type"),
        (
            "unsupported_public_record_type_count",
            2,
            "unsupported record type",
        ),
        ("enum_type_count", 96, "enum type"),
        ("enum_option_count", 643, "enum option"),
        ("labeled_enum_option_count", 616, "labeled enum option"),
        ("resource_enum_option_count", 349, "resource enum option"),
        ("combo_enum_type_count", 87, "combo enum type"),
        ("combo_option_mapping_count", 655, "combo option mapping"),
    ];

    assert_eq!(
        required_usize(schema, "schema_version")?,
        usize::try_from(MCP_D75_SCHEMA_VERSION)?,
        "schema version"
    );
    let source = required(schema, "source")?;
    assert_eq!(
        required_str(source, "normalized_source_sha256")?,
        MCP_D75_SOURCE_SHA256,
        "manifest and generated registry must come from the same reviewed source"
    );
    for (property, expected_count, description) in expected {
        assert_eq!(
            required_usize(summary, property)?,
            expected_count,
            "summary {description} count"
        );
    }
    Ok(())
}

#[test]
fn summary_matches_operations_and_public_names_are_unique() -> TestResult {
    let schema = load_schema()?;
    let schema_menus = menus(&schema)?;
    assert_pinned_summary(&schema)?;

    let mut menu_names = HashSet::new();
    let mut public_names = HashSet::new();
    let mut public_types = HashSet::new();
    let mut operation_count = 0usize;
    let mut field_count = 0usize;
    let mut constant_count = 0usize;
    let mut internal_count = 0usize;
    let mut clear_count = 0usize;
    let mut nested_count = 0usize;

    for menu in schema_menus {
        let menu_name = required_str(menu, "menu")?;
        assert!(
            menu_names.insert(menu_name),
            "duplicate schema menu name `{menu_name}`"
        );
        let public_type = required_str(menu, "public_name")?;
        assert!(
            public_types.insert(public_type),
            "duplicate public menu type `{public_type}`"
        );

        let menu_operations = operations(menu)?;
        let menu_fields = menu_operations
            .iter()
            .filter(|operation| operation.get("role").and_then(Value::as_str) == Some("field"))
            .count();
        assert_eq!(
            required_usize(menu, "operation_count")?,
            menu_operations.len(),
            "reported operation count for menu `{menu_name}`"
        );
        assert_eq!(
            required_usize(menu, "field_count")?,
            menu_fields,
            "reported field count for menu `{menu_name}`"
        );

        operation_count += menu_operations.len();
        nested_count += required_array(menu, "nested_serializers")?.len();
        for operation in menu_operations {
            match required_str(operation, "role")? {
                "field" => {
                    field_count += 1;
                    let field_name = required_str(operation, "name")?;
                    assert!(
                        !field_name.is_empty(),
                        "public field name must not be empty"
                    );
                    assert!(
                        public_names.insert((menu_name, field_name)),
                        "duplicate public schema field `{menu_name}.{field_name}`"
                    );
                }
                "constant" => constant_count += 1,
                "internal" => internal_count += 1,
                "clear" => clear_count += 1,
                unexpected => {
                    return Err(invalid_data(format!(
                        "unsupported operation role `{unexpected}`"
                    )));
                }
            }
        }
    }

    assert_eq!(schema_menus.len(), 4, "calculated menu count");
    assert_eq!(operation_count, 267, "calculated operation count");
    assert_eq!(field_count, 257, "calculated public field count");
    assert_eq!(constant_count, 7, "calculated constant count");
    assert_eq!(internal_count, 1, "calculated internal operation count");
    assert_eq!(clear_count, 2, "calculated clear operation count");
    assert_eq!(nested_count, 10, "calculated nested serializer count");
    Ok(())
}

#[test]
fn repeated_records_are_expanded_with_checked_offsets_and_domains() -> TestResult {
    let schema = load_schema()?;
    let anchors = [
        ("gps", "MyPositionList[4].Name", 0x11AE),
        ("aprs", "StatusTextList[4].TxRate", 0x132A),
        ("aprs", "ObjectList[2].ObjectComment", 0x1996),
        ("dv", "MyCallsignDvGatewayList[5].MemoDvGateway", 0x1CEC),
    ];
    for (menu, name, offset) in anchors {
        let field = find_expanded_field(&schema, menu, name)?;
        assert_eq!(required_usize(field, "offset")?, offset, "{menu}.{name}");
    }

    let latitude = find_expanded_field(&schema, "gps", "MyPositionList[0].LatitudeSecondEncoded")?;
    assert_eq!(required_str(codec(latitude)?, "kind")?, "unsigned_le");
    let domain = required(latitude, "domain")?;
    assert_eq!(required_usize(domain, "min")?, 0);
    assert_eq!(required_usize(domain, "max")?, 9_999);
    let transform = required(latitude, "storage_transform")?;
    assert_eq!(required_usize(transform, "numerator")?, 10_000);
    assert_eq!(required_usize(transform, "denominator")?, 60);

    let channel = find_expanded_field(&schema, "gps", "MyPositionList[0].MyPositionChannel")?;
    assert_eq!(
        channel.get("writable").and_then(Value::as_bool),
        Some(false)
    );
    let gps_blob = find_field(&schema, "radio", "GpsLogBitmap")?;
    assert_eq!(
        gps_blob.get("writable").and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[test]
fn aprs_distance_limits_include_off_and_reject_reserved_high_values() -> TestResult {
    let schema = load_schema()?;
    for short_name in ["QsyLimit", "FilterPositionLimit"] {
        let qualified = format!("aprs.{short_name}");
        let json_field = find_field(&schema, "aprs", short_name)?;
        let domain = required(json_field, "domain")?;
        assert_eq!(required_usize(domain, "min")?, 0, "{qualified} minimum");
        assert_eq!(required_usize(domain, "max")?, 250, "{qualified} maximum");
        assert_eq!(
            required_str(domain, "provenance")?,
            "ui_numeric_scaled_with_off",
            "{qualified} must retain the official raw-zero Off semantics"
        );

        let field = menu_field(&qualified)
            .ok_or_else(|| invalid_data(format!("missing registry field `{qualified}`")))?;
        assert_eq!(
            field.descriptor.codec,
            FieldCodec::Byte { min: 0, max: 250 },
            "{qualified} generated codec"
        );
        for raw in [0, 1, 250] {
            let mut planner = PatchPlanner::new();
            field.plan_value(&mut planner, FieldValue::Unsigned(raw))?;
            assert_eq!(
                planner.finish()?.pages().count(),
                1,
                "{qualified} raw {raw} should produce one page patch"
            );
        }

        let mut planner = PatchPlanner::new();
        assert!(
            matches!(
                field.plan_value(&mut planner, FieldValue::Unsigned(251)),
                Err(SchemaError::UnsignedOutOfRange {
                    value: 251,
                    min: 0,
                    max: 250,
                    ..
                })
            ),
            "{qualified} raw 251 must remain outside the write domain"
        );
    }
    Ok(())
}

#[test]
fn every_codec_range_fits_the_complete_mcp_image() -> TestResult {
    let schema = load_schema()?;

    for menu in menus(&schema)? {
        let menu_name = required_str(menu, "menu")?;
        for operation in operations(menu)? {
            let offset = required_usize(operation, "offset")?;
            let span = codec_span(operation)?;
            let end = offset.checked_add(span).ok_or_else(|| {
                invalid_data(format!(
                    "range overflow for operation in menu `{menu_name}`"
                ))
            })?;
            let offset_hex = required_str(operation, "offset_hex")?;
            assert_eq!(
                offset_hex,
                format!("0x{offset:X}"),
                "hex and numeric offsets disagree in menu `{menu_name}`"
            );
            assert!(
                offset < TOTAL_SIZE,
                "operation in menu `{menu_name}` starts outside the MCP image: 0x{offset:X}"
            );
            assert!(
                end <= TOTAL_SIZE,
                "operation in menu `{menu_name}` ends at 0x{end:X}, beyond 0x{TOTAL_SIZE:X}"
            );
        }
    }
    Ok(())
}

#[test]
fn generated_registry_exactly_matches_safe_manifest_fields() -> TestResult {
    let schema = load_schema()?;
    let mut expected = HashMap::<String, (&Value, &Value)>::new();
    let mut expanded_count = 0usize;
    let mut blocked_expanded_count = 0usize;

    for menu in menus(&schema)? {
        let menu_name = required_str(menu, "menu")?;
        for operation in operations(menu)? {
            if required_str(operation, "role")? != "field"
                || operation.get("writable").and_then(Value::as_bool) == Some(false)
            {
                continue;
            }
            let name = format!("{menu_name}.{}", required_str(operation, "name")?);
            assert!(
                expected.insert(name.clone(), (menu, operation)).is_none(),
                "duplicate writable manifest field `{name}`"
            );
        }

        for record in required_array(menu, "repeated_records")? {
            let Some(fields) = record.get("expanded_fields").and_then(Value::as_array) else {
                continue;
            };
            for field in fields {
                expanded_count += 1;
                if field.get("writable").and_then(Value::as_bool) == Some(false) {
                    blocked_expanded_count += 1;
                    continue;
                }
                let name = format!("{menu_name}.{}", required_str(field, "name")?);
                let offset = required_usize(field, "offset")?;
                let span = codec_span(field)?;
                let end = offset
                    .checked_add(span)
                    .ok_or_else(|| invalid_data(format!("expanded field `{name}` overflow")))?;
                assert!(
                    end <= TOTAL_SIZE,
                    "expanded field `{name}` exceeds the MCP image"
                );
                assert!(
                    expected.insert(name.clone(), (menu, field)).is_none(),
                    "duplicate writable expanded field `{name}`"
                );
            }
        }
    }

    assert_eq!(expanded_count, 149, "all public repeated fields cataloged");
    assert_eq!(
        blocked_expanded_count, 5,
        "unverified MyPositionChannel bytes remain blocked"
    );
    assert_eq!(expected.len(), 400, "safe writable manifest field count");
    assert_eq!(
        MCP_D75_MENU_FIELDS.len(),
        expected.len(),
        "generated Rust registry count"
    );

    for field in MCP_D75_MENU_FIELDS {
        let name = field.descriptor.name;
        let Some((menu, json_field)) = expected.remove(name) else {
            return Err(invalid_data(format!(
                "registry field `{name}` is not a writable manifest field"
            )));
        };
        assert_registry_field_matches(menu, json_field, field)?;
    }
    assert!(
        expected.is_empty(),
        "every safe manifest field must be generated"
    );
    assert!(
        !MCP_D75_MENU_FIELDS
            .iter()
            .any(|field| field.descriptor.name == "radio.GpsLogBitmap"),
        "blob crossing protected pages must not be writable"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    menu: &'static str,
    name: &'static str,
    offset: usize,
    kind: &'static str,
}

const ANCHORS: &[Anchor] = &[
    Anchor {
        menu: "radio",
        name: "BeatShift",
        offset: 0x1000,
        kind: "byte",
    },
    Anchor {
        menu: "radio",
        name: "WxAlert",
        offset: 0x1007,
        kind: "bool",
    },
    Anchor {
        menu: "radio",
        name: "ScanResumeAnalog",
        offset: 0x100C,
        kind: "byte",
    },
    Anchor {
        menu: "radio",
        name: "ScanResumeDigital",
        offset: 0x100D,
        kind: "byte",
    },
    Anchor {
        menu: "radio",
        name: "Vox",
        offset: 0x101B,
        kind: "bool",
    },
    Anchor {
        menu: "radio",
        name: "VoxGain",
        offset: 0x101C,
        kind: "byte",
    },
    Anchor {
        menu: "radio",
        name: "VoxDelay",
        offset: 0x101D,
        kind: "byte",
    },
    Anchor {
        menu: "radio",
        name: "BacklightControl",
        offset: 0x1060,
        kind: "byte",
    },
    Anchor {
        menu: "radio",
        name: "Beep",
        offset: 0x1071,
        kind: "bool",
    },
    Anchor {
        menu: "radio",
        name: "BluetoothOnOff",
        offset: 0x1078,
        kind: "bool",
    },
    Anchor {
        menu: "gps",
        name: "Interval",
        offset: 0x1110,
        kind: "unsigned_le",
    },
    Anchor {
        menu: "aprs",
        name: "MyCallsign",
        offset: 0x1200,
        kind: "fixed_string",
    },
    Anchor {
        menu: "dv",
        name: "DirectReplyTxRx",
        offset: 0x1A00,
        kind: "bool",
    },
];

#[test]
fn official_serializer_anchor_mappings_do_not_drift() -> TestResult {
    let schema = load_schema()?;
    for anchor in ANCHORS {
        let operation = find_field(&schema, anchor.menu, anchor.name)?;
        assert_eq!(
            required_usize(operation, "offset")?,
            anchor.offset,
            "official offset for {}.{}",
            anchor.menu,
            anchor.name
        );
        assert_eq!(
            required_str(codec(operation)?, "kind")?,
            anchor.kind,
            "official codec for {}.{}",
            anchor.menu,
            anchor.name
        );
    }

    let gps_interval = find_field(&schema, "gps", "Interval")?;
    assert_eq!(
        required_usize(codec(gps_interval)?, "width")?,
        2,
        "GPS interval is a two-byte unsigned little-endian value"
    );
    let aprs_callsign = find_field(&schema, "aprs", "MyCallsign")?;
    assert_eq!(
        required_usize(codec(aprs_callsign)?, "length")?,
        9,
        "APRS callsign storage width"
    );
    let dv_aprs_bit = find_field(&schema, "dv", "SentenceGpsDataTx_Aprs")?;
    assert_eq!(
        required_usize(dv_aprs_bit, "offset")?,
        0x1A0B,
        "DV APRS bit offset"
    );
    assert_eq!(
        required_usize(codec(dv_aprs_bit)?, "bit")?,
        6,
        "DV APRS bit index"
    );
    assert_eq!(
        required_usize(codec(dv_aprs_bit)?, "width")?,
        1,
        "DV APRS bit width"
    );
    Ok(())
}

fn bit_mask(operation: &Value) -> TestResult<u8> {
    let operation_codec = codec(operation)?;
    let bit = required_usize(operation_codec, "bit")?;
    let width = required_usize(operation_codec, "width")?;
    let low_bits = (1u16 << width) - 1;
    Ok(u8::try_from(low_bits << bit)?)
}

#[test]
fn constants_and_bit_fields_have_intentional_non_destructive_overlaps() -> TestResult {
    let schema = load_schema()?;
    let mut bit_occupancy: HashMap<(&str, usize), u8> = HashMap::new();
    let mut constants_checked = 0usize;
    let mut full_byte_initializers = 0usize;

    for menu in menus(&schema)? {
        let menu_name = required_str(menu, "menu")?;
        let menu_operations = operations(menu)?;

        for operation in menu_operations {
            if required_str(operation, "role")? != "field"
                || required_str(codec(operation)?, "kind")? != "bit_field"
            {
                continue;
            }
            let offset = required_usize(operation, "offset")?;
            let mask = bit_mask(operation)?;
            let occupied = bit_occupancy.entry((menu_name, offset)).or_default();
            assert_eq!(
                *occupied & mask,
                0,
                "public bit fields overlap at {menu_name} offset 0x{offset:X}"
            );
            *occupied |= mask;
        }

        for constant in menu_operations {
            if required_str(constant, "role")? != "constant" {
                continue;
            }
            constants_checked += 1;
            assert_eq!(
                required_str(codec(constant)?, "value_expression")?,
                "(byte)0",
                "bit-container constants must initialize to zero"
            );
            let offset = required_usize(constant, "offset")?;
            let overlapping_fields: Vec<&Value> = menu_operations
                .iter()
                .filter(|operation| {
                    operation.get("role").and_then(Value::as_str) == Some("field")
                        && operation.get("offset").and_then(Value::as_u64)
                            == u64::try_from(offset).ok()
                })
                .collect();
            assert!(
                !overlapping_fields.is_empty(),
                "constant at {menu_name} offset 0x{offset:X} does not initialize any fields"
            );
            assert!(
                overlapping_fields.iter().all(|operation| {
                    operation
                        .get("codec")
                        .and_then(|field_codec| field_codec.get("kind"))
                        .and_then(Value::as_str)
                        == Some("bit_field")
                }),
                "constant at {menu_name} offset 0x{offset:X} overlaps a non-bit field"
            );

            match required_str(codec(constant)?, "kind")? {
                "byte" => {
                    full_byte_initializers += 1;
                    let constant_sequence = required_usize(constant, "sequence")?;
                    for field in overlapping_fields {
                        assert!(
                            constant_sequence < required_usize(field, "sequence")?,
                            "full-byte initializer at {menu_name} offset 0x{offset:X} must precede its bit fields"
                        );
                    }
                }
                "bit_field" => {
                    let constant_mask = bit_mask(constant)?;
                    let public_mask = bit_occupancy
                        .get(&(menu_name, offset))
                        .copied()
                        .unwrap_or_default();
                    assert_eq!(
                        constant_mask & public_mask,
                        0,
                        "constant bit overlaps a public field at {menu_name} offset 0x{offset:X}"
                    );
                }
                unexpected => {
                    return Err(invalid_data(format!(
                        "constant uses unsupported codec `{unexpected}`"
                    )));
                }
            }
        }
    }

    assert_eq!(constants_checked, 7, "all extracted constants were checked");
    assert_eq!(
        full_byte_initializers, 6,
        "six bit containers use a leading full-byte zero initializer"
    );
    Ok(())
}

#[test]
fn real_memory_dump_values_agree_with_official_schema_anchors() -> TestResult {
    const EXPECTED_BYTES: &[(&str, &str, u8)] = &[
        ("radio", "BeatShift", 0x00),
        ("radio", "WxAlert", 0x00),
        ("radio", "ScanResumeAnalog", 0x01),
        ("radio", "ScanResumeDigital", 0x02),
        ("radio", "Vox", 0x00),
        ("radio", "VoxGain", 0x04),
        ("radio", "VoxDelay", 0x01),
        ("radio", "BacklightControl", 0x01),
        ("radio", "Beep", 0x01),
        ("radio", "BluetoothOnOff", 0x01),
        ("gps", "BuiltInGps", 0x00),
        ("dv", "DirectReplyTxRx", 0x01),
    ];

    let schema = load_schema()?;
    let dump = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/memory_dump.bin"
    ))?;
    assert_eq!(
        dump.len(),
        TOTAL_SIZE,
        "fixture must be a complete MCP image"
    );

    for &(menu_name, field_name, expected) in EXPECTED_BYTES {
        let operation = find_field(&schema, menu_name, field_name)?;
        let offset = required_usize(operation, "offset")?;
        let actual = dump
            .get(offset)
            .copied()
            .ok_or_else(|| invalid_data(format!("fixture lacks offset 0x{offset:X}")))?;
        assert_eq!(
            actual, expected,
            "raw fixture value for {menu_name}.{field_name} at 0x{offset:X}"
        );
    }

    let device_name = find_field(&schema, "radio", "BluetoothDeviceName")?;
    let name_offset = required_usize(device_name, "offset")?;
    let expected_name = b"TH-D75";
    let name_end = name_offset
        .checked_add(expected_name.len())
        .ok_or_else(|| invalid_data("Bluetooth device-name range overflow"))?;
    assert_eq!(
        dump.get(name_offset..name_end),
        Some(expected_name.as_slice()),
        "raw fixture Bluetooth device name at its official schema offset"
    );

    let gateway = find_expanded_field(
        &schema,
        "dv",
        "MyCallsignDvGatewayList[0].MyCallsignDvGateway",
    )?;
    let gateway_codec = codec(gateway)?;
    assert_eq!(
        required_usize(gateway_codec, "padding")?,
        32,
        "MY gateway callsigns use space padding"
    );
    let gateway_offset = required_usize(gateway, "offset")?;
    let gateway_end = gateway_offset
        .checked_add(required_usize(gateway_codec, "length")?)
        .ok_or_else(|| invalid_data("gateway callsign range overflow"))?;
    assert_eq!(
        dump.get(gateway_offset..gateway_end),
        Some(b"KQ4NIT  ".as_slice()),
        "stored MY gateway callsign is space-padded on hardware"
    );
    Ok(())
}
