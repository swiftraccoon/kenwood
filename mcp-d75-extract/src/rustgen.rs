//! Rendering of the manifest's writable fields as crate-native Rust descriptors.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::address::Term;
use crate::error::{Result, extract_error};
use crate::manifest::{
    Codec, Domain, EnumCatalog, ExpandedField, Manifest, Menu, Operation, RecordEntry, Role,
    StorageTransform,
};

/// Generator name recorded in the `@generated` header.
const GENERATOR: &str = env!("CARGO_PKG_NAME");

/// Per-radio rendering choices. The TH-D75 target reproduces the registry
/// the crate has always emitted, byte for byte.
struct Target {
    radio: &'static str,
    prefix: &'static str,
    product: &'static str,
    menus_doc: &'static str,
    terms: bool,
}

const THD75_TARGET: Target = Target {
    radio: "thd75",
    prefix: "MCP_D75",
    product: "MCP-D75",
    menus_doc: "(`radio`, `gps`, `aprs`, or `dv`)",
    terms: false,
};

const TMD750_TARGET: Target = Target {
    radio: "tmd750",
    prefix: "MCP_D750",
    product: "MCP-D750",
    menus_doc: "(`radio`, `gps`, `aprs`, `dv`, `ipnet`, or `pm`)",
    terms: true,
};

/// Render a decimal literal, grouping digits with `_` once it has six or
/// more of them (`1929472` becomes `1_929_472`; `86400` stays as it is).
fn rust_decimal(value: u64) -> String {
    let digits = value.to_string();
    if digits.len() < 6 {
        return digits;
    }
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push('_');
        }
        grouped.push(digit);
    }
    grouped
}

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

/// One writable field of either kind.
#[derive(Debug, Clone, Copy)]
enum Field<'a> {
    Direct(&'a Operation),
    Expanded(&'a ExpandedField),
}

impl<'a> Field<'a> {
    fn name(self) -> String {
        match self {
            Self::Direct(operation) => operation.name.clone().unwrap_or_else(|| "None".to_owned()),
            Self::Expanded(field) => field.name.clone(),
        }
    }

    const fn offset(self) -> u64 {
        match self {
            Self::Direct(operation) => operation.offset,
            Self::Expanded(field) => field.offset,
        }
    }

    const fn codec(self) -> &'a Codec {
        match self {
            Self::Direct(operation) => &operation.codec,
            Self::Expanded(field) => &field.codec,
        }
    }

    const fn domain(self) -> Option<&'a Domain> {
        match self {
            Self::Direct(operation) => operation.domain.as_ref(),
            Self::Expanded(field) => field.domain.as_ref(),
        }
    }

    const fn storage_transform(self) -> Option<&'a StorageTransform> {
        match self {
            Self::Direct(_) => None,
            Self::Expanded(field) => field.storage_transform.as_ref(),
        }
    }

    fn is_blob(self) -> bool {
        matches!(self, Self::Direct(operation) if operation.category.as_deref() == Some("blob"))
    }

    const fn has_terms(self) -> bool {
        match self {
            Self::Direct(operation) => !operation.address.is_absolute(),
            Self::Expanded(field) => !field.address.is_absolute(),
        }
    }

    fn terms(self) -> &'a [Term] {
        match self {
            Self::Direct(operation) => &operation.address.terms,
            Self::Expanded(field) => &field.address.terms,
        }
    }
}

/// Index enum catalogs by qualified name across all menus.
fn enum_catalogs(manifest: &Manifest) -> Result<HashMap<&str, &EnumCatalog>> {
    let mut catalogs = HashMap::new();
    for menu in &manifest.menus {
        for catalog in &menu.enum_types {
            if catalogs.insert(catalog.name.as_str(), catalog).is_some() {
                return Err(extract_error!(
                    "duplicate qualified enum catalog: {}",
                    catalog.name
                ));
            }
        }
    }
    Ok(catalogs)
}

/// Rust-literal min/max bounds for a little-endian integer width.
fn rust_integer_bounds(width: u64, signed: bool) -> Result<(String, String)> {
    if !(1..=8).contains(&width) {
        return Err(extract_error!(
            "integer field width must be in 1..=8, got {width}"
        ));
    }
    let bits = u32::try_from(width * 8).map_err(|_| extract_error!("width overflow"))?;
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
    catalogs: &HashMap<&str, &EnumCatalog>,
    maximum: i64,
) -> Result<(i64, i64)> {
    let catalog = catalogs
        .get(enum_type)
        .ok_or_else(|| extract_error!("field references missing enum catalog: {enum_type}"))?;
    if catalog.options.is_empty() {
        return Err(extract_error!("enum catalog has no options: {enum_type}"));
    }
    let mut values = Vec::new();
    for option in &catalog.options {
        if (0..=maximum).contains(&option.value) {
            values.push(option.value);
        } else {
            return Err(extract_error!(
                "enum {enum_type} contains a value outside its 0..={maximum} storage domain"
            ));
        }
    }
    let minimum = values.iter().min().copied().unwrap_or_default();
    let maximum = values.iter().max().copied().unwrap_or_default();
    Ok((minimum, maximum))
}

/// Inclusive `(min, max)` of a range or choices domain, if any.
fn domain_bounds(domain: Option<&Domain>) -> Result<Option<(i64, i64)>> {
    match domain {
        None => Ok(None),
        Some(Domain::Range { min, max, .. }) => Ok(Some((*min, *max))),
        Some(Domain::Choices { allowed_values, .. }) => {
            if allowed_values.is_empty() {
                return Err(extract_error!("choice domain has no allowed values"));
            }
            let minimum = allowed_values.iter().min().copied().unwrap_or_default();
            let maximum = allowed_values.iter().max().copied().unwrap_or_default();
            Ok(Some((minimum, maximum)))
        }
    }
}

/// Render a bit-field codec: `BitBool` for boolean values, `BitField` else.
fn bit_field_codec_lines(
    bit: u64,
    width: u64,
    is_bool: bool,
    enum_type: Option<&str>,
    catalogs: &HashMap<&str, &EnumCatalog>,
    domain: Option<&Domain>,
) -> Result<Vec<String>> {
    if width < 1 {
        return Err(extract_error!(
            "invalid bit field coordinates: bit={bit}, width={width}"
        ));
    }
    if bit + width > 8 {
        return Err(extract_error!(
            "bit field exceeds one byte: bit={bit}, width={width}"
        ));
    }
    let mask = ((1_u64 << width) - 1) << bit;
    if is_bool {
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
    let capacity =
        i64::try_from((1_u64 << width) - 1).map_err(|_| extract_error!("capacity overflow"))?;
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
fn integer_codec_lines(width: u64, domain: Option<&Domain>, signed: bool) -> Result<Vec<String>> {
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
    codec: &Codec,
    catalogs: &HashMap<&str, &EnumCatalog>,
    domain: Option<&Domain>,
) -> Result<Vec<String>> {
    match codec {
        Codec::Byte { enum_type, .. } => {
            let (minimum, maximum) = match enum_type.as_deref() {
                Some(enum_type) => raw_enum_bounds(enum_type, catalogs, 255)?,
                None => domain_bounds(domain)?.unwrap_or((0, 255)),
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
        Codec::Bool { .. } => Ok(vec!["FieldCodec::Bool".to_owned()]),
        Codec::BitField {
            bit,
            width,
            value_type,
            enum_type,
            ..
        } => bit_field_codec_lines(
            *bit,
            *width,
            value_type == "bool",
            enum_type.as_deref(),
            catalogs,
            domain,
        ),
        Codec::FixedString {
            encoding,
            length,
            padding,
            ..
        } => {
            let encoding = match encoding.as_str() {
                "utf8" => "StringEncoding::Utf8",
                "memory_map" => "StringEncoding::MemoryMap",
                other => return Err(extract_error!("unsupported string encoding: {other}")),
            };
            Ok(vec![
                "FieldCodec::FixedString {".to_owned(),
                format!("    len: {},", rust_decimal(*length)),
                format!("    encoding: {encoding},"),
                format!("    padding: {padding},"),
                "}".to_owned(),
            ])
        }
        Codec::SignedLe { width, .. } => integer_codec_lines(*width, domain, true),
        Codec::UnsignedLe { width, .. } => integer_codec_lines(*width, domain, false),
        Codec::RawBytes { length, .. } => {
            let length =
                length.ok_or_else(|| extract_error!("raw byte field has no inferred length"))?;
            Ok(vec![
                "FieldCodec::Bytes {".to_owned(),
                format!("    len: {},", rust_decimal(length)),
                "}".to_owned(),
            ])
        }
        Codec::ClearRange { .. } => Err(extract_error!(
            "cannot render field codec kind: clear_range"
        )),
    }
}

/// Yield writable direct and expanded fields with their containing menu.
fn writable_manifest_fields<'a>(
    manifest: &'a Manifest,
    target: &Target,
) -> Result<Vec<(&'a Menu, Field<'a>)>> {
    let mut fields = Vec::new();
    for menu in &manifest.menus {
        for operation in &menu.operations {
            if operation.role == Role::Field && operation.writable.unwrap_or(true) {
                fields.push((menu, Field::Direct(operation)));
            }
        }
        for entry in &menu.repeated_records {
            let RecordEntry::Extracted(record) = entry else {
                continue;
            };
            for field in &record.expanded_fields {
                if field.writable.unwrap_or(true) {
                    fields.push((menu, Field::Expanded(field)));
                }
            }
        }
    }
    for (menu, field) in &fields {
        if !target.terms && field.has_terms() {
            return Err(extract_error!(
                "Rust registry generation for {} requires absolute addresses; {}.{} has dimension terms",
                target.radio,
                menu.menu,
                field.name()
            ));
        }
    }
    Ok(fields)
}

/// Render a field's storage transform as a Rust `Option` literal.
fn rust_storage_transform(transform: Option<&StorageTransform>) -> Result<String> {
    let Some(transform) = transform else {
        return Ok("None".to_owned());
    };
    if transform.kind != "scaled_integer" {
        return Err(extract_error!(
            "unsupported storage transform: {}",
            transform.kind
        ));
    }
    Ok(format!(
        "Some(StorageTransform {{ input_unit: {}, numerator: {}, denominator: {} }})",
        rust_string(&transform.input_unit),
        transform.numerator,
        transform.denominator
    ))
}

/// Image length and slot constants for a target with dimension terms;
/// nothing for a target without them.
fn slot_constant_lines(manifest: &Manifest, target: &Target) -> Result<Vec<String>> {
    if !target.terms {
        return Ok(Vec::new());
    }
    let prefix = target.prefix;
    let slot = manifest
        .dimensions
        .iter()
        .find(|dimension| dimension.name == "pm_slot")
        .ok_or_else(|| extract_error!("{} manifest declares no pm_slot dimension", target.radio))?;
    let stride = slot
        .anchors
        .iter()
        .find(|anchor| anchor.property == "OffsetProgrammableMemoryAddress")
        .map(|anchor| anchor.stride)
        .ok_or_else(|| {
            extract_error!("pm_slot dimension has no OffsetProgrammableMemoryAddress anchor")
        })?;
    Ok(vec![
        "/// Bytes in the memory image.".to_owned(),
        format!(
            "pub const {prefix}_IMAGE_LENGTH: usize = {};",
            rust_decimal(manifest.model.image_length)
        ),
        "/// Programmable-Memory slots.".to_owned(),
        format!("pub const {prefix}_SLOT_COUNT: u8 = {};", slot.count),
        "/// Bytes between one slot's menu block and the next.".to_owned(),
        format!(
            "pub const {prefix}_SLOT_STRIDE: u32 = {};",
            rust_decimal(stride)
        ),
    ])
}

/// Fixed preamble of the generated registry: header comment, provenance
/// constants, and the `MenuOption`/`StorageTransform`/`MenuField` types.
fn registry_header_lines(manifest: &Manifest, target: &Target) -> Result<Vec<String>> {
    let prefix = target.prefix;
    let product = target.product;
    let schema_use = if target.terms {
        "use super::schema::{Endian, FieldCodec, FieldDescriptor, StringEncoding, Term};"
    } else {
        "use super::schema::{Endian, FieldCodec, FieldDescriptor, StringEncoding};"
    };
    let mut lines = vec![
        format!("// @generated by {GENERATOR}; do not edit."),
        format!(
            "//! {product} menu field registry generated from the reviewed serializer manifest."
        ),
        String::new(),
        schema_use.to_owned(),
        String::new(),
        "/// Manifest format version used to generate this registry.".to_owned(),
        format!(
            "pub const {prefix}_SCHEMA_VERSION: u32 = {};",
            manifest.schema_version
        ),
        "/// SHA-256 of the normalized reviewed `ILSpy` source project.".to_owned(),
        format!("pub const {prefix}_SOURCE_SHA256: &str ="),
        format!(
            "    {};",
            rust_string(&manifest.source.normalized_source_sha256)
        ),
    ];
    lines.extend(slot_constant_lines(manifest, target)?);
    lines.extend([
        String::new(),
        format!("/// One raw value in an {product} enum domain."),
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
        format!("/// One writable public {product} menu or repeated-record field."),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]".to_owned(),
        "pub struct MenuField {".to_owned(),
        format!("    /// Top-level MCP menu group {}.", target.menus_doc),
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
    ]);
    Ok(lines)
}

/// Render the per-menu enum option constant blocks, filling `option_names`.
fn option_constant_lines(
    manifest: &Manifest,
    option_names: &mut HashMap<String, String>,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for menu in &manifest.menus {
        for (index, catalog) in menu.enum_types.iter().enumerate() {
            let constant_name = format!("OPTIONS_{}_{index:03}", menu.menu.to_uppercase());
            drop(option_names.insert(catalog.name.clone(), constant_name.clone()));
            lines.push(format!("static {constant_name}: &[MenuOption] = &["));
            for option in &catalog.options {
                if option.value < 0 {
                    return Err(extract_error!(
                        "enum {} has unsupported raw value {}",
                        catalog.name,
                        option.value
                    ));
                }
                lines.extend([
                    "    MenuOption {".to_owned(),
                    format!("        raw: {},", option.value),
                    format!("        member: {},", rust_string(&option.member)),
                    format!(
                        "        label: {},",
                        rust_option_string(option.label.as_deref())
                    ),
                    format!(
                        "        resource_key: {},",
                        rust_option_string(option.resource_key.as_deref())
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
    manifest: &Manifest,
    target: &Target,
    choice_value_names: &mut Vec<(Vec<i64>, String)>,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for (_menu, field) in writable_manifest_fields(manifest, target)? {
        let Some(Domain::Choices { allowed_values, .. }) = field.domain() else {
            continue;
        };
        if choice_value_names
            .iter()
            .any(|(known, _)| *known == *allowed_values)
        {
            continue;
        }
        let constant_name = format!("ALLOWED_DOMAIN_{:03}", choice_value_names.len());
        lines.push("#[rustfmt::skip]".to_owned());
        lines.push(format!("static {constant_name}: &[u64] = &["));
        for raw in allowed_values {
            if *raw < 0 {
                return Err(extract_error!(
                    "choice domain has unsupported raw value {raw}"
                ));
            }
            lines.push(format!("    {raw},"));
        }
        lines.extend(["];".to_owned(), String::new()]);
        choice_value_names.push((allowed_values.clone(), constant_name));
    }
    Ok(lines)
}

/// Render one `MenuField` entry of the registry array.
fn menu_field_entry_lines(
    menu: &Menu,
    field: Field<'_>,
    catalogs: &HashMap<&str, &EnumCatalog>,
    option_names: &HashMap<String, String>,
    choice_value_names: &[(Vec<i64>, String)],
) -> Result<Vec<String>> {
    let name = format!("{}.{}", menu.menu, field.name());
    let codec = field.codec();
    let enum_type = codec.enum_type();
    let options = match enum_type {
        None => "&[]".to_owned(),
        Some(enum_type) => option_names
            .get(enum_type)
            .cloned()
            .ok_or_else(|| extract_error!("field {name} references missing option domain"))?,
    };
    let domain = field.domain();
    let allowed_values = if let Some(Domain::Choices { allowed_values, .. }) = domain {
        choice_value_names
            .iter()
            .find(|(known, _)| *known == *allowed_values)
            .map(|(_, constant)| constant.clone())
            .ok_or_else(|| extract_error!("field {name} references missing option domain"))?
    } else {
        "&[]".to_owned()
    };
    let mut lines = vec![
        "    MenuField {".to_owned(),
        format!("        menu: {},", rust_string(&menu.menu)),
        format!("        enum_type: {},", rust_option_string(enum_type)),
    ];
    let terms = field.terms();
    if terms.is_empty() {
        lines.push("        descriptor: FieldDescriptor::new(".to_owned());
        lines.push(format!("            {},", rust_string(&name)));
        lines.push(format!("            0x{:X},", field.offset()));
    } else {
        lines.push("        descriptor: FieldDescriptor::with_terms(".to_owned());
        lines.push(format!("            {},", rust_string(&name)));
        lines.push(format!("            0x{:X},", field.offset()));
        lines.push("            &[".to_owned());
        for term in terms {
            lines.push("                Term {".to_owned());
            lines.push(format!(
                "                    dimension: {},",
                rust_string(&term.dimension)
            ));
            lines.push(format!(
                "                    stride: {},",
                rust_decimal(term.stride)
            ));
            lines.push("                },".to_owned());
        }
        lines.push("            ],".to_owned());
    }
    for codec_line in rust_codec(codec, catalogs, domain)? {
        lines.push(format!("            {codec_line}"));
    }
    lines.extend([
        "        ),".to_owned(),
        format!("        options: {options},"),
        format!("        allowed_values: {allowed_values},"),
        format!(
            "        storage_transform: {},",
            rust_storage_transform(field.storage_transform())?
        ),
        format!("        is_blob: {},", field.is_blob()),
        "    },".to_owned(),
    ]);
    Ok(lines)
}

/// Render the public manifest fields as crate-native Rust descriptors.
///
/// # Errors
///
/// Returns an error when the manifest is not the TH-D75's, when a field
/// carries dimension terms, when a codec references a missing enum catalog
/// or option domain, when a domain exceeds its storage capacity, or when the
/// rendered field count disagrees with the manifest's summary.
pub fn rust_text(manifest: &Manifest) -> Result<String> {
    let target = match manifest.model.radio.as_str() {
        "thd75" => &THD75_TARGET,
        "tmd750" => &TMD750_TARGET,
        other => {
            return Err(extract_error!(
                "Rust registry generation is defined for thd75 and tmd750; got {other}"
            ));
        }
    };
    let catalogs = enum_catalogs(manifest)?;
    let mut option_names: HashMap<String, String> = HashMap::new();
    let mut choice_value_names: Vec<(Vec<i64>, String)> = Vec::new();
    let mut lines = registry_header_lines(manifest, target)?;
    lines.extend(option_constant_lines(manifest, &mut option_names)?);
    lines.extend(choice_domain_lines(
        manifest,
        target,
        &mut choice_value_names,
    )?);
    let statics = format!("{}_MENU_FIELDS", target.prefix);
    lines.extend([
        format!(
            "/// All safely writable public fields from the reviewed {} serializers.",
            target.product
        ),
        "#[rustfmt::skip]".to_owned(),
        format!("pub static {statics}: &[MenuField] = &["),
    ]);
    let mut rendered_fields: u64 = 0;
    for (menu, field) in writable_manifest_fields(manifest, target)? {
        rendered_fields += 1;
        lines.extend(menu_field_entry_lines(
            menu,
            field,
            &catalogs,
            &option_names,
            &choice_value_names,
        )?);
    }
    if rendered_fields != manifest.summary.writable_registry_field_count {
        return Err(extract_error!(
            "rendered {rendered_fields} public fields but manifest reports {}",
            manifest.summary.writable_registry_field_count
        ));
    }
    lines.extend([
        "];".to_owned(),
        String::new(),
        "/// Look up a field by its `menu.name`, preferring an exact match before".to_owned(),
        "/// accepting ASCII case-insensitive input.".to_owned(),
        "#[must_use]".to_owned(),
        "pub fn menu_field(name: &str) -> Option<&'static MenuField> {".to_owned(),
        format!("    {statics}"),
        "        .iter()".to_owned(),
        "        .find(|field| field.descriptor.name == name)".to_owned(),
        "        .or_else(|| {".to_owned(),
        format!("            {statics}"),
        "                .iter()".to_owned(),
        "                .find(|field| field.descriptor.name.eq_ignore_ascii_case(name))"
            .to_owned(),
        "        })".to_owned(),
        "}".to_owned(),
        String::new(),
    ]);
    Ok(lines.join("\n"))
}
