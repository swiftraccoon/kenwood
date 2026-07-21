//! Rendering of the manifest's writable fields as crate-native Rust descriptors.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::error::{Result, extract_error};
use crate::tables::GENERATOR;
use crate::value::{display_name, req, req_array, req_i64, req_str};

/// Encode a string as a deterministic Rust string literal.
pub(crate) fn rust_string(value: &str) -> String {
    let mut escaped = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ if (character as u32) < 0x20 || (character as u32) == 0x7F => {
                let _infallible = write!(escaped, "\\u{{{:X}}}", character as u32);
            }
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

/// Render an optional string as a Rust `Option` literal.
pub(crate) fn rust_option_string(value: Option<&str>) -> String {
    value.map_or_else(
        || "None".to_owned(),
        |text| format!("Some({})", rust_string(text)),
    )
}

/// Index enum catalogs by qualified name across all menus.
fn enum_catalogs(schema: &Value) -> Result<HashMap<String, &Value>> {
    let mut catalogs = HashMap::new();
    for menu in req_array(schema, "menus")? {
        for catalog in req_array(menu, "enum_types")? {
            let name = req_str(catalog, "name")?.to_owned();
            if catalogs.contains_key(&name) {
                return Err(extract_error!("duplicate qualified enum catalog: {name}"));
            }
            let _previous = catalogs.insert(name, catalog);
        }
    }
    Ok(catalogs)
}

/// Rust-literal min/max bounds for a little-endian integer width.
fn rust_integer_bounds(width: i64, signed: bool) -> Result<(String, String)> {
    if !(1..=8).contains(&width) {
        return Err(extract_error!(
            "integer field width must be in 1..=8, got {width}"
        ));
    }
    let bits = width * 8;
    if signed {
        if bits == 64 {
            return Ok(("i64::MIN".to_owned(), "i64::MAX".to_owned()));
        }
        let magnitude: i64 = 1_i64 << (bits - 1);
        return Ok(((-magnitude).to_string(), (magnitude - 1).to_string()));
    }
    if bits == 64 {
        return Ok(("0".to_owned(), "u64::MAX".to_owned()));
    }
    Ok(("0".to_owned(), ((1_i64 << bits) - 1).to_string()))
}

/// Min/max raw enum values, checked against the storage capacity.
fn raw_enum_bounds(
    enum_type: &str,
    catalogs: &HashMap<String, &Value>,
    maximum: i64,
) -> Result<(i64, i64)> {
    let catalog = catalogs
        .get(enum_type)
        .ok_or_else(|| extract_error!("field references missing enum catalog: {enum_type}"))?;
    let options = req_array(catalog, "options")?;
    if options.is_empty() {
        return Err(extract_error!("enum catalog has no options: {enum_type}"));
    }
    let mut values = Vec::new();
    for option in options {
        let value = option.get("value").and_then(Value::as_i64);
        match value {
            Some(value) if (0..=maximum).contains(&value) => values.push(value),
            _ => {
                return Err(extract_error!(
                    "enum {enum_type} contains a value outside its 0..={maximum} storage domain"
                ));
            }
        }
    }
    let minimum = values.iter().min().copied().unwrap_or_default();
    let maximum = values.iter().max().copied().unwrap_or_default();
    Ok((minimum, maximum))
}

/// Inclusive `(min, max)` of a range or choices domain, if any.
fn domain_bounds(domain: Option<&Value>) -> Result<Option<(i64, i64)>> {
    let Some(domain) = domain else {
        return Ok(None);
    };
    match req_str(domain, "kind")? {
        "range" => Ok(Some((req_i64(domain, "min")?, req_i64(domain, "max")?))),
        "choices" => {
            let values: Vec<i64> = req_array(domain, "allowed_values")?
                .iter()
                .filter_map(Value::as_i64)
                .collect();
            if values.is_empty() {
                return Err(extract_error!("choice domain has no allowed values"));
            }
            let minimum = values.iter().min().copied().unwrap_or_default();
            let maximum = values.iter().max().copied().unwrap_or_default();
            Ok(Some((minimum, maximum)))
        }
        _ => Err(extract_error!("unsupported field domain: {domain}")),
    }
}

/// Render a bit-field codec: `BitBool` for boolean values, `BitField` else.
fn bit_field_codec_lines(
    codec: &Value,
    catalogs: &HashMap<String, &Value>,
    domain: Option<&Value>,
    enum_type: Option<&str>,
) -> Result<Vec<String>> {
    let bit = codec.get("bit").and_then(Value::as_i64);
    let width = codec.get("width").and_then(Value::as_i64);
    let (Some(bit), Some(width)) = (bit, width) else {
        return Err(extract_error!(
            "invalid bit field coordinates: bit={:?}, width={:?}",
            codec.get("bit"),
            codec.get("width")
        ));
    };
    if width < 1 || bit < 0 {
        return Err(extract_error!(
            "invalid bit field coordinates: bit={bit}, width={width}"
        ));
    }
    if bit + width > 8 {
        return Err(extract_error!(
            "bit field exceeds one byte: bit={bit}, width={width}"
        ));
    }
    let mask = ((1_i64 << width) - 1) << bit;
    if codec.get("value_type").and_then(Value::as_str) == Some("bool") {
        if width != 1 {
            return Err(extract_error!(
                "boolean bit field must have width 1, got {width}"
            ));
        }
        return Ok(vec![
            "FieldCodec::BitBool {".to_owned(),
            format!("    mask: 0x{mask:02X},"),
            "}".to_owned(),
        ]);
    }
    let capacity = (1_i64 << width) - 1;
    let (minimum, maximum) = match enum_type {
        Some(enum_type) => raw_enum_bounds(enum_type, catalogs, capacity)?,
        None => domain_bounds(domain)?.unwrap_or((0, capacity)),
    };
    if !(0 <= minimum && minimum <= maximum && maximum <= capacity) {
        return Err(extract_error!(
            "bit-field domain {minimum}..={maximum} exceeds capacity {capacity}"
        ));
    }
    Ok(vec![
        "FieldCodec::BitField {".to_owned(),
        format!("    mask: 0x{mask:02X},"),
        format!("    shift: {bit},"),
        format!("    min: {minimum},"),
        format!("    max: {maximum},"),
        "}".to_owned(),
    ])
}

/// Render a little-endian integer codec, validating any audited domain
/// against the storage width.
fn integer_codec_lines(codec: &Value, domain: Option<&Value>, signed: bool) -> Result<Vec<String>> {
    let width = req_i64(codec, "width")?;
    let (default_minimum, default_maximum) = rust_integer_bounds(width, signed)?;
    let bounds = domain_bounds(domain)?;
    let (minimum, maximum) = match bounds {
        Some((minimum, maximum)) => {
            let parsed_minimum: i128 = if default_minimum == "i64::MIN" {
                i128::from(i64::MIN)
            } else {
                default_minimum.parse().unwrap_or_default()
            };
            let parsed_maximum: i128 = match default_maximum.as_str() {
                "i64::MAX" => i128::from(i64::MAX),
                "u64::MAX" => i128::from(u64::MAX),
                text => text.parse().unwrap_or_default(),
            };
            let within = parsed_minimum <= i128::from(minimum)
                && minimum <= maximum
                && i128::from(maximum) <= parsed_maximum;
            if !within {
                return Err(extract_error!(
                    "integer domain {minimum}..={maximum} exceeds {width}-byte storage"
                ));
            }
            (minimum.to_string(), maximum.to_string())
        }
        None => (default_minimum, default_maximum),
    };
    let variant = if signed { "Signed" } else { "Unsigned" };
    Ok(vec![
        format!("FieldCodec::{variant} {{"),
        format!("    width: {width},"),
        "    endian: Endian::Little,".to_owned(),
        format!("    min: {minimum},"),
        format!("    max: {maximum},"),
        "}".to_owned(),
    ])
}

/// Render one manifest codec using the crate's real `FieldCodec` type.
fn rust_codec(
    codec: &Value,
    catalogs: &HashMap<String, &Value>,
    domain: Option<&Value>,
) -> Result<Vec<String>> {
    let kind = req_str(codec, "kind")?;
    let enum_type = codec.get("enum_type").and_then(Value::as_str);
    match kind {
        "byte" => {
            let bounds = domain_bounds(domain)?;
            let (minimum, maximum) = match enum_type {
                Some(enum_type) => raw_enum_bounds(enum_type, catalogs, 255)?,
                None => bounds.unwrap_or((0, 255)),
            };
            if !(0 <= minimum && minimum <= maximum && maximum <= 255) {
                return Err(extract_error!(
                    "byte domain {minimum}..={maximum} exceeds storage"
                ));
            }
            Ok(vec![
                "FieldCodec::Byte {".to_owned(),
                format!("    min: {minimum},"),
                format!("    max: {maximum},"),
                "}".to_owned(),
            ])
        }
        "bool" => Ok(vec!["FieldCodec::Bool".to_owned()]),
        "bit_field" => bit_field_codec_lines(codec, catalogs, domain, enum_type),
        "fixed_string" => {
            let encoding = match req_str(codec, "encoding")? {
                "utf8" => "StringEncoding::Utf8",
                "memory_map" => "StringEncoding::MemoryMap",
                other => return Err(extract_error!("unsupported string encoding: {other}")),
            };
            Ok(vec![
                "FieldCodec::FixedString {".to_owned(),
                format!("    len: {},", req_i64(codec, "length")?),
                format!("    encoding: {encoding},"),
                format!("    padding: {},", req_i64(codec, "padding")?),
                "}".to_owned(),
            ])
        }
        "signed_le" | "unsigned_le" => integer_codec_lines(codec, domain, kind == "signed_le"),
        "raw_bytes" => {
            let length = codec
                .get("length")
                .and_then(Value::as_i64)
                .ok_or_else(|| extract_error!("raw byte field has no inferred length"))?;
            Ok(vec![
                "FieldCodec::Bytes {".to_owned(),
                format!("    len: {length},"),
                "}".to_owned(),
            ])
        }
        other => Err(extract_error!("cannot render field codec kind: {other}")),
    }
}

/// Yield writable direct and expanded fields with their containing menu.
fn writable_manifest_fields(schema: &Value) -> Result<Vec<(&Value, &Value)>> {
    let mut fields = Vec::new();
    for menu in req_array(schema, "menus")? {
        for operation in req_array(menu, "operations")? {
            let writable = operation
                .get("writable")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if req_str(operation, "role")? == "field" && writable {
                fields.push((menu, operation));
            }
        }
        for record in req_array(menu, "repeated_records")? {
            let Some(expanded) = record.get("expanded_fields").and_then(Value::as_array) else {
                continue;
            };
            for field in expanded {
                let writable = field
                    .get("writable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if writable {
                    fields.push((menu, field));
                }
            }
        }
    }
    Ok(fields)
}

/// Render a field's storage transform as a Rust `Option` literal.
fn rust_storage_transform(field: &Value) -> Result<String> {
    let Some(transform) = field.get("storage_transform") else {
        return Ok("None".to_owned());
    };
    if transform.get("kind").and_then(Value::as_str) != Some("scaled_integer") {
        return Err(extract_error!("unsupported storage transform: {transform}"));
    }
    Ok(format!(
        "Some(StorageTransform {{ input_unit: {}, numerator: {}, denominator: {} }})",
        rust_string(req_str(transform, "input_unit")?),
        req_i64(transform, "numerator")?,
        req_i64(transform, "denominator")?
    ))
}

/// Fixed preamble of the generated registry: header comment, provenance
/// constants, and the `MenuOption`/`StorageTransform`/`MenuField` types.
fn registry_header_lines(schema: &Value) -> Result<Vec<String>> {
    Ok([
        format!("// @generated by {GENERATOR}; do not edit."),
        "//! MCP-D75 menu field registry generated from the reviewed serializer manifest."
            .to_owned(),
        String::new(),
        "use super::schema::{Endian, FieldCodec, FieldDescriptor, StringEncoding};".to_owned(),
        String::new(),
        "/// Manifest format version used to generate this registry.".to_owned(),
        format!(
            "pub const MCP_D75_SCHEMA_VERSION: u32 = {};",
            req_i64(schema, "schema_version")?
        ),
        "/// SHA-256 of the normalized reviewed `ILSpy` source project.".to_owned(),
        "pub const MCP_D75_SOURCE_SHA256: &str =".to_owned(),
        format!(
            "    {};",
            rust_string(req_str(req(schema, "source")?, "normalized_source_sha256")?)
        ),
        String::new(),
        "/// One raw value in an MCP-D75 enum domain.".to_owned(),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]".to_owned(),
        "pub struct MenuOption {".to_owned(),
        "    /// Value stored in the memory image.".to_owned(),
        "    pub raw: u64,".to_owned(),
        "    /// Original member name from the decompiled enum.".to_owned(),
        "    pub member: &'static str,".to_owned(),
        "    /// English display label, when the official UI exposes one.".to_owned(),
        "    pub label: Option<&'static str>,".to_owned(),
        "    /// Official language-resource key, when the label is resource-backed.".to_owned(),
        "    pub resource_key: Option<&'static str>,".to_owned(),
        "}".to_owned(),
        String::new(),
        "/// Optional scaling between a display value and its stored integer.".to_owned(),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]".to_owned(),
        "pub struct StorageTransform {".to_owned(),
        "    /// Unit accepted by the documented transform.".to_owned(),
        "    pub input_unit: &'static str,".to_owned(),
        "    /// Encoding multiplier before rounding.".to_owned(),
        "    pub numerator: i64,".to_owned(),
        "    /// Encoding divisor before rounding.".to_owned(),
        "    pub denominator: i64,".to_owned(),
        "}".to_owned(),
        String::new(),
        "/// One writable public MCP-D75 menu or repeated-record field.".to_owned(),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]".to_owned(),
        "pub struct MenuField {".to_owned(),
        "    /// Top-level MCP menu group (`radio`, `gps`, `aprs`, or `dv`).".to_owned(),
        "    pub menu: &'static str,".to_owned(),
        "    /// Qualified decompiled enum type, when this field is enum-valued.".to_owned(),
        "    pub enum_type: Option<&'static str>,".to_owned(),
        "    /// Absolute memory offset and on-image codec.".to_owned(),
        "    pub descriptor: FieldDescriptor,".to_owned(),
        "    /// Raw enum domain; empty for non-enum fields.".to_owned(),
        "    pub options: &'static [MenuOption],".to_owned(),
        "    /// Exact allowed raw values for a non-enum choice domain.".to_owned(),
        "    pub allowed_values: &'static [u64],".to_owned(),
        "    /// Explicit display-to-storage scaling, when raw storage is encoded.".to_owned(),
        "    pub storage_transform: Option<StorageTransform>,".to_owned(),
        "    /// True for large persistent data blobs rather than scalar menu values.".to_owned(),
        "    pub is_blob: bool,".to_owned(),
        "}".to_owned(),
        String::new(),
        "impl MenuField {".to_owned(),
        "    /// Find enum metadata for a raw value.".to_owned(),
        "    #[must_use]".to_owned(),
        "    pub fn option(&self, raw: u64) -> Option<&'static MenuOption> {".to_owned(),
        "        self.options.iter().find(|option| option.raw == raw)".to_owned(),
        "    }".to_owned(),
        "}".to_owned(),
        String::new(),
    ]
    .to_vec())
}

/// Render the per-menu enum option constant blocks, filling `option_names`.
fn option_constant_lines(
    schema: &Value,
    option_names: &mut HashMap<String, String>,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for menu in req_array(schema, "menus")? {
        let menu_name = req_str(menu, "menu")?;
        for (index, catalog) in req_array(menu, "enum_types")?.iter().enumerate() {
            let enum_type = req_str(catalog, "name")?;
            let constant_name = format!("OPTIONS_{}_{index:03}", menu_name.to_uppercase());
            drop(option_names.insert(enum_type.to_owned(), constant_name.clone()));
            lines.push(format!("static {constant_name}: &[MenuOption] = &["));
            for option in req_array(catalog, "options")? {
                let raw = option.get("value").and_then(Value::as_i64);
                let Some(raw) = raw.filter(|value| *value >= 0) else {
                    return Err(extract_error!(
                        "enum {enum_type} has unsupported raw value {:?}",
                        option.get("value")
                    ));
                };
                lines.extend([
                    "    MenuOption {".to_owned(),
                    format!("        raw: {raw},"),
                    format!(
                        "        member: {},",
                        rust_string(req_str(option, "member")?)
                    ),
                    format!(
                        "        label: {},",
                        rust_option_string(option.get("label").and_then(Value::as_str))
                    ),
                    format!(
                        "        resource_key: {},",
                        rust_option_string(option.get("resource_key").and_then(Value::as_str))
                    ),
                    "    },".to_owned(),
                ]);
            }
            lines.extend(["];".to_owned(), String::new()]);
        }
    }
    Ok(lines)
}

/// Render the deduplicated allowed-value constants, filling
/// `choice_value_names` in first-encounter order.
fn choice_domain_lines(
    schema: &Value,
    choice_value_names: &mut Vec<(Vec<i64>, String)>,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for (_menu, field) in writable_manifest_fields(schema)? {
        let Some(domain) = field.get("domain") else {
            continue;
        };
        if domain.get("kind").and_then(Value::as_str) != Some("choices") {
            continue;
        }
        let values: Vec<i64> = req_array(domain, "allowed_values")?
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        if choice_value_names.iter().any(|(known, _)| *known == values) {
            continue;
        }
        let constant_name = format!("ALLOWED_DOMAIN_{:03}", choice_value_names.len());
        lines.push("#[rustfmt::skip]".to_owned());
        lines.push(format!("static {constant_name}: &[u64] = &["));
        for raw in &values {
            if *raw < 0 {
                return Err(extract_error!(
                    "choice domain has unsupported raw value {raw}"
                ));
            }
            lines.push(format!("    {raw},"));
        }
        lines.extend(["];".to_owned(), String::new()]);
        choice_value_names.push((values, constant_name));
    }
    Ok(lines)
}

/// Render one `MenuField` entry of the registry array.
fn menu_field_entry_lines(
    menu: &Value,
    operation: &Value,
    catalogs: &HashMap<String, &Value>,
    option_names: &HashMap<String, String>,
    choice_value_names: &[(Vec<i64>, String)],
) -> Result<Vec<String>> {
    let name = format!(
        "{}.{}",
        req_str(menu, "menu")?,
        display_name(req(operation, "name")?)
    );
    let codec = req(operation, "codec")?;
    let enum_type = codec.get("enum_type").and_then(Value::as_str);
    let options = match enum_type {
        None => "&[]".to_owned(),
        Some(enum_type) => option_names
            .get(enum_type)
            .cloned()
            .ok_or_else(|| extract_error!("field {name} references missing option domain"))?,
    };
    let domain = operation.get("domain");
    let allowed_values =
        if domain.and_then(|d| d.get("kind")).and_then(Value::as_str) == Some("choices") {
            let values: Vec<i64> = domain
                .and_then(|d| d.get("allowed_values"))
                .and_then(Value::as_array)
                .map(|array| array.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default();
            choice_value_names
                .iter()
                .find(|(known, _)| *known == values)
                .map(|(_, constant)| constant.clone())
                .ok_or_else(|| extract_error!("field {name} references missing option domain"))?
        } else {
            "&[]".to_owned()
        };
    let mut lines = vec![
        "    MenuField {".to_owned(),
        format!("        menu: {},", rust_string(req_str(menu, "menu")?)),
        format!("        enum_type: {},", rust_option_string(enum_type)),
        "        descriptor: FieldDescriptor::new(".to_owned(),
        format!("            {},", rust_string(&name)),
        format!("            0x{:X},", req_i64(operation, "offset")?),
    ];
    for codec_line in rust_codec(codec, catalogs, domain)? {
        lines.push(format!("            {codec_line}"));
    }
    let is_blob = operation.get("category").and_then(Value::as_str) == Some("blob");
    lines.extend([
        "        ),".to_owned(),
        format!("        options: {options},"),
        format!("        allowed_values: {allowed_values},"),
        format!(
            "        storage_transform: {},",
            rust_storage_transform(operation)?
        ),
        format!("        is_blob: {is_blob},"),
        "    },".to_owned(),
    ]);
    Ok(lines)
}

/// Render the public manifest fields as crate-native Rust descriptors.
///
/// # Errors
///
/// Returns an error when the schema is internally inconsistent: a codec
/// references a missing enum catalog or option domain, a domain exceeds its
/// storage capacity, or the rendered field count disagrees with the
/// manifest's summary.
pub fn rust_text(schema: &Value) -> Result<String> {
    let catalogs = enum_catalogs(schema)?;
    let mut option_names: HashMap<String, String> = HashMap::new();
    let mut choice_value_names: Vec<(Vec<i64>, String)> = Vec::new();
    let mut lines = registry_header_lines(schema)?;
    lines.extend(option_constant_lines(schema, &mut option_names)?);
    lines.extend(choice_domain_lines(schema, &mut choice_value_names)?);
    lines.extend([
        "/// All safely writable public fields from the reviewed MCP-D75 serializers.".to_owned(),
        "#[rustfmt::skip]".to_owned(),
        "pub static MCP_D75_MENU_FIELDS: &[MenuField] = &[".to_owned(),
    ]);
    let mut rendered_fields: u64 = 0;
    for (menu, operation) in writable_manifest_fields(schema)? {
        rendered_fields += 1;
        lines.extend(menu_field_entry_lines(
            menu,
            operation,
            &catalogs,
            &option_names,
            &choice_value_names,
        )?);
    }
    let expected_fields = req_i64(req(schema, "summary")?, "writable_registry_field_count")?;
    if i64::try_from(rendered_fields).unwrap_or_default() != expected_fields {
        return Err(extract_error!(
            "rendered {rendered_fields} public fields but manifest reports {expected_fields}"
        ));
    }
    lines.extend([
        "];".to_owned(),
        String::new(),
        "/// Look up a field by its `menu.name`, preferring an exact match before".to_owned(),
        "/// accepting ASCII case-insensitive input.".to_owned(),
        "#[must_use]".to_owned(),
        "pub fn menu_field(name: &str) -> Option<&'static MenuField> {".to_owned(),
        "    MCP_D75_MENU_FIELDS".to_owned(),
        "        .iter()".to_owned(),
        "        .find(|field| field.descriptor.name == name)".to_owned(),
        "        .or_else(|| {".to_owned(),
        "            MCP_D75_MENU_FIELDS".to_owned(),
        "                .iter()".to_owned(),
        "                .find(|field| field.descriptor.name.eq_ignore_ascii_case(name))"
            .to_owned(),
        "        })".to_owned(),
        "}".to_owned(),
        String::new(),
    ]);
    Ok(lines.join("\n"))
}
