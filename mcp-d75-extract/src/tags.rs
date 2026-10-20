//! Closed manifest tags shared by extraction and checked decoding.

use serde::{Deserialize, Serialize};

/// A supported radio identity in a manifest or extraction request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
pub enum RadioModel {
    /// Kenwood TH-D75.
    #[serde(rename = "thd75")]
    #[value(name = "thd75")]
    Thd75,
    /// Kenwood TM-D750.
    #[serde(rename = "tmd750")]
    #[value(name = "tmd750")]
    Tmd750,
}

impl RadioModel {
    /// Returns the stable command-line and JSON identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Thd75 => "thd75",
            Self::Tmd750 => "tmd750",
        }
    }

    /// Returns the sealed specification for this radio.
    #[must_use]
    pub const fn specification(self) -> &'static crate::ModelSpec {
        match self {
            Self::Thd75 => &crate::THD75,
            Self::Tmd750 => &crate::TMD750,
        }
    }
}

impl std::fmt::Display for RadioModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for RadioModel {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for RadioModel {
    type Err = crate::ExtractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "thd75" => Ok(Self::Thd75),
            "tmd750" => Ok(Self::Tmd750),
            _ => Err(crate::ExtractError::InvalidInput {
                message: format!("unsupported radio model {value:?}"),
            }),
        }
    }
}

/// Classified serializer value semantics, before or after candidate validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    /// Boolean source value.
    Bool,
    /// One-byte integer source value.
    Byte,
    /// Byte-array source value.
    Bytes,
    /// String source value.
    String,
    /// Signed integer source value.
    Int,
    /// Unsigned integer source value.
    Uint,
    /// Enum source value with a corresponding catalog reference.
    Enum,
    /// Literal source expression.
    Constant,
    /// Unresolved candidate metadata; checked manifests reject this value.
    Unknown,
}

impl ValueType {
    /// Returns the JSON tag for source inspection and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Byte => "byte",
            Self::Bytes => "bytes",
            Self::String => "string",
            Self::Int => "int",
            Self::Uint => "uint",
            Self::Enum => "enum",
            Self::Constant => "constant",
            Self::Unknown => "unknown",
        }
    }
}

impl AsRef<str> for ValueType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Encoding of a fixed-width text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextEncoding {
    /// The radio memory-map text encoding.
    MemoryMap,
    /// UTF-8 text.
    #[serde(rename = "utf8")]
    Utf8,
}

impl TextEncoding {
    /// Returns the persisted encoding name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryMap => "memory_map",
            Self::Utf8 => "utf8",
        }
    }
}

impl std::fmt::Display for TextEncoding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for TextEncoding {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// An additional interpretation of a raw-byte field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RawEncoding {
    /// Four IPv4 octets presented as dotted decimal text.
    #[serde(rename = "ipv4_dotted_quad")]
    Ipv4DottedQuad,
}

/// The supported display-to-storage transform family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformKind {
    /// Integer storage obtained by rounding a positive rational scaling.
    ScaledInteger,
}

/// The record-base expansion formula encoded in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordLayout {
    /// One fixed base without an index stride.
    Fixed,
    /// Base plus stride multiplied by record index.
    Linear,
    /// Linear expansion with explicit replacements for selected indices.
    LinearWithOverride,
}

impl RecordLayout {
    /// Returns the persisted formula tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Linear => "linear",
            Self::LinearWithOverride => "linear_with_override",
        }
    }
}

impl std::fmt::Display for RecordLayout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for RecordLayout {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// A field category with an additional storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldCategory {
    /// A persistent byte blob; its separate write restriction still applies.
    Blob,
}

/// The source representation read by the extraction pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceKind {
    /// An `ILSpy` C# project.
    #[serde(rename = "ILSpy C# project")]
    IlspyCsharpProject,
}

/// The language-resource file's checked encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LanguageEncoding {
    /// BOM-marked UTF-16, with byte order selected by its BOM.
    #[serde(rename = "UTF-16")]
    Utf16,
}
