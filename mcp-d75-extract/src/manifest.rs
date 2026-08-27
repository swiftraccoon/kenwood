//! The typed v4 manifest and its JSON projection.
//!
//! Field declaration order is the JSON key order; every v3 key keeps its
//! position and meaning, and v4 additions (`model`, `release`, `dimensions`,
//! `writer_class`, `address`, `terms`, provenance keys) sit after the v3
//! keys they extend. `Option` fields that v3 omitted when absent are skipped
//! when `None`; `name` and `index_expression` serialize as `null` exactly as
//! v3 did.

use serde::{Deserialize, Serialize};

use crate::address::{Address, Term};
use crate::error::{Result, extract_error};

/// Manifest format version emitted by this generator.
pub const SCHEMA_VERSION: u64 = 4;

/// Ordered-object serde adapter for `Vec<(String, V)>` fields.
pub(crate) mod ordered {
    use std::fmt;
    use std::marker::PhantomData;

    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Serialize entries as a JSON object in slice order.
    pub(crate) fn serialize<S, V>(entries: &[(String, V)], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        V: Serialize,
    {
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (key, value) in entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }

    struct EntriesVisitor<V>(PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for EntriesVisitor<V> {
        type Value = Vec<(String, V)>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
            let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(0));
            while let Some((key, value)) = access.next_entry::<String, V>()? {
                entries.push((key, value));
            }
            Ok(entries)
        }
    }

    /// Deserialize a JSON object into entries in document order.
    pub(crate) fn deserialize<'de, D, V>(deserializer: D) -> Result<Vec<(String, V)>, D::Error>
    where
        D: Deserializer<'de>,
        V: Deserialize<'de>,
    {
        deserializer.deserialize_map(EntriesVisitor(PhantomData))
    }
}

/// Role of one direct write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Writes a public property: a menu field.
    Field,
    /// Writes a private field: internal serializer state.
    Internal,
    /// Writes a literal.
    Constant,
    /// Fills a range with `0xFF`.
    Clear,
}

impl Role {
    /// The manifest string for this role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Internal => "internal",
            Self::Constant => "constant",
            Self::Clear => "clear",
        }
    }
}

/// On-image encoding of one write, tagged by `kind` exactly as v3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Codec {
    /// One byte.
    Byte {
        /// Decompiled C# type of the written expression, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        csharp_type: Option<String>,
        /// Classified value type (`byte`, `enum`, `constant`, ...).
        value_type: String,
        /// Literal expression for constant writes.
        #[serde(skip_serializing_if = "Option::is_none")]
        value_expression: Option<String>,
        /// Qualified enum type when the value is enum-valued.
        #[serde(skip_serializing_if = "Option::is_none")]
        enum_type: Option<String>,
    },
    /// One byte holding `0` or `1`.
    Bool {
        /// Decompiled C# type of the written expression, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        csharp_type: Option<String>,
        /// Classified value type.
        value_type: String,
        /// Literal expression for constant writes.
        #[serde(skip_serializing_if = "Option::is_none")]
        value_expression: Option<String>,
    },
    /// A raw byte array.
    RawBytes {
        /// Decompiled C# type of the written expression, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        csharp_type: Option<String>,
        /// Classified value type.
        value_type: String,
        /// Byte count, inferred from a clear at the same offset or pinned by a value helper.
        #[serde(skip_serializing_if = "Option::is_none")]
        length: Option<u64>,
        /// Value encoding pinned by a value helper (`ipv4_dotted_quad`).
        #[serde(skip_serializing_if = "Option::is_none")]
        encoding: Option<String>,
        /// Literal expression for constant writes.
        #[serde(skip_serializing_if = "Option::is_none")]
        value_expression: Option<String>,
    },
    /// A masked value inside one byte.
    BitField {
        /// Lowest bit index.
        bit: u64,
        /// Bit count.
        width: u64,
        /// Decompiled C# type of the written expression, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        csharp_type: Option<String>,
        /// Classified value type.
        value_type: String,
        /// Literal expression for constant writes.
        #[serde(skip_serializing_if = "Option::is_none")]
        value_expression: Option<String>,
        /// Qualified enum type when the value is enum-valued.
        #[serde(skip_serializing_if = "Option::is_none")]
        enum_type: Option<String>,
    },
    /// A little-endian unsigned integer.
    UnsignedLe {
        /// Byte width.
        width: u64,
        /// Decompiled C# type of the written expression.
        csharp_type: String,
        /// Classified value type.
        value_type: String,
        /// Qualified enum type when the value is enum-valued.
        #[serde(skip_serializing_if = "Option::is_none")]
        enum_type: Option<String>,
    },
    /// A little-endian signed integer.
    SignedLe {
        /// Byte width.
        width: u64,
        /// Decompiled C# type of the written expression.
        csharp_type: String,
        /// Classified value type.
        value_type: String,
        /// Qualified enum type when the value is enum-valued.
        #[serde(skip_serializing_if = "Option::is_none")]
        enum_type: Option<String>,
    },
    /// A fixed-width padded string.
    FixedString {
        /// `memory_map` or `utf8`.
        encoding: String,
        /// Reserved byte count.
        length: u64,
        /// Fill byte.
        padding: u64,
        /// Decompiled C# type of the written expression.
        csharp_type: String,
        /// Classified value type.
        value_type: String,
    },
    /// A range filled with `fill`.
    ClearRange {
        /// Byte count.
        length: u64,
        /// Fill value.
        fill: u64,
    },
}

impl Codec {
    /// The `kind` tag as written in the manifest.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Byte { .. } => "byte",
            Self::Bool { .. } => "bool",
            Self::RawBytes { .. } => "raw_bytes",
            Self::BitField { .. } => "bit_field",
            Self::UnsignedLe { .. } => "unsigned_le",
            Self::SignedLe { .. } => "signed_le",
            Self::FixedString { .. } => "fixed_string",
            Self::ClearRange { .. } => "clear_range",
        }
    }

    /// The classified value type, or `None` for clears.
    #[must_use]
    pub fn value_type(&self) -> Option<&str> {
        match self {
            Self::Byte { value_type, .. }
            | Self::Bool { value_type, .. }
            | Self::RawBytes { value_type, .. }
            | Self::BitField { value_type, .. }
            | Self::UnsignedLe { value_type, .. }
            | Self::SignedLe { value_type, .. }
            | Self::FixedString { value_type, .. } => Some(value_type),
            Self::ClearRange { .. } => None,
        }
    }

    /// The decompiled C# type, when recorded.
    #[must_use]
    pub fn csharp_type(&self) -> Option<&str> {
        match self {
            Self::Byte { csharp_type, .. }
            | Self::Bool { csharp_type, .. }
            | Self::RawBytes { csharp_type, .. }
            | Self::BitField { csharp_type, .. } => csharp_type.as_deref(),
            Self::UnsignedLe { csharp_type, .. }
            | Self::SignedLe { csharp_type, .. }
            | Self::FixedString { csharp_type, .. } => Some(csharp_type),
            Self::ClearRange { .. } => None,
        }
    }

    /// The qualified `<declaring class>.<enum>` type of an enum-valued codec.
    #[must_use]
    pub fn enum_type(&self) -> Option<&str> {
        match self {
            Self::Byte { enum_type, .. }
            | Self::BitField { enum_type, .. }
            | Self::UnsignedLe { enum_type, .. }
            | Self::SignedLe { enum_type, .. } => enum_type.as_deref(),
            _ => None,
        }
    }

    /// Bytes occupied on the image, when the codec fixes it.
    #[must_use]
    pub const fn encoded_length(&self) -> Option<u64> {
        match self {
            Self::Byte { .. } | Self::Bool { .. } | Self::BitField { .. } => Some(1),
            Self::UnsignedLe { width, .. } | Self::SignedLe { width, .. } => Some(*width),
            Self::FixedString { length, .. } | Self::ClearRange { length, .. } => Some(*length),
            Self::RawBytes { length, .. } => *length,
        }
    }
}

/// Audited value domain of a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Domain {
    /// Inclusive integer range.
    Range {
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
        /// Step between accepted values.
        step: u64,
        /// Where the audit came from (`ui_numeric`, `ui_choices`, `model_validation`, ...).
        provenance: String,
    },
    /// Explicit accepted values.
    Choices {
        /// Accepted raw values in audit order.
        allowed_values: Vec<i64>,
        /// Where the audit came from.
        provenance: String,
    },
}

/// Display-to-storage scaling recorded for encoded coordinate fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageTransform {
    /// Always `scaled_integer`.
    pub kind: String,
    /// Display unit accepted by the transform.
    pub input_unit: String,
    /// Human-readable encode formula.
    pub encode: String,
    /// Human-readable decode formula.
    pub decode: String,
    /// Multiplier before rounding.
    pub numerator: i64,
    /// Divisor before rounding.
    pub denominator: i64,
}

/// One direct write of a menu serializer or its per-slot detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Position in the menu's combined write order.
    pub sequence: u64,
    /// Field, internal, constant, or clear.
    pub role: Role,
    /// Property or field name; `null` for constants and clears.
    pub name: Option<String>,
    /// Class whose writer method contains the call.
    pub writer_class: String,
    /// All-indices-zero address (v3 `offset`).
    pub offset: u64,
    /// `offset` as `0x%04X`.
    pub offset_hex: String,
    /// Affine address with dimension terms.
    pub address: Address,
    /// On-image codec.
    pub codec: Codec,
    /// Audited domain, when pinned by the model spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<Domain>,
    /// `blob` for large persistent data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// `false` when sparse radio writes must reject the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    /// Why the field is not writable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_writable_reason: Option<String>,
}

/// A statement-form call into another serializer from a writer body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NestedSerializer {
    /// Call target as written (`this.m_a`, `u[num3]`, `ObjectList[num6]`).
    pub target: String,
    /// Called method name.
    pub method: String,
    /// Second argument, when the call passes an index.
    pub index_expression: Option<String>,
}

/// Record base layout: `linear`, `linear_with_override`, or `fixed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffsetLayout {
    /// Layout kind.
    pub kind: String,
    /// Slot-zero base of record 0.
    pub base: u64,
    /// Bytes between records, absent for `fixed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stride: Option<u64>,
    /// Per-index base replacements for `linear_with_override`.
    #[serde(default, with = "ordered", skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<(String, u64)>,
    /// Inherited dimension terms.
    pub terms: Vec<Term>,
}

/// One per-record write, relative to the record base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordField {
    /// Position in the child writer.
    pub sequence: u64,
    /// Field, internal, constant, or clear.
    pub role: Role,
    /// Public name after symbol overrides; `null` for constants.
    pub name: Option<String>,
    /// Offset from the record base.
    pub relative_offset: u64,
    /// On-image codec.
    pub codec: Codec,
    /// Alternative public names from the symbol table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    /// Encoded-storage transform from the symbol table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_transform: Option<StorageTransform>,
    /// Audited domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<Domain>,
    /// `false` when the field must not be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    /// Why the field is not writable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_writable_reason: Option<String>,
}

/// A record field expanded to one record index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedField {
    /// Record index.
    pub record_index: u64,
    /// `List[index].Name`.
    pub name: String,
    /// Record class containing the child writer.
    pub writer_class: String,
    /// All-indices-zero address.
    pub offset: u64,
    /// `offset` as `0x%04X`.
    pub offset_hex: String,
    /// Affine address with inherited slot terms.
    pub address: Address,
    /// On-image codec.
    pub codec: Codec,
    /// Alternative public names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    /// Encoded-storage transform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_transform: Option<StorageTransform>,
    /// Audited domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<Domain>,
    /// `false` when the field must not be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    /// Why the field is not writable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_writable_reason: Option<String>,
}

/// An extracted public repeated-record list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Public list property name.
    pub name: String,
    /// Record class.
    pub source_class: String,
    /// Source path relative to the project root.
    pub source_file: String,
    /// Child writer signature.
    pub write_method: String,
    /// Line of the child writer's opening brace.
    pub write_method_line: u64,
    /// Reviewed record count.
    pub count: u64,
    /// Base layout.
    pub offset_layout: OffsetLayout,
    /// Slot-zero base of every record.
    pub record_base_offsets: Vec<u64>,
    /// Writes per record.
    pub operation_count_per_record: u64,
    /// Public fields per record.
    pub field_count_per_record: u64,
    /// Per-record writes.
    pub fields: Vec<RecordField>,
    /// Fields expanded over the record index.
    pub expanded_fields: Vec<ExpandedField>,
}

/// A cataloged private sub-writer that exposes no public properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateRecord {
    /// Spec name (`private_pair_848`).
    pub name: String,
    /// Discovered private class.
    pub source_class: String,
    /// Nested calls into the class from the menu writer.
    pub call_count: u64,
    /// Records the writer covers.
    pub count: u64,
    /// Base layout.
    pub offset_layout: OffsetLayout,
    /// Why the writer is not extracted.
    pub unsupported_public_reason: String,
}

/// Either an extracted record list or a cataloged private writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RecordEntry {
    /// Extracted public record list.
    Extracted(Record),
    /// Cataloged private writer.
    Unsupported(PrivateRecord),
}

/// One raw enum member with its display metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumOption {
    /// Raw stored value.
    pub value: i64,
    /// Decompiled member name.
    pub member: String,
    /// Language resource key, when combo-backed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_key: Option<String>,
    /// English label, when resolvable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One enum type used by a menu's codecs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumCatalog {
    /// `owner.Enum` qualified name.
    pub name: String,
    /// Decompiled enum name.
    pub csharp_name: String,
    /// Underlying C# integer type.
    pub underlying_type: String,
    /// Members in declaration order.
    pub options: Vec<EnumOption>,
}

/// One top-level menu serializer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Menu {
    /// Spec menu key.
    pub menu: String,
    /// Public container property name.
    pub public_name: String,
    /// Discovered serializer class.
    pub csharp_class: String,
    /// Source path relative to the project root.
    pub source_file: String,
    /// Writer signature (`a0(m6 A_0)`).
    pub write_method: String,
    /// Line of the writer's opening brace.
    pub write_method_line: u64,
    /// Discovered per-slot detail class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_class: Option<String>,
    /// Detail writer signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_write_method: Option<String>,
    /// Line of the detail writer's opening brace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_write_method_line: Option<u64>,
    /// Direct writes, serializer first then detail.
    pub operation_count: u64,
    /// Writes with role `field`.
    pub field_count: u64,
    /// Direct writes.
    pub operations: Vec<Operation>,
    /// Nested serializer calls from the serializer writer body.
    pub nested_serializers: Vec<NestedSerializer>,
    /// Record lists and private catalog entries.
    pub repeated_records: Vec<RecordEntry>,
    /// Enum catalogs used by this menu.
    pub enum_types: Vec<EnumCatalog>,
}

/// Radio identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Spec id (`thd75`, `tmd750`).
    pub radio: String,
    /// Marketing name.
    pub product: String,
    /// `AssemblyProduct` of the decompiled program.
    pub mcp_product: String,
    /// Writer image length.
    pub image_length: u64,
}

/// Release identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseInfo {
    /// Declared MCP marketing version (`1.03`).
    pub mcp_version: String,
    /// `AssemblyVersion` of the decompiled program.
    pub assembly_version: String,
    /// Declared firmware target (`1.03`).
    pub firmware_target: String,
}

/// One anchor property of a dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// Public setter property name.
    pub property: String,
    /// Bytes assigned per index.
    pub stride: u64,
}

/// One declared index dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dimension {
    /// Dimension name (`pm_slot`).
    pub name: String,
    /// Index count.
    pub count: u64,
    /// Anchor properties carrying `stride * index`.
    pub anchors: Vec<Anchor>,
}

/// One discovered writer method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteMethodRef {
    /// Class name.
    pub class: String,
    /// Method name.
    pub method: String,
    /// Line of the opening brace.
    pub line: u64,
}

/// Language file provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageFileInfo {
    /// File name only.
    pub file_name: String,
    /// SHA-256 of the raw file.
    pub sha256: String,
    /// Always `UTF-16`.
    pub encoding: String,
}

/// Source provenance including discovered obfuscated anchors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Always `ILSpy C# project`.
    pub kind: String,
    /// Digest over label-tagged, newline-normalized sources.
    pub normalized_source_sha256: String,
    /// Menu key to serializer class.
    #[serde(with = "ordered")]
    pub serializer_classes: Vec<(String, String)>,
    /// Memory writer class.
    pub writer_class: String,
    /// Language resource singleton class.
    pub resource_class: String,
    /// Menu key to writer method.
    #[serde(with = "ordered")]
    pub write_methods: Vec<(String, WriteMethodRef)>,
    /// Menu key to per-slot detail class.
    #[serde(with = "ordered")]
    pub detail_classes: Vec<(String, String)>,
    /// Language file provenance when one was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_file: Option<LanguageFileInfo>,
}

/// Manifest-wide counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Menus.
    pub menu_count: u64,
    /// Direct writes.
    pub operation_count: u64,
    /// Direct writes with role `field`.
    pub field_count: u64,
    /// Expanded record fields.
    pub expanded_record_field_count: u64,
    /// `field_count + expanded_record_field_count`.
    pub total_public_field_count: u64,
    /// Writable direct and expanded fields.
    pub writable_registry_field_count: u64,
    /// Constant writes.
    pub constant_operation_count: u64,
    /// Internal writes.
    pub internal_operation_count: u64,
    /// Clears.
    pub clear_operation_count: u64,
    /// Nested serializer calls.
    pub nested_serializer_call_count: u64,
    /// Extracted record lists.
    pub repeated_record_type_count: u64,
    /// Private catalog entries.
    pub unsupported_public_record_type_count: u64,
    /// Enum catalogs.
    pub enum_type_count: u64,
    /// Enum members.
    pub enum_option_count: u64,
    /// Members with a label.
    pub labeled_enum_option_count: u64,
    /// Members with a resource key.
    pub resource_enum_option_count: u64,
    /// Combo-backed enum types.
    pub combo_enum_type_count: u64,
    /// Combo option mappings.
    pub combo_option_mapping_count: u64,
    /// Declared dimensions.
    pub dimension_count: u64,
    /// Writable fields with at least one dimension term.
    pub slot_relative_field_count: u64,
}

/// The complete v4 manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Always [`SCHEMA_VERSION`].
    pub schema_version: u64,
    /// Generator crate name.
    pub generator: String,
    /// Radio identity.
    pub model: ModelInfo,
    /// Release identity.
    pub release: ReleaseInfo,
    /// Declared dimensions (empty for the D75).
    pub dimensions: Vec<Dimension>,
    /// Source provenance.
    pub source: SourceInfo,
    /// Counts.
    pub summary: Summary,
    /// Menus in spec roster order.
    pub menus: Vec<Menu>,
}

/// Format an offset the way v3 did: `0x` plus at least four upper-case hex digits.
#[must_use]
pub fn offset_hex(offset: u64) -> String {
    format!("0x{offset:04X}")
}

/// Serialize the manifest as stable, indented JSON with a trailing newline.
///
/// # Errors
///
/// Returns an error when serialization fails.
pub fn json_text(manifest: &Manifest) -> Result<String> {
    let rendered = serde_json::to_string_pretty(manifest)
        .map_err(|error| extract_error!("cannot serialize manifest: {error}"))?;
    Ok(format!("{rendered}\n"))
}

/// Parse a v4 manifest.
///
/// # Errors
///
/// Returns an error when the text is not a v4 manifest.
pub fn parse_manifest(text: &str) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(text)
        .map_err(|error| extract_error!("cannot parse manifest: {error}"))?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(extract_error!(
            "unsupported manifest schema_version {}; this tool reads version {SCHEMA_VERSION}",
            manifest.schema_version
        ));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn codec_keys_follow_v3_order() -> TestResult {
        let codec = Codec::FixedString {
            encoding: "utf8".to_owned(),
            length: 3,
            padding: 0,
            csharp_type: "string".to_owned(),
            value_type: "string".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&codec)?,
            r#"{"kind":"fixed_string","encoding":"utf8","length":3,"padding":0,"csharp_type":"string","value_type":"string"}"#
        );
        let constant = Codec::Byte {
            csharp_type: None,
            value_type: "constant".to_owned(),
            value_expression: Some("(byte)0".to_owned()),
            enum_type: None,
        };
        assert_eq!(
            serde_json::to_string(&constant)?,
            r#"{"kind":"byte","value_type":"constant","value_expression":"(byte)0"}"#
        );
        Ok(())
    }

    #[test]
    fn operation_serializes_null_name_and_skips_absent_extras() -> TestResult {
        let operation = Operation {
            sequence: 0,
            role: Role::Clear,
            name: None,
            writer_class: "m9".to_owned(),
            offset: 327_680,
            offset_hex: offset_hex(327_680),
            address: Address::absolute(327_680),
            codec: Codec::ClearRange {
                length: 86_400,
                fill: 255,
            },
            domain: None,
            category: None,
            writable: None,
            not_writable_reason: None,
        };
        assert_eq!(
            serde_json::to_string(&operation)?,
            r#"{"sequence":0,"role":"clear","name":null,"writer_class":"m9","offset":327680,"offset_hex":"0x50000","address":{"base":327680,"terms":[]},"codec":{"kind":"clear_range","length":86400,"fill":255}}"#
        );
        Ok(())
    }

    #[test]
    fn ordered_maps_round_trip_in_document_order() -> TestResult {
        let source = SourceInfo {
            kind: "ILSpy C# project".to_owned(),
            normalized_source_sha256: "00".to_owned(),
            serializer_classes: vec![
                ("radio".to_owned(), "m9".to_owned()),
                ("gps".to_owned(), "m1".to_owned()),
                ("aprs".to_owned(), "l4".to_owned()),
            ],
            writer_class: "m6".to_owned(),
            resource_class: "kb".to_owned(),
            write_methods: Vec::new(),
            detail_classes: Vec::new(),
            language_file: None,
        };
        let text = serde_json::to_string(&source)?;
        assert!(
            text.contains(r#""serializer_classes":{"radio":"m9","gps":"m1","aprs":"l4"}"#),
            "ordered map must keep roster order: {text}"
        );
        let parsed: SourceInfo = serde_json::from_str(&text)?;
        assert_eq!(parsed, source);
        Ok(())
    }

    #[test]
    fn record_entries_deserialize_by_shape() -> TestResult {
        let private = r#"{"name":"private_blob_880","source_class":"m9.a5","call_count":1,"count":1,"offset_layout":{"kind":"fixed","base":880,"terms":[]},"unsupported_public_reason":"private"}"#;
        let entry: RecordEntry = serde_json::from_str(private)?;
        assert!(
            matches!(entry, RecordEntry::Unsupported(_)),
            "expected a private record, got {entry:?}"
        );
        Ok(())
    }

    #[test]
    fn offset_hex_pads_to_four_digits() {
        assert_eq!(offset_hex(4), "0x0004");
        assert_eq!(offset_hex(0x1080), "0x1080");
        assert_eq!(offset_hex(414_080), "0x65180");
    }
}
