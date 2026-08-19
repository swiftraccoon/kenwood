//! Schema-driven MCP menu fields and safe masked patch planning.
//!
//! The official MCP-D75 application serializes menu properties into a raw
//! 500,480-byte image.  [`FieldDescriptor`] models those serializer writes,
//! while [`PatchPlanner`] converts requested values into byte masks that can
//! be applied to freshly-read radio pages.  Bit fields therefore preserve
//! unrelated bits even when the caller does not hold a current full image.

use std::collections::BTreeMap;
use std::fmt;

use crate::protocol::programming::{self, McpPage, WritableMcpPage};

/// Byte order for a multi-byte integer field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// Least-significant byte first.
    Little,
    /// Most-significant byte first.
    Big,
}

/// Encoding for a fixed-width string field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringEncoding {
    /// UTF-8 bytes.
    Utf8,
    /// MCP-D75's model-dependent memory-map encoding.
    ///
    /// The patch engine accepts only printable ASCII (`0x20`-`0x7E`) for this
    /// encoding. Other bytes are rejected because control bytes are not
    /// display text and the official application switches between Windows-
    /// 1252 and Shift-JIS for extended characters according to radio model.
    MemoryMap,
}

/// On-image encoding and validation domain for one menu field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldCodec {
    /// One unsigned byte.
    Byte {
        /// Smallest accepted raw value.
        min: u8,
        /// Largest accepted raw value.
        max: u8,
    },
    /// One byte containing `0` or `1`.
    Bool,
    /// One boolean bit within a byte shared by other fields.
    BitBool {
        /// The single bit owned by this field.
        mask: u8,
    },
    /// A masked unsigned value within one byte.
    BitField {
        /// Bits owned by this field.
        mask: u8,
        /// Right shift between the masked bits and the raw value.
        shift: u8,
        /// Smallest accepted raw value.
        min: u8,
        /// Largest accepted raw value.
        max: u8,
    },
    /// A fixed-width padded string.
    ///
    /// NUL padding is terminator-based: decoded bytes after the first NUL
    /// must also be NUL. Other padding bytes are removed only from the end.
    /// Semantic text containing a NUL for NUL padding, or ending in any
    /// non-NUL padding byte, is rejected so encoding and decoding are exact
    /// inverses for every accepted value.
    FixedString {
        /// Number of bytes reserved in the image.
        len: usize,
        /// Character encoding used by the field.
        encoding: StringEncoding,
        /// Byte used to fill unused trailing space.
        padding: u8,
    },
    /// An unsigned integer occupying one to eight bytes.
    Unsigned {
        /// Encoded width in bytes.
        width: u8,
        /// Byte order.
        endian: Endian,
        /// Smallest accepted value.
        min: u64,
        /// Largest accepted value.
        max: u64,
    },
    /// A signed integer occupying one to eight bytes.
    Signed {
        /// Encoded width in bytes.
        width: u8,
        /// Byte order.
        endian: Endian,
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
    },
    /// An exact-length raw byte sequence.
    Bytes {
        /// Required byte count.
        len: usize,
    },
}

impl FieldCodec {
    /// Number of image bytes this codec occupies.
    ///
    /// Bit-level codecs share their byte with other fields but still occupy
    /// exactly one image byte for span purposes.
    #[must_use]
    pub const fn encoded_len(self) -> usize {
        match self {
            Self::Byte { .. } | Self::Bool | Self::BitBool { .. } | Self::BitField { .. } => 1,
            Self::FixedString { len, .. } | Self::Bytes { len } => len,
            Self::Unsigned { width, .. } | Self::Signed { width, .. } => width as usize,
        }
    }

    /// Short human-readable name for the expected value kind.
    #[must_use]
    pub const fn value_kind(self) -> &'static str {
        match self {
            Self::Byte { .. } | Self::BitField { .. } | Self::Unsigned { .. } => "unsigned",
            Self::Bool | Self::BitBool { .. } => "boolean",
            Self::FixedString { .. } => "text",
            Self::Signed { .. } => "signed",
            Self::Bytes { .. } => "bytes",
        }
    }
}

/// One persistent MCP-D75 menu field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDescriptor {
    /// Stable field name, normally prefixed with its menu group.
    pub name: &'static str,
    /// Absolute byte offset in the raw MCP image.
    pub offset: usize,
    /// Storage encoding and validation domain.
    pub codec: FieldCodec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueDomain {
    Stored,
    Writable,
}

impl FieldDescriptor {
    /// Construct a field descriptor.
    #[must_use]
    pub const fn new(name: &'static str, offset: usize, codec: FieldCodec) -> Self {
        Self {
            name,
            offset,
            codec,
        }
    }

    /// Physical MCP page containing the field's first byte.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OffsetTooLarge`] when the offset cannot be
    /// represented by the TH-D75's 16-bit page address, or
    /// [`SchemaError::OutOfBounds`] when it is outside the physical image.
    pub fn page(self) -> Result<McpPage, SchemaError> {
        let page = self.offset / programming::PAGE_SIZE;
        let page = u16::try_from(page).map_err(|_| SchemaError::OffsetTooLarge {
            field: self.name,
            offset: self.offset,
        })?;
        McpPage::new(page).map_err(|_| SchemaError::OutOfBounds {
            field: self.name,
            offset: self.offset,
            len: 1,
            image_len: programming::TOTAL_SIZE,
        })
    }

    /// Every physical MCP page the field's encoded bytes touch, ascending.
    ///
    /// Multi-byte fields can span pages (the widest generated field spans
    /// hundreds), so sparse reads must fetch this whole list rather than
    /// [`Self::page`] alone.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OffsetTooLarge`] when a spanned page cannot be
    /// represented by the 16-bit page address, or
    /// [`SchemaError::OutOfBounds`] when the span leaves the physical image.
    pub fn pages(self) -> Result<Vec<McpPage>, SchemaError> {
        let len = self.codec.encoded_len().max(1);
        let last_offset = self
            .offset
            .checked_add(len - 1)
            .ok_or(SchemaError::OffsetTooLarge {
                field: self.name,
                offset: self.offset,
            })?;
        let first_page = self.offset / programming::PAGE_SIZE;
        let last_page = last_offset / programming::PAGE_SIZE;
        let mut pages = Vec::with_capacity(last_page - first_page + 1);
        for page in first_page..=last_page {
            let page = u16::try_from(page).map_err(|_| SchemaError::OffsetTooLarge {
                field: self.name,
                offset: last_offset,
            })?;
            let page = McpPage::new(page).map_err(|_| SchemaError::OutOfBounds {
                field: self.name,
                offset: self.offset,
                len,
                image_len: programming::TOTAL_SIZE,
            })?;
            pages.push(page);
        }
        Ok(pages)
    }

    /// Decode this field from a complete raw MCP image.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-bounds field, malformed codec, invalid
    /// encoded value, or a value outside the field's declared domain.
    /// [`SchemaError::FixedStringDataAfterNul`] identifies an ambiguous
    /// NUL-padded field instead of discarding bytes after its terminator. If
    /// this descriptor names a generated menu field, its finite enum or
    /// UI-choice domain is enforced as well.
    pub fn read(self, image: &[u8]) -> Result<DecodedFieldValue, SchemaError> {
        self.decode(image, ValueDomain::Writable)
    }

    /// Decode this field's exact stored value from a complete raw MCP image.
    ///
    /// Unlike [`Self::read`], this accepts every value representable by the
    /// storage codec even when the value is outside the radio's official
    /// writable menu domain. This is intended for lossless snapshots of
    /// factory, firmware-added, and otherwise off-menu values. It does not
    /// relax [`PatchPlanner`] or any other write path.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-bounds field, malformed codec, or an
    /// invalid stored representation such as a non-boolean byte in a boolean
    /// field or malformed fixed-width text.
    pub fn read_stored(self, image: &[u8]) -> Result<DecodedFieldValue, SchemaError> {
        self.decode(image, ValueDomain::Stored)
    }

    /// Validate that a typed value is representable by this field's storage
    /// codec without requiring it to be in the writable menu domain.
    ///
    /// This is suitable for optimistic-concurrency expected values copied
    /// from [`Self::read_stored`]. Callers must still use [`PatchPlanner`] to
    /// validate any value that will be written.
    ///
    /// # Errors
    ///
    /// Returns an error for a mismatched value kind, a value wider than the
    /// storage representation, malformed text or byte lengths, malformed
    /// codec metadata, or a stale generated descriptor.
    pub fn validate_stored_value(self, value: FieldValue<'_>) -> Result<(), SchemaError> {
        let _catalog_field = self.validate_catalog_descriptor()?;
        let _encoded = encode_field(&self, value, ValueDomain::Stored)?;
        Ok(())
    }

    fn validate_catalog_descriptor(self) -> Result<Option<&'static super::MenuField>, SchemaError> {
        let menu_field = super::menu_field(self.name);
        if let Some(field) = menu_field
            && field.descriptor != self
        {
            return Err(SchemaError::CatalogDescriptorMismatch {
                field: self.name,
                offset: self.offset,
                expected_offset: field.descriptor.offset,
            });
        }
        Ok(menu_field)
    }

    fn decode(self, image: &[u8], domain: ValueDomain) -> Result<DecodedFieldValue, SchemaError> {
        let menu_field = self.validate_catalog_descriptor()?;

        let decoded = match self.codec {
            FieldCodec::Byte { min, max } => {
                let value = u64::from(read_byte(image, self.name, self.offset)?);
                if domain == ValueDomain::Writable {
                    validate_unsigned(self.name, value, u64::from(min), u64::from(max))?;
                }
                DecodedFieldValue::Unsigned(value)
            }
            FieldCodec::Bool => {
                let value = read_byte(image, self.name, self.offset)?;
                validate_unsigned(self.name, u64::from(value), 0, 1)?;
                DecodedFieldValue::Bool(value == 1)
            }
            FieldCodec::BitBool { mask } => {
                validate_bool_mask(self.name, mask)?;
                DecodedFieldValue::Bool(read_byte(image, self.name, self.offset)? & mask != 0)
            }
            FieldCodec::BitField {
                mask,
                shift,
                min,
                max,
            } => {
                validate_bit_codec(self.name, mask, shift, min, max)?;
                let byte = read_byte(image, self.name, self.offset)?;
                let value = u64::from((byte & mask) >> shift);
                if domain == ValueDomain::Writable {
                    validate_unsigned(self.name, value, u64::from(min), u64::from(max))?;
                }
                DecodedFieldValue::Unsigned(value)
            }
            FieldCodec::FixedString {
                len,
                encoding,
                padding,
            } => {
                let bytes = read_range(image, self.name, self.offset, len)?;
                let semantic =
                    decode_fixed_string_bytes(self.name, self.offset, image.len(), bytes, padding)?;
                if encoding == StringEncoding::MemoryMap
                    && let Some((offset, &value)) = semantic
                        .iter()
                        .enumerate()
                        .find(|(_, value)| !is_printable_ascii(**value))
                {
                    return Err(SchemaError::InvalidMemoryMapTextByte {
                        field: self.name,
                        offset,
                        value,
                    });
                }
                let text = std::str::from_utf8(semantic)
                    .map_err(|_| SchemaError::InvalidText { field: self.name })?;
                DecodedFieldValue::Text(text.to_owned())
            }
            FieldCodec::Unsigned {
                width,
                endian,
                min,
                max,
            } => {
                let width = validate_width(self.name, width)?;
                validate_unsigned_capacity(self.name, width, max)?;
                let bytes = read_range(image, self.name, self.offset, width.bytes())?;
                let value = decode_unsigned(bytes, endian);
                if domain == ValueDomain::Writable {
                    validate_unsigned(self.name, value, min, max)?;
                }
                DecodedFieldValue::Unsigned(value)
            }
            FieldCodec::Signed {
                width,
                endian,
                min,
                max,
            } => {
                let width = validate_width(self.name, width)?;
                validate_signed_capacity(self.name, width, min, max)?;
                let bytes = read_range(image, self.name, self.offset, width.bytes())?;
                let value = decode_signed(bytes, width, endian);
                if domain == ValueDomain::Writable {
                    validate_signed(self.name, value, min, max)?;
                }
                DecodedFieldValue::Signed(value)
            }
            FieldCodec::Bytes { len } => {
                DecodedFieldValue::Bytes(read_range(image, self.name, self.offset, len)?.to_vec())
            }
        };

        if domain == ValueDomain::Writable
            && let Some(field) = menu_field
        {
            field.validate_patch_value(decoded.as_field_value())?;
        }

        Ok(decoded)
    }
}

/// Caller-supplied value for a schema field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldValue<'a> {
    /// Unsigned byte, enum, bit-field, or multi-byte value.
    Unsigned(u64),
    /// Signed multi-byte value.
    Signed(i64),
    /// Boolean value.
    Bool(bool),
    /// Text value.
    Text(&'a str),
    /// Raw byte sequence.
    Bytes(&'a [u8]),
}

impl FieldValue<'_> {
    /// Short human-readable name for this value variant.
    pub(crate) const fn kind_name(self) -> &'static str {
        match self {
            Self::Unsigned(_) => "unsigned",
            Self::Signed(_) => "signed",
            Self::Bool(_) => "boolean",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
        }
    }
}

/// Owned value decoded from an MCP image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedFieldValue {
    /// Unsigned byte, enum, bit-field, or multi-byte value.
    Unsigned(u64),
    /// Signed multi-byte value.
    Signed(i64),
    /// Boolean value.
    Bool(bool),
    /// Decoded text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
}

impl DecodedFieldValue {
    const fn as_field_value(&self) -> FieldValue<'_> {
        match self {
            Self::Unsigned(value) => FieldValue::Unsigned(*value),
            Self::Signed(value) => FieldValue::Signed(*value),
            Self::Bool(value) => FieldValue::Bool(*value),
            Self::Text(value) => FieldValue::Text(value.as_str()),
            Self::Bytes(value) => FieldValue::Bytes(value.as_slice()),
        }
    }
}

/// Failure while validating, encoding, merging, or applying schema patches.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaError {
    /// The supplied value variant does not match the field codec.
    TypeMismatch {
        /// Field name.
        field: &'static str,
        /// Codec's expected value kind.
        expected: &'static str,
        /// Supplied value kind.
        actual: &'static str,
    },
    /// An unsigned value is outside the descriptor's accepted domain.
    UnsignedOutOfRange {
        /// Field name.
        field: &'static str,
        /// Supplied value.
        value: u64,
        /// Smallest accepted value.
        min: u64,
        /// Largest accepted value.
        max: u64,
    },
    /// A signed value is outside the descriptor's accepted domain.
    SignedOutOfRange {
        /// Field name.
        field: &'static str,
        /// Supplied value.
        value: i64,
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
    },
    /// An unsigned raw value is not a member of the field's finite domain.
    DisallowedValue {
        /// Field name.
        field: &'static str,
        /// Supplied raw value.
        value: u64,
    },
    /// A descriptor reuses a generated catalog name with different metadata.
    CatalogDescriptorMismatch {
        /// Field name shared with the generated catalog entry.
        field: &'static str,
        /// Offset supplied by the caller.
        offset: usize,
        /// Offset declared by the generated catalog.
        expected_offset: usize,
    },
    /// Text is too long for a fixed-width field.
    TextTooLong {
        /// Field name.
        field: &'static str,
        /// Encoded byte count.
        actual: usize,
        /// Maximum byte count.
        max: usize,
    },
    /// Semantic text for a NUL-padded field contains its terminator byte.
    TextContainsNul {
        /// Field name.
        field: &'static str,
        /// Zero-based byte offset of the NUL within the supplied text.
        offset: usize,
    },
    /// Semantic text ends with the field's padding byte and would be shortened
    /// when decoded.
    TextEndsWithPadding {
        /// Field name.
        field: &'static str,
        /// Zero-based byte offset of the trailing padding byte.
        offset: usize,
        /// Padding byte declared by the field codec.
        padding: u8,
    },
    /// A NUL-padded image contains data after its first NUL terminator.
    FixedStringDataAfterNul {
        /// Field name.
        field: &'static str,
        /// Zero-based byte offset of the first NUL terminator.
        terminator_offset: usize,
        /// Zero-based byte offset of the unexpected later byte.
        offset: usize,
        /// Unexpected non-NUL byte.
        value: u8,
    },
    /// A model-dependent memory-map field contains a non-display byte.
    InvalidMemoryMapTextByte {
        /// Field name.
        field: &'static str,
        /// Zero-based byte offset within the semantic text.
        offset: usize,
        /// Invalid byte.
        value: u8,
    },
    /// Existing bytes are not valid text for the descriptor.
    InvalidText {
        /// Field name.
        field: &'static str,
    },
    /// A byte sequence has the wrong length.
    ByteLength {
        /// Field name.
        field: &'static str,
        /// Supplied byte count.
        actual: usize,
        /// Required byte count.
        expected: usize,
    },
    /// A field extends beyond the target image.
    OutOfBounds {
        /// Field name.
        field: &'static str,
        /// Absolute starting offset.
        offset: usize,
        /// Required byte count.
        len: usize,
        /// Available image byte count.
        image_len: usize,
    },
    /// A sparse snapshot does not contain a page required by the field.
    SnapshotPageMissing {
        /// Field whose bytes were requested.
        field: &'static str,
        /// Required page absent from the snapshot.
        page: McpPage,
    },
    /// An offset cannot be represented by a 16-bit MCP page number.
    OffsetTooLarge {
        /// Field name.
        field: &'static str,
        /// Absolute byte offset.
        offset: usize,
    },
    /// A patch targets the factory-calibration region, which must never be
    /// overwritten.
    WriteProtected {
        /// Field name.
        field: &'static str,
        /// Protected MCP page.
        page: McpPage,
    },
    /// Integer width is zero or greater than eight bytes.
    InvalidIntegerWidth {
        /// Field name.
        field: &'static str,
        /// Invalid width.
        width: u8,
    },
    /// An integer domain does not fit within the codec's encoded width.
    DomainExceedsWidth {
        /// Field name.
        field: &'static str,
        /// Encoded width in bytes.
        width: u8,
    },
    /// A bit-field mask and shift do not describe usable bits.
    InvalidBitField {
        /// Field name.
        field: &'static str,
        /// Declared mask.
        mask: u8,
        /// Declared shift.
        shift: u8,
    },
    /// Two requested fields assign different values to the same owned bit.
    PatchConflict {
        /// Field whose requested bits conflict with an earlier assignment.
        field: &'static str,
        /// Field that first claimed bits at the conflicting byte.
        existing: &'static str,
        /// Absolute byte offset containing the conflict.
        offset: usize,
        /// Conflicting bits.
        mask: u8,
    },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => write!(f, "field {field} expects {expected}, received {actual}"),
            Self::UnsignedOutOfRange {
                field,
                value,
                min,
                max,
            } => write!(f, "field {field} value {value} is outside {min}..={max}"),
            Self::SignedOutOfRange {
                field,
                value,
                min,
                max,
            } => write!(f, "field {field} value {value} is outside {min}..={max}"),
            Self::DisallowedValue { field, value } => {
                write!(f, "field {field} does not allow raw value {value}")
            }
            Self::CatalogDescriptorMismatch {
                field,
                offset,
                expected_offset,
            } => fmt_catalog_descriptor_mismatch(f, field, *offset, *expected_offset),
            Self::TextTooLong { field, actual, max } => {
                write!(f, "field {field} text is {actual} bytes (maximum {max})")
            }
            Self::TextContainsNul { field, offset } => write!(
                f,
                "field {field} text contains a NUL terminator at byte {offset}"
            ),
            Self::TextEndsWithPadding {
                field,
                offset,
                padding,
            } => fmt_text_ends_with_padding(f, field, *offset, *padding),
            Self::FixedStringDataAfterNul {
                field,
                terminator_offset,
                offset,
                value,
            } => fmt_fixed_string_data_after_nul(f, field, *terminator_offset, *offset, *value),
            Self::InvalidMemoryMapTextByte {
                field,
                offset,
                value,
            } => write!(
                f,
                "field {field} text byte at offset {offset} is 0x{value:02X} \
                 (expected printable ASCII 0x20-0x7E)"
            ),
            Self::InvalidText { field } => write!(f, "field {field} contains invalid text"),
            Self::ByteLength {
                field,
                actual,
                expected,
            } => write!(
                f,
                "field {field} received {actual} bytes (expected {expected})"
            ),
            Self::OutOfBounds {
                field,
                offset,
                len,
                image_len,
            } => fmt_out_of_bounds(f, field, *offset, *len, *image_len),
            Self::SnapshotPageMissing { field, page } => write!(
                f,
                "field {field} requires MCP page 0x{page:04X}, which was not fetched"
            ),
            Self::OffsetTooLarge { field, offset } => fmt_offset_too_large(f, field, *offset),
            Self::WriteProtected { field, page } => write!(
                f,
                "field {field} touches write-protected factory calibration page 0x{page:04X}"
            ),
            Self::InvalidIntegerWidth { field, width } => {
                write!(f, "field {field} has invalid integer width {width}")
            }
            Self::DomainExceedsWidth { field, width } => {
                write!(
                    f,
                    "field {field} integer domain does not fit in {width} byte(s)"
                )
            }
            Self::InvalidBitField { field, mask, shift } => write!(
                f,
                "field {field} has invalid bit field mask 0x{mask:02X}, shift {shift}"
            ),
            Self::PatchConflict {
                field,
                existing,
                offset,
                mask,
            } => fmt_patch_conflict(f, field, existing, *offset, *mask),
        }
    }
}

fn fmt_offset_too_large(
    formatter: &mut fmt::Formatter<'_>,
    field: &str,
    offset: usize,
) -> fmt::Result {
    write!(
        formatter,
        "field {field} offset 0x{offset:X} exceeds MCP addressing"
    )
}

fn fmt_catalog_descriptor_mismatch(
    formatter: &mut fmt::Formatter<'_>,
    field: &str,
    offset: usize,
    expected_offset: usize,
) -> fmt::Result {
    write!(
        formatter,
        "field {field} descriptor does not match the generated catalog \
         (offset 0x{offset:X}, expected 0x{expected_offset:X})"
    )
}

fn fmt_text_ends_with_padding(
    formatter: &mut fmt::Formatter<'_>,
    field: &str,
    offset: usize,
    padding: u8,
) -> fmt::Result {
    write!(
        formatter,
        "field {field} text ends with padding byte 0x{padding:02X} at byte {offset}"
    )
}

fn fmt_fixed_string_data_after_nul(
    formatter: &mut fmt::Formatter<'_>,
    field: &str,
    terminator_offset: usize,
    offset: usize,
    value: u8,
) -> fmt::Result {
    write!(
        formatter,
        "field {field} contains byte 0x{value:02X} at byte {offset} after its NUL terminator at \
         byte {terminator_offset}"
    )
}

fn fmt_out_of_bounds(
    formatter: &mut fmt::Formatter<'_>,
    field: &str,
    offset: usize,
    len: usize,
    image_len: usize,
) -> fmt::Result {
    write!(
        formatter,
        "field {field} range 0x{offset:X}..+{len} exceeds image length {image_len}"
    )
}

fn fmt_patch_conflict(
    formatter: &mut fmt::Formatter<'_>,
    field: &str,
    existing: &str,
    offset: usize,
    mask: u8,
) -> fmt::Result {
    write!(
        formatter,
        "field {field} conflicts with bits planned by {existing} at MCP offset 0x{offset:X}, \
         mask 0x{mask:02X}"
    )
}

impl std::error::Error for SchemaError {}

/// One masked byte update within an MCP page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BytePatch {
    offset: u8,
    mask: u8,
    value: u8,
}

impl BytePatch {
    /// Byte offset within the page.
    #[must_use]
    pub const fn offset(self) -> u8 {
        self.offset
    }

    /// Bits owned by the patch.
    #[must_use]
    pub const fn mask(self) -> u8 {
        self.mask
    }

    /// Desired raw bits already positioned under [`mask`](Self::mask).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.value
    }
}

/// Masked changes for one 256-byte MCP page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePatch {
    page: WritableMcpPage,
    bytes: Vec<BytePatch>,
}

impl PagePatch {
    /// Validated writable MCP page address.
    #[must_use]
    pub const fn page(&self) -> WritableMcpPage {
        self.page
    }

    /// Sorted byte patches within this page.
    #[must_use]
    pub fn bytes(&self) -> &[BytePatch] {
        &self.bytes
    }

    /// Apply this patch to a freshly-read page.
    ///
    /// Every byte update is masked so unrelated bits remain unchanged.
    pub fn apply_to_page(&self, page: &mut [u8; programming::PAGE_SIZE]) {
        for patch in &self.bytes {
            if let Some(byte) = page.get_mut(usize::from(patch.offset)) {
                *byte = (*byte & !patch.mask) | (patch.as_raw() & patch.mask);
            }
        }
    }
}

/// A validated, page-coalesced group of MCP field changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PatchSet {
    pages: Vec<PagePatch>,
}

impl PatchSet {
    /// Whether this set contains no byte changes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Number of pages touched by this set.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.pages.len()
    }

    /// Sorted page patches.
    #[must_use]
    pub fn page_patches(&self) -> &[PagePatch] {
        &self.pages
    }

    /// Iterate over the sorted writable MCP page addresses.
    pub fn pages(&self) -> impl Iterator<Item = WritableMcpPage> + '_ {
        self.pages.iter().map(PagePatch::page)
    }

    /// Find the patch for one MCP page.
    #[must_use]
    pub fn page(&self, page: WritableMcpPage) -> Option<&PagePatch> {
        self.pages.iter().find(|patch| patch.page == page)
    }

    /// Apply every patch to a complete raw MCP image.
    ///
    /// The whole set is validated against the image bounds before any byte
    /// is modified, so a failed application never leaves the image partially
    /// patched.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OutOfBounds`] if the image does not contain a
    /// touched byte; the image is unmodified in that case.
    pub fn apply_to_image(&self, image: &mut [u8]) -> Result<(), SchemaError> {
        for page_patch in &self.pages {
            let page_start = usize::from(page_patch.page.as_raw()) * programming::PAGE_SIZE;
            for patch in &page_patch.bytes {
                let absolute = page_start + usize::from(patch.offset);
                if absolute >= image.len() {
                    return Err(SchemaError::OutOfBounds {
                        field: "patch set",
                        offset: absolute,
                        len: 1,
                        image_len: image.len(),
                    });
                }
            }
        }
        for page_patch in &self.pages {
            let page_start = usize::from(page_patch.page.as_raw()) * programming::PAGE_SIZE;
            for patch in &page_patch.bytes {
                let absolute = page_start + usize::from(patch.offset);
                if let Some(byte) = image.get_mut(absolute) {
                    *byte = (*byte & !patch.mask) | (patch.as_raw() & patch.mask);
                }
            }
        }
        Ok(())
    }
}

/// One planned byte: claimed bits, their values, and the claiming field.
#[derive(Debug)]
struct ByteClaim {
    mask: u8,
    value: u8,
    owner: &'static str,
}

/// Builds a [`PatchSet`] without requiring a cached memory image.
#[derive(Debug, Default)]
pub struct PatchPlanner {
    bytes: BTreeMap<usize, ByteClaim>,
}

impl PatchPlanner {
    /// Create an empty planner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: BTreeMap::new(),
        }
    }

    /// Add a requested menu field value.
    ///
    /// Non-overlapping bits at the same byte are coalesced.  Overlapping bits
    /// may be repeated only when both assignments request the same value;
    /// assigning a different value to already-claimed bits is a
    /// [`SchemaError::PatchConflict`].  A later assignment therefore never
    /// silently replaces an earlier one.
    ///
    /// The descriptor's storage codec is always validated here. Finite enum
    /// and UI-choice domains live in the generated [`MenuField`] metadata; if
    /// this descriptor names a generated menu field, those domains are also
    /// enforced so callers cannot bypass [`MenuField::plan_value`] by passing
    /// its descriptor directly. A descriptor that reuses a generated field
    /// name but does not exactly match its catalog entry is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::DisallowedValue`] for a generated field value
    /// outside its finite enum or UI-choice domain, and
    /// [`SchemaError::CatalogDescriptorMismatch`] when generated catalog
    /// metadata has been altered. Other errors cover type mismatches,
    /// out-of-range values, oversized or padding-ambiguous text, invalid byte
    /// input, malformed descriptors, and conflicting overlapping patches.
    ///
    /// [`MenuField`]: super::MenuField
    /// [`MenuField::plan_value`]: super::MenuField::plan_value
    pub fn set(
        &mut self,
        field: &FieldDescriptor,
        value: FieldValue<'_>,
    ) -> Result<&mut Self, SchemaError> {
        if let Some(menu_field) = super::menu_field(field.name) {
            if menu_field.descriptor != *field {
                return Err(SchemaError::CatalogDescriptorMismatch {
                    field: field.name,
                    offset: field.offset,
                    expected_offset: menu_field.descriptor.offset,
                });
            }
            menu_field.validate_patch_value(value)?;
        }
        let encoded = encode_field(field, value, ValueDomain::Writable)?;
        for (absolute, mask, bits) in encoded {
            self.merge_byte(field.name, absolute, mask, bits)?;
        }
        Ok(self)
    }

    /// Finish and return patches grouped by ascending MCP page.
    ///
    /// The complete plan is validated against the radio's address space
    /// before any patch is produced, so a [`PatchSet`] can never address
    /// bytes outside the real memory image or inside the factory-calibration
    /// region.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OutOfBounds`] for a patch beyond the radio's
    /// memory image, and [`SchemaError::WriteProtected`] for a patch inside
    /// the factory-calibration region.
    pub fn finish(self) -> Result<PatchSet, SchemaError> {
        let mut pages: BTreeMap<WritableMcpPage, Vec<BytePatch>> = BTreeMap::new();
        for (absolute, claim) in self.bytes {
            if absolute >= programming::TOTAL_SIZE {
                return Err(SchemaError::OutOfBounds {
                    field: claim.owner,
                    offset: absolute,
                    len: 1,
                    image_len: programming::TOTAL_SIZE,
                });
            }
            let page_number = absolute / programming::PAGE_SIZE;
            let page = u16::try_from(page_number).map_err(|_| SchemaError::OffsetTooLarge {
                field: claim.owner,
                offset: absolute,
            })?;
            let physical_page = McpPage::new(page).map_err(|_| SchemaError::OutOfBounds {
                field: claim.owner,
                offset: absolute,
                len: 1,
                image_len: programming::TOTAL_SIZE,
            })?;
            let writable_page = WritableMcpPage::from_page(physical_page).map_err(|_| {
                SchemaError::WriteProtected {
                    field: claim.owner,
                    page: physical_page,
                }
            })?;
            let in_page = absolute % programming::PAGE_SIZE;
            let offset = u8::try_from(in_page).map_err(|_| SchemaError::OffsetTooLarge {
                field: claim.owner,
                offset: absolute,
            })?;
            pages.entry(writable_page).or_default().push(BytePatch {
                offset,
                mask: claim.mask,
                value: claim.value,
            });
        }
        Ok(PatchSet {
            pages: pages
                .into_iter()
                .map(|(page, bytes)| PagePatch { page, bytes })
                .collect(),
        })
    }

    fn merge_byte(
        &mut self,
        owner: &'static str,
        offset: usize,
        mask: u8,
        value: u8,
    ) -> Result<(), SchemaError> {
        if let Some(claim) = self.bytes.get_mut(&offset) {
            let overlap = claim.mask & mask;
            if ((claim.value ^ value) & overlap) != 0 {
                return Err(SchemaError::PatchConflict {
                    field: owner,
                    existing: claim.owner,
                    offset,
                    mask: overlap,
                });
            }
            claim.value = (claim.value & !mask) | (value & mask);
            claim.mask |= mask;
        } else {
            let _previous = self.bytes.insert(
                offset,
                ByteClaim {
                    mask,
                    value: value & mask,
                    owner,
                },
            );
        }
        Ok(())
    }
}

const fn type_mismatch(field: &FieldDescriptor, value: FieldValue<'_>) -> SchemaError {
    SchemaError::TypeMismatch {
        field: field.name,
        expected: field.codec.value_kind(),
        actual: value.kind_name(),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "The match is intentionally exhaustive over every FieldCodec/FieldValue pairing; \
              keeping validation beside each encoding makes unsupported pairings explicit."
)]
fn encode_field(
    field: &FieldDescriptor,
    value: FieldValue<'_>,
    domain: ValueDomain,
) -> Result<Vec<(usize, u8, u8)>, SchemaError> {
    match (field.codec, value) {
        (FieldCodec::Byte { min, max }, FieldValue::Unsigned(value)) => {
            let (accepted_min, accepted_max) = if domain == ValueDomain::Writable {
                (u64::from(min), u64::from(max))
            } else {
                (0, u64::from(u8::MAX))
            };
            validate_unsigned(field.name, value, accepted_min, accepted_max)?;
            let byte = u8::try_from(value).map_err(|_| SchemaError::UnsignedOutOfRange {
                field: field.name,
                value,
                min: accepted_min,
                max: accepted_max,
            })?;
            Ok(vec![(field.offset, u8::MAX, byte)])
        }
        (FieldCodec::Bool, FieldValue::Bool(value)) => {
            Ok(vec![(field.offset, u8::MAX, u8::from(value))])
        }
        (FieldCodec::BitBool { mask }, FieldValue::Bool(value)) => {
            validate_bool_mask(field.name, mask)?;
            Ok(vec![(field.offset, mask, if value { mask } else { 0 })])
        }
        (
            FieldCodec::BitField {
                mask,
                shift,
                min,
                max,
            },
            FieldValue::Unsigned(value),
        ) => {
            validate_bit_codec(field.name, mask, shift, min, max)?;
            let (accepted_min, accepted_max) = if domain == ValueDomain::Writable {
                (u64::from(min), u64::from(max))
            } else {
                (0, u64::from(mask >> shift))
            };
            validate_unsigned(field.name, value, accepted_min, accepted_max)?;
            let byte = u8::try_from(value).map_err(|_| SchemaError::UnsignedOutOfRange {
                field: field.name,
                value,
                min: accepted_min,
                max: accepted_max,
            })?;
            let shifted = (byte << shift) & mask;
            Ok(vec![(field.offset, mask, shifted)])
        }
        (
            FieldCodec::FixedString {
                len,
                encoding,
                padding,
            },
            FieldValue::Text(text),
        ) => {
            let bytes = text.as_bytes();
            if bytes.len() > len {
                return Err(SchemaError::TextTooLong {
                    field: field.name,
                    actual: bytes.len(),
                    max: len,
                });
            }
            if padding == 0 {
                if let Some(offset) = bytes.iter().position(|&byte| byte == 0) {
                    return Err(SchemaError::TextContainsNul {
                        field: field.name,
                        offset,
                    });
                }
            } else if let Some((offset, &last_byte)) = bytes.iter().enumerate().next_back()
                && last_byte == padding
            {
                return Err(SchemaError::TextEndsWithPadding {
                    field: field.name,
                    offset,
                    padding,
                });
            }
            if encoding == StringEncoding::MemoryMap
                && let Some((offset, &value)) = bytes
                    .iter()
                    .enumerate()
                    .find(|(_, value)| !is_printable_ascii(**value))
            {
                return Err(SchemaError::InvalidMemoryMapTextByte {
                    field: field.name,
                    offset,
                    value,
                });
            }
            let mut result = Vec::with_capacity(len);
            for index in 0..len {
                let byte = bytes.get(index).copied().unwrap_or(padding);
                let offset = checked_offset(field, index)?;
                result.push((offset, u8::MAX, byte));
            }
            Ok(result)
        }
        (
            FieldCodec::Unsigned {
                width,
                endian,
                min,
                max,
            },
            FieldValue::Unsigned(value),
        ) => {
            let width = validate_width(field.name, width)?;
            validate_unsigned_capacity(field.name, width, max)?;
            let (accepted_min, accepted_max) = if domain == ValueDomain::Writable {
                (min, max)
            } else {
                (0, unsigned_storage_max(width))
            };
            validate_unsigned(field.name, value, accepted_min, accepted_max)?;
            let bytes = match endian {
                Endian::Little => value.to_le_bytes(),
                Endian::Big => value.to_be_bytes(),
            };
            encode_integer_bytes(field, bytes, width, endian)
        }
        (
            FieldCodec::Signed {
                width,
                endian,
                min,
                max,
            },
            FieldValue::Signed(value),
        ) => {
            let width = validate_width(field.name, width)?;
            validate_signed_capacity(field.name, width, min, max)?;
            let (accepted_min, accepted_max) = if domain == ValueDomain::Writable {
                (min, max)
            } else {
                signed_storage_bounds(width)
            };
            validate_signed(field.name, value, accepted_min, accepted_max)?;
            let bytes = match endian {
                Endian::Little => value.to_le_bytes(),
                Endian::Big => value.to_be_bytes(),
            };
            encode_integer_bytes(field, bytes, width, endian)
        }
        (FieldCodec::Bytes { len }, FieldValue::Bytes(bytes)) => {
            if bytes.len() != len {
                return Err(SchemaError::ByteLength {
                    field: field.name,
                    actual: bytes.len(),
                    expected: len,
                });
            }
            bytes
                .iter()
                .copied()
                .enumerate()
                .map(|(index, byte)| Ok((checked_offset(field, index)?, u8::MAX, byte)))
                .collect()
        }
        (_, other) => Err(type_mismatch(field, other)),
    }
}

fn decode_fixed_string_bytes<'a>(
    field: &'static str,
    field_offset: usize,
    image_len: usize,
    bytes: &'a [u8],
    padding: u8,
) -> Result<&'a [u8], SchemaError> {
    let end = if padding == 0 {
        let Some(terminator_offset) = bytes.iter().position(|&byte| byte == 0) else {
            return Ok(bytes);
        };
        if let Some((offset, &value)) = bytes
            .iter()
            .enumerate()
            .skip(terminator_offset + 1)
            .find(|(_, byte)| **byte != 0)
        {
            return Err(SchemaError::FixedStringDataAfterNul {
                field,
                terminator_offset,
                offset,
                value,
            });
        }
        terminator_offset
    } else {
        bytes
            .iter()
            .rposition(|&byte| byte != padding)
            .map_or(0, |index| index + 1)
    };

    bytes.get(..end).ok_or(SchemaError::OutOfBounds {
        field,
        offset: field_offset,
        len: bytes.len(),
        image_len,
    })
}

const fn is_printable_ascii(value: u8) -> bool {
    value == b' ' || value.is_ascii_graphic()
}

fn encode_integer_bytes(
    field: &FieldDescriptor,
    bytes: [u8; 8],
    width: IntegerWidth,
    endian: Endian,
) -> Result<Vec<(usize, u8, u8)>, SchemaError> {
    let selected = match endian {
        Endian::Little => bytes.get(..width.bytes()),
        Endian::Big => bytes.get(width.start_in_full_width()..),
    }
    .ok_or(SchemaError::InvalidIntegerWidth {
        field: field.name,
        width: width.0,
    })?;
    selected
        .iter()
        .copied()
        .enumerate()
        .map(|(index, byte)| Ok((checked_offset(field, index)?, u8::MAX, byte)))
        .collect()
}

fn checked_offset(field: &FieldDescriptor, relative: usize) -> Result<usize, SchemaError> {
    field
        .offset
        .checked_add(relative)
        .ok_or(SchemaError::OffsetTooLarge {
            field: field.name,
            offset: field.offset,
        })
}

const fn validate_unsigned(
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Result<(), SchemaError> {
    if value < min || value > max {
        return Err(SchemaError::UnsignedOutOfRange {
            field,
            value,
            min,
            max,
        });
    }
    Ok(())
}

const fn validate_signed(
    field: &'static str,
    value: i64,
    min: i64,
    max: i64,
) -> Result<(), SchemaError> {
    if value < min || value > max {
        return Err(SchemaError::SignedOutOfRange {
            field,
            value,
            min,
            max,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntegerWidth(u8);

impl IntegerWidth {
    fn new(field: &'static str, width: u8) -> Result<Self, SchemaError> {
        if !(1..=8).contains(&width) {
            return Err(SchemaError::InvalidIntegerWidth { field, width });
        }
        Ok(Self(width))
    }

    fn bytes(self) -> usize {
        usize::from(self.0)
    }

    fn bits(self) -> u32 {
        u32::from(self.0) * 8
    }

    fn start_in_full_width(self) -> usize {
        8 - self.bytes()
    }
}

fn validate_width(field: &'static str, width: u8) -> Result<IntegerWidth, SchemaError> {
    IntegerWidth::new(field, width)
}

fn unsigned_storage_max(width: IntegerWidth) -> u64 {
    if width.0 == 8 {
        u64::MAX
    } else {
        (1_u64 << width.bits()) - 1
    }
}

fn signed_storage_bounds(width: IntegerWidth) -> (i64, i64) {
    if width.0 == 8 {
        (i64::MIN, i64::MAX)
    } else {
        let half = 1_i64 << (width.bits() - 1);
        (-half, half - 1)
    }
}

/// Reject an unsigned domain wider than the encoded byte width, which would
/// otherwise truncate silently.
fn validate_unsigned_capacity(
    field: &'static str,
    width: IntegerWidth,
    max: u64,
) -> Result<(), SchemaError> {
    if width.0 < 8 {
        let capacity = (1_u64 << width.bits()) - 1;
        if max > capacity {
            return Err(SchemaError::DomainExceedsWidth {
                field,
                width: width.0,
            });
        }
    }
    Ok(())
}

/// Reject a signed domain wider than the encoded byte width, which would
/// otherwise truncate silently.
fn validate_signed_capacity(
    field: &'static str,
    width: IntegerWidth,
    min: i64,
    max: i64,
) -> Result<(), SchemaError> {
    if width.0 < 8 {
        let half = 1_i64 << (width.bits() - 1);
        if min < -half || max > half - 1 {
            return Err(SchemaError::DomainExceedsWidth {
                field,
                width: width.0,
            });
        }
    }
    Ok(())
}

const fn validate_bit_codec(
    field: &'static str,
    mask: u8,
    shift: u8,
    min: u8,
    max: u8,
) -> Result<(), SchemaError> {
    let shifted = if shift < 8 { mask >> shift } else { 0 };
    let lower_mask = if shift == 0 || shift >= 8 {
        0
    } else {
        u8::MAX >> (8 - shift)
    };
    if mask == 0
        || shift >= 8
        || mask & lower_mask != 0
        || shifted & shifted.wrapping_add(1) != 0
        || min > max
        || max > shifted
    {
        return Err(SchemaError::InvalidBitField { field, mask, shift });
    }
    Ok(())
}

const fn validate_bool_mask(field: &'static str, mask: u8) -> Result<(), SchemaError> {
    if mask.count_ones() != 1 {
        return Err(SchemaError::InvalidBitField {
            field,
            mask,
            shift: 0,
        });
    }
    Ok(())
}

fn read_byte(image: &[u8], field: &'static str, offset: usize) -> Result<u8, SchemaError> {
    image.get(offset).copied().ok_or(SchemaError::OutOfBounds {
        field,
        offset,
        len: 1,
        image_len: image.len(),
    })
}

fn read_range<'a>(
    image: &'a [u8],
    field: &'static str,
    offset: usize,
    len: usize,
) -> Result<&'a [u8], SchemaError> {
    let end = offset
        .checked_add(len)
        .ok_or(SchemaError::OffsetTooLarge { field, offset })?;
    image.get(offset..end).ok_or(SchemaError::OutOfBounds {
        field,
        offset,
        len,
        image_len: image.len(),
    })
}

fn decode_unsigned(bytes: &[u8], endian: Endian) -> u64 {
    match endian {
        Endian::Little => bytes
            .iter()
            .rev()
            .fold(0_u64, |value, &byte| (value << 8) | u64::from(byte)),
        Endian::Big => bytes
            .iter()
            .fold(0_u64, |value, &byte| (value << 8) | u64::from(byte)),
    }
}

fn decode_signed(bytes: &[u8], width: IntegerWidth, endian: Endian) -> i64 {
    let unsigned = decode_unsigned(bytes, endian);
    let bit_count = width.bits();
    if bit_count == 64 {
        return i64::from_ne_bytes(unsigned.to_ne_bytes());
    }
    let sign_bit = 1_u64 << (bit_count - 1);
    let extended = if unsigned & sign_bit == 0 {
        unsigned
    } else {
        unsigned | (u64::MAX << bit_count)
    };
    i64::from_ne_bytes(extended.to_ne_bytes())
}

impl super::menu_fields::StorageTransform {
    /// Convert a stored raw integer into its display-unit value, rounded to
    /// one decimal place (the official application's display precision).
    ///
    /// Returns `None` when the transform's numerator is zero (malformed
    /// metadata) or the raw value exceeds the exactly-representable integer
    /// range of `f64`.
    #[must_use]
    pub fn decode_display(&self, raw: u64) -> Option<f64> {
        const EXACT_INTEGER_BOUND: u64 = 1 << 53;
        if self.numerator == 0 || raw > EXACT_INTEGER_BOUND {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "raw is bounded to 2^53 above, and generated numerators and denominators \
                      are small unit ratios, so every conversion here is exact"
        )]
        let value = (raw as f64) * (self.denominator as f64) / (self.numerator as f64);
        Some((value * 10.0).round() / 10.0)
    }

    /// Convert a display-unit value into the stored raw integer:
    /// `round(display * numerator / denominator)`.
    ///
    /// Returns `None` for a non-finite input, a zero denominator, a negative
    /// result, or a result beyond the exactly-representable integer range of
    /// `f64`. Field-domain validation stays with the patch planner:
    /// [`PatchPlanner::set`] enforces generated domains on the encoded value.
    #[must_use]
    pub fn encode_display(&self, display: f64) -> Option<u64> {
        /// Largest f64 value that is still an exactly-representable integer.
        const EXACT_INTEGER_BOUND: f64 = 9_007_199_254_740_992.0;
        if !display.is_finite() || self.denominator == 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "generated numerators and denominators are small unit ratios; the \
                      conversion is exact"
        )]
        let encoded = (display * self.numerator as f64 / self.denominator as f64).round();
        if !(0.0..=EXACT_INTEGER_BOUND).contains(&encoded) {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "encoded is a rounded integer proven within 0..=2^53 by the containment \
                      check above"
        )]
        Some(encoded as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const ENABLE: FieldDescriptor = FieldDescriptor::new(
        "test.enable",
        0x1010,
        FieldCodec::BitField {
            mask: 0b0000_0100,
            shift: 2,
            min: 0,
            max: 1,
        },
    );

    #[test]
    fn field_page_is_physical_and_bounds_checked() -> TestResult {
        assert_eq!(ENABLE.page()?.as_raw(), 0x10);

        let beyond = FieldDescriptor::new(
            "test.beyond",
            programming::TOTAL_SIZE,
            FieldCodec::Byte { min: 0, max: 255 },
        );
        assert!(matches!(
            beyond.page(),
            Err(SchemaError::OutOfBounds {
                field: "test.beyond",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn bit_patch_preserves_fresh_unrelated_bits() -> TestResult {
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&ENABLE, FieldValue::Unsigned(1))?;
        let patches = planner.finish()?;
        let page_patch = patches
            .page(WritableMcpPage::new(0x10)?)
            .ok_or("page 0x10 missing")?;
        let mut page = [0b1010_0011; programming::PAGE_SIZE];
        page_patch.apply_to_page(&mut page);
        assert_eq!(page.get(0x10), Some(&0b1010_0111));
        Ok(())
    }

    #[test]
    fn boolean_bit_patch_uses_boolean_values() -> TestResult {
        let field = FieldDescriptor::new(
            "test.boolean_bit",
            0x1010,
            FieldCodec::BitBool { mask: 0b0000_0100 },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Bool(true))?;
        let patches = planner.finish()?;
        let mut page = [0b1010_0011; programming::PAGE_SIZE];
        patches
            .page(WritableMcpPage::new(0x10)?)
            .ok_or("page 0x10 missing")?
            .apply_to_page(&mut page);
        assert_eq!(page.get(0x10), Some(&0b1010_0111));
        let mut image = vec![0; 0x1011];
        if let Some(byte) = image.get_mut(0x1010) {
            *byte = 0b0000_0100;
        }
        assert_eq!(field.read(&image)?, DecodedFieldValue::Bool(true));
        Ok(())
    }

    #[test]
    fn independent_bits_coalesce_and_conflicts_fail() -> TestResult {
        let second = FieldDescriptor::new(
            "test.second",
            ENABLE.offset,
            FieldCodec::BitField {
                mask: 0b0000_1000,
                shift: 3,
                min: 0,
                max: 1,
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner
            .set(&ENABLE, FieldValue::Unsigned(1))?
            .set(&second, FieldValue::Unsigned(1))?;
        let patches = planner.finish()?;
        let bytes = patches
            .page(WritableMcpPage::new(0x10)?)
            .ok_or("page missing")?
            .bytes();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes.first().map(|patch| patch.mask()), Some(0x0C));
        assert_eq!(bytes.first().map(|patch| patch.as_raw()), Some(0x0C));

        let contradictory = FieldDescriptor::new(
            "test.contradictory",
            ENABLE.offset,
            FieldCodec::BitField {
                mask: 0b0000_0100,
                shift: 2,
                min: 0,
                max: 1,
            },
        );
        let mut conflict = PatchPlanner::new();
        let _planner = conflict.set(&ENABLE, FieldValue::Unsigned(1))?;
        let result = conflict.set(&contradictory, FieldValue::Unsigned(0));
        assert!(
            matches!(
                result,
                Err(SchemaError::PatchConflict {
                    field: "test.contradictory",
                    existing: "test.enable",
                    ..
                })
            ),
            "conflict must name both fields: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn string_crosses_page_and_is_padded() -> TestResult {
        let field = FieldDescriptor::new(
            "test.text",
            0x10FE,
            FieldCodec::FixedString {
                len: 5,
                encoding: StringEncoding::Utf8,
                padding: 0,
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Text("OK"))?;
        let patches = planner.finish()?;
        assert_eq!(
            patches
                .pages()
                .map(WritableMcpPage::as_raw)
                .collect::<Vec<_>>(),
            vec![0x10, 0x11]
        );
        let mut image = vec![0xFF; 0x1200];
        patches.apply_to_image(&mut image)?;
        assert_eq!(image.get(0x10FE..0x1103), Some(&b"OK\0\0\0"[..]));
        assert_eq!(field.read(&image)?, DecodedFieldValue::Text("OK".into()));
        Ok(())
    }

    #[test]
    fn nul_padded_string_rejects_embedded_nul_on_write() {
        let field = FieldDescriptor::new(
            "test.nul_text",
            0,
            FieldCodec::FixedString {
                len: 5,
                encoding: StringEncoding::Utf8,
                padding: 0,
            },
        );
        let mut planner = PatchPlanner::new();
        let result = planner.set(&field, FieldValue::Text("A\0B"));

        assert!(
            matches!(
                result,
                Err(SchemaError::TextContainsNul {
                    field: "test.nul_text",
                    offset: 1,
                })
            ),
            "embedded NUL must not be accepted as semantic text: {result:?}"
        );
    }

    #[test]
    fn nul_padded_string_rejects_image_data_after_terminator() {
        let field = FieldDescriptor::new(
            "test.nul_image",
            0,
            FieldCodec::FixedString {
                len: 5,
                encoding: StringEncoding::Utf8,
                padding: 0,
            },
        );
        let image = *b"A\0B\0\0";
        let result = field.read(&image);

        assert!(
            matches!(
                result,
                Err(SchemaError::FixedStringDataAfterNul {
                    field: "test.nul_image",
                    terminator_offset: 1,
                    offset: 2,
                    value: b'B',
                })
            ),
            "non-NUL data after the terminator must be rejected: {result:?}"
        );
    }

    #[test]
    fn space_padded_string_rejects_semantic_trailing_space() {
        let field = FieldDescriptor::new(
            "test.space_text",
            0,
            FieldCodec::FixedString {
                len: 5,
                encoding: StringEncoding::Utf8,
                padding: b' ',
            },
        );
        let mut planner = PatchPlanner::new();
        let result = planner.set(&field, FieldValue::Text("AB "));

        assert!(
            matches!(
                result,
                Err(SchemaError::TextEndsWithPadding {
                    field: "test.space_text",
                    offset: 2,
                    padding: b' ',
                })
            ),
            "semantic trailing space would be lost on read: {result:?}"
        );
    }

    #[test]
    fn space_padded_string_preserves_interior_space() -> TestResult {
        let field = FieldDescriptor::new(
            "test.interior_space",
            0,
            FieldCodec::FixedString {
                len: 6,
                encoding: StringEncoding::Utf8,
                padding: b' ',
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Text("A B"))?;
        let patches = planner.finish()?;
        let mut image = [0_u8; programming::PAGE_SIZE];
        patches.apply_to_image(&mut image)?;

        assert_eq!(image.get(..6), Some(&b"A B   "[..]));
        assert_eq!(field.read(&image)?, DecodedFieldValue::Text("A B".into()));
        Ok(())
    }

    #[test]
    fn fixed_string_exact_full_width_round_trips_without_terminator() -> TestResult {
        let field = FieldDescriptor::new(
            "test.full_width",
            0,
            FieldCodec::FixedString {
                len: 5,
                encoding: StringEncoding::Utf8,
                padding: 0,
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Text("A BCD"))?;
        let patches = planner.finish()?;
        let mut image = [0_u8; programming::PAGE_SIZE];
        patches.apply_to_image(&mut image)?;

        assert_eq!(image.get(..5), Some(&b"A BCD"[..]));
        assert_eq!(field.read(&image)?, DecodedFieldValue::Text("A BCD".into()));
        Ok(())
    }

    #[test]
    fn memory_map_text_accepts_exact_printable_ascii_boundaries() -> TestResult {
        let field = FieldDescriptor::new(
            "test.memory_map_text",
            0,
            FieldCodec::FixedString {
                len: 4,
                encoding: StringEncoding::MemoryMap,
                padding: 0,
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Text(" ~"))?;
        let patches = planner.finish()?;
        let mut image = [0_u8; programming::PAGE_SIZE];
        patches.apply_to_image(&mut image)?;

        assert_eq!(image.get(..4), Some(&b" ~\0\0"[..]));
        assert_eq!(field.read(&image)?, DecodedFieldValue::Text(" ~".into()));
        Ok(())
    }

    #[test]
    fn memory_map_text_rejects_non_printable_input_without_mutating_the_plan() -> TestResult {
        let existing = FieldDescriptor::new("test.existing", 0, FieldCodec::Bool);
        let text = FieldDescriptor::new(
            "test.memory_map_text",
            8,
            FieldCodec::FixedString {
                len: 4,
                encoding: StringEncoding::MemoryMap,
                padding: 0,
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&existing, FieldValue::Bool(true))?;

        for (value, offset, byte) in [("A\n", 1, b'\n'), ("\u{7f}", 0, 0x7F), ("✓", 0, 0xE2)] {
            let result = planner.set(&text, FieldValue::Text(value));
            assert!(matches!(
                result,
                Err(SchemaError::InvalidMemoryMapTextByte {
                    field: "test.memory_map_text",
                    offset: actual_offset,
                    value: actual_value,
                }) if actual_offset == offset && actual_value == byte
            ));
        }

        let patches = planner.finish()?;
        let mut image = [0_u8; programming::PAGE_SIZE];
        patches.apply_to_image(&mut image)?;
        assert_eq!(image.first(), Some(&1));
        assert_eq!(image.get(8..12), Some(&[0; 4][..]));
        Ok(())
    }

    #[test]
    fn memory_map_text_read_reports_the_exact_non_printable_byte() {
        let field = FieldDescriptor::new(
            "test.memory_map_text",
            0,
            FieldCodec::FixedString {
                len: 4,
                encoding: StringEncoding::MemoryMap,
                padding: 0,
            },
        );
        let image = [b'A', 0x1F, 0, 0];

        assert!(matches!(
            field.read(&image),
            Err(SchemaError::InvalidMemoryMapTextByte {
                field: "test.memory_map_text",
                offset: 1,
                value: 0x1F,
            })
        ));
    }

    #[test]
    fn nonstandard_padding_rejects_semantic_trailing_padding_byte() {
        let field = FieldDescriptor::new(
            "test.other_padding",
            0,
            FieldCodec::FixedString {
                len: 5,
                encoding: StringEncoding::Utf8,
                padding: b'~',
            },
        );
        let mut planner = PatchPlanner::new();
        let result = planner.set(&field, FieldValue::Text("END~"));

        assert!(
            matches!(
                result,
                Err(SchemaError::TextEndsWithPadding {
                    field: "test.other_padding",
                    offset: 3,
                    padding: b'~',
                })
            ),
            "semantic trailing padding would be lost on read: {result:?}"
        );
    }

    #[test]
    fn integers_round_trip_in_both_orders() -> TestResult {
        let little = FieldDescriptor::new(
            "test.le",
            2,
            FieldCodec::Unsigned {
                width: 2,
                endian: Endian::Little,
                min: 0,
                max: u64::from(u16::MAX),
            },
        );
        let big = FieldDescriptor::new(
            "test.be",
            4,
            FieldCodec::Signed {
                width: 2,
                endian: Endian::Big,
                min: i64::from(i16::MIN),
                max: i64::from(i16::MAX),
            },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner
            .set(&little, FieldValue::Unsigned(0x1234))?
            .set(&big, FieldValue::Signed(-2))?;
        let patches = planner.finish()?;
        let mut image = vec![0; 256];
        patches.apply_to_image(&mut image)?;
        assert_eq!(image.get(2..6), Some(&[0x34, 0x12, 0xFF, 0xFE][..]));
        assert_eq!(little.read(&image)?, DecodedFieldValue::Unsigned(0x1234));
        assert_eq!(big.read(&image)?, DecodedFieldValue::Signed(-2));
        Ok(())
    }

    #[test]
    fn integer_codecs_reject_invalid_widths_before_accessing_image_data() {
        let zero_width = FieldDescriptor::new(
            "test.zero_width",
            0,
            FieldCodec::Unsigned {
                width: 0,
                endian: Endian::Little,
                min: 0,
                max: 0,
            },
        );
        let oversized = FieldDescriptor::new(
            "test.oversized_width",
            0,
            FieldCodec::Signed {
                width: 9,
                endian: Endian::Big,
                min: 0,
                max: 0,
            },
        );

        for (field, value, expected_width) in [
            (zero_width, FieldValue::Unsigned(0), 0),
            (oversized, FieldValue::Signed(0), 9),
        ] {
            let read = field.read(&[]);
            assert!(
                matches!(
                    read,
                    Err(SchemaError::InvalidIntegerWidth {
                        field: error_field,
                        width,
                    }) if error_field == field.name && width == expected_width
                ),
                "invalid width must be rejected before reading the image: {read:?}"
            );

            let mut planner = PatchPlanner::new();
            let write = planner.set(&field, value);
            assert!(
                matches!(
                    write,
                    Err(SchemaError::InvalidIntegerWidth {
                        field: error_field,
                        width,
                    }) if error_field == field.name && width == expected_width
                ),
                "invalid width must be rejected before encoding: {write:?}"
            );
        }
    }

    #[test]
    fn validation_rejects_bad_values_and_lengths() {
        let byte = FieldDescriptor::new("test.byte", 0, FieldCodec::Byte { min: 1, max: 3 });
        let bytes = FieldDescriptor::new("test.bytes", 0, FieldCodec::Bytes { len: 2 });
        let mut planner = PatchPlanner::new();
        assert!(matches!(
            planner.set(&byte, FieldValue::Unsigned(4)),
            Err(SchemaError::UnsignedOutOfRange { .. })
        ));
        assert!(matches!(
            planner.set(&byte, FieldValue::Bool(true)),
            Err(SchemaError::TypeMismatch { .. })
        ));
        assert!(matches!(
            planner.set(&bytes, FieldValue::Bytes(&[1])),
            Err(SchemaError::ByteLength { .. })
        ));
    }

    #[test]
    fn reads_reject_values_outside_declared_domains() {
        let byte = FieldDescriptor::new("test.byte", 0, FieldCodec::Byte { min: 1, max: 3 });
        let boolean = FieldDescriptor::new("test.bool", 1, FieldCodec::Bool);
        let bit_field = FieldDescriptor::new(
            "test.bits",
            2,
            FieldCodec::BitField {
                mask: 0b0000_1100,
                shift: 2,
                min: 1,
                max: 2,
            },
        );
        let unsigned = FieldDescriptor::new(
            "test.unsigned",
            3,
            FieldCodec::Unsigned {
                width: 2,
                endian: Endian::Little,
                min: 10,
                max: 20,
            },
        );
        let signed = FieldDescriptor::new(
            "test.signed",
            5,
            FieldCodec::Signed {
                width: 1,
                endian: Endian::Little,
                min: -2,
                max: 2,
            },
        );
        let image = [4, 2, 0, 9, 0, 3];

        for field in [byte, boolean, bit_field, unsigned] {
            assert!(matches!(
                field.read(&image),
                Err(SchemaError::UnsignedOutOfRange { .. })
            ));
        }
        assert!(matches!(
            signed.read(&image),
            Err(SchemaError::SignedOutOfRange { .. })
        ));
    }

    #[test]
    fn finish_rejects_offsets_beyond_the_radio_image() -> TestResult {
        let field = FieldDescriptor::new(
            "test.beyond",
            programming::TOTAL_SIZE,
            FieldCodec::Byte { min: 0, max: 255 },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Unsigned(1))?;
        let result = planner.finish();
        assert!(
            matches!(
                result,
                Err(SchemaError::OutOfBounds {
                    field: "test.beyond",
                    ..
                })
            ),
            "plan-time bounds check must name the field: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn finish_rejects_factory_calibration_pages() -> TestResult {
        let field = FieldDescriptor::new(
            "test.calibration",
            0x7A100,
            FieldCodec::Byte { min: 0, max: 255 },
        );
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&field, FieldValue::Unsigned(1))?;
        let result = planner.finish();
        assert!(
            matches!(
                result,
                Err(SchemaError::WriteProtected {
                    field: "test.calibration",
                    page,
                }) if page.as_raw() == 0x7A1
            ),
            "calibration pages must be rejected at plan time: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn apply_to_image_never_partially_patches_on_error() -> TestResult {
        let low = FieldDescriptor::new("test.low", 0x1000, FieldCodec::Byte { min: 0, max: 255 });
        let high = FieldDescriptor::new("test.high", 0x1200, FieldCodec::Byte { min: 0, max: 255 });
        let mut planner = PatchPlanner::new();
        let _planner = planner
            .set(&low, FieldValue::Unsigned(0xA5))?
            .set(&high, FieldValue::Unsigned(0x5A))?;
        let patches = planner.finish()?;

        // The image ends between the two patched bytes, so the set as a
        // whole is out of bounds and nothing may change.
        let mut image = vec![0_u8; 0x1100];
        let result = patches.apply_to_image(&mut image);
        assert!(
            matches!(result, Err(SchemaError::OutOfBounds { .. })),
            "short image must be rejected: {result:?}"
        );
        assert_eq!(
            image.get(0x1000),
            Some(&0),
            "no byte may change when any patch is out of bounds"
        );
        Ok(())
    }

    #[test]
    fn integer_domains_must_fit_their_declared_width() {
        let unsigned = FieldDescriptor::new(
            "test.wide_unsigned",
            0,
            FieldCodec::Unsigned {
                width: 1,
                endian: Endian::Little,
                min: 0,
                max: 300,
            },
        );
        let signed = FieldDescriptor::new(
            "test.wide_signed",
            0,
            FieldCodec::Signed {
                width: 2,
                endian: Endian::Little,
                min: -40_000,
                max: 0,
            },
        );
        let mut planner = PatchPlanner::new();
        let unsigned_result = planner.set(&unsigned, FieldValue::Unsigned(260));
        assert!(
            matches!(
                unsigned_result,
                Err(SchemaError::DomainExceedsWidth {
                    field: "test.wide_unsigned",
                    width: 1,
                })
            ),
            "an over-wide unsigned domain must not truncate: {unsigned_result:?}"
        );
        let signed_result = planner.set(&signed, FieldValue::Signed(-40_000));
        assert!(
            matches!(
                signed_result,
                Err(SchemaError::DomainExceedsWidth {
                    field: "test.wide_signed",
                    width: 2,
                })
            ),
            "an over-wide signed domain must not truncate: {signed_result:?}"
        );
    }

    #[test]
    fn encoded_len_covers_every_codec_shape() {
        assert_eq!(FieldCodec::Byte { min: 0, max: 5 }.encoded_len(), 1);
        assert_eq!(FieldCodec::Bool.encoded_len(), 1);
        assert_eq!(FieldCodec::BitBool { mask: 0x08 }.encoded_len(), 1);
        assert_eq!(
            FieldCodec::BitField {
                mask: 0x30,
                shift: 4,
                min: 0,
                max: 3
            }
            .encoded_len(),
            1
        );
        assert_eq!(
            FieldCodec::FixedString {
                len: 16,
                encoding: StringEncoding::Utf8,
                padding: 0
            }
            .encoded_len(),
            16
        );
        assert_eq!(
            FieldCodec::Unsigned {
                width: 4,
                endian: Endian::Little,
                min: 0,
                max: 100
            }
            .encoded_len(),
            4
        );
        assert_eq!(
            FieldCodec::Signed {
                width: 2,
                endian: Endian::Big,
                min: -5,
                max: 5
            }
            .encoded_len(),
            2
        );
        assert_eq!(FieldCodec::Bytes { len: 300 }.encoded_len(), 300);
    }

    #[test]
    fn pages_lists_the_whole_span_in_ascending_order() -> TestResult {
        let single = FieldDescriptor::new("test.single", 10, FieldCodec::Bool);
        assert_eq!(single.pages()?, vec![McpPage::new(0)?]);

        let straddles = FieldDescriptor::new(
            "test.straddle",
            programming::PAGE_SIZE - 1,
            FieldCodec::Bytes { len: 2 },
        );
        assert_eq!(straddles.pages()?, vec![McpPage::new(0)?, McpPage::new(1)?]);

        let wide = FieldDescriptor::new("test.wide_span", 0, FieldCodec::Bytes { len: 300 });
        assert_eq!(wide.pages()?, vec![McpPage::new(0)?, McpPage::new(1)?]);

        let outside =
            FieldDescriptor::new("test.outside", programming::TOTAL_SIZE, FieldCodec::Bool);
        let outside_result = outside.pages();
        assert!(
            matches!(outside_result, Err(SchemaError::OutOfBounds { .. })),
            "a span outside the image must be refused: {outside_result:?}"
        );
        Ok(())
    }

    #[test]
    fn storage_transform_round_trips_the_documented_scaling() -> TestResult {
        use crate::memory::menu_fields::StorageTransform;

        // A real generated ratio: stored per-minute rate for a
        // seconds-denominated display value.
        let per_minute = StorageTransform {
            input_unit: "seconds",
            numerator: 10_000,
            denominator: 60,
        };
        assert_eq!(per_minute.encode_display(3.0), Some(500));
        let decoded = per_minute
            .decode_display(500)
            .ok_or("decode of an in-range raw value must succeed")?;
        assert!(
            (decoded - 3.0).abs() < f64::EPSILON,
            "raw 500 must decode to 3.0 seconds, got {decoded}"
        );

        let zero_numerator = StorageTransform {
            input_unit: "x",
            numerator: 0,
            denominator: 60,
        };
        assert_eq!(zero_numerator.decode_display(1), None);
        let zero_denominator = StorageTransform {
            input_unit: "x",
            numerator: 10,
            denominator: 0,
        };
        assert_eq!(zero_denominator.encode_display(1.0), None);
        assert_eq!(per_minute.encode_display(f64::NAN), None);
        assert_eq!(per_minute.encode_display(-1.0), None);
        Ok(())
    }
}
