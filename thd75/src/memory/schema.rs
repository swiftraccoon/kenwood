//! Schema-driven MCP menu fields and safe masked patch planning.
//!
//! The official MCP-D75 application serializes menu properties into a raw
//! 500,480-byte image.  [`FieldDescriptor`] models those serializer writes,
//! while [`PatchPlanner`] converts requested values into byte masks that can
//! be applied to freshly-read radio pages.  Bit fields therefore preserve
//! unrelated bits even when the caller does not hold a current full image.

use std::collections::BTreeMap;
use std::fmt;

use crate::protocol::programming;

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
    /// The patch engine accepts ASCII for this encoding.  Non-ASCII input is
    /// rejected because the official application switches between Windows-
    /// 1252 and Shift-JIS according to the radio model.
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

    /// MCP page containing the field's first byte.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OffsetTooLarge`] when the offset cannot be
    /// represented by the TH-D75's 16-bit page address.
    pub fn page(self) -> Result<u16, SchemaError> {
        let page = self.offset / programming::PAGE_SIZE;
        u16::try_from(page).map_err(|_| SchemaError::OffsetTooLarge {
            field: self.name,
            offset: self.offset,
        })
    }

    /// Decode this field from a complete raw MCP image.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-bounds field, malformed codec, or an
    /// invalid encoded string.
    pub fn read(self, image: &[u8]) -> Result<DecodedFieldValue, SchemaError> {
        match self.codec {
            FieldCodec::Byte { .. } => Ok(DecodedFieldValue::Unsigned(u64::from(read_byte(
                image,
                self.name,
                self.offset,
            )?))),
            FieldCodec::Bool => Ok(DecodedFieldValue::Bool(
                read_byte(image, self.name, self.offset)? != 0,
            )),
            FieldCodec::BitBool { mask } => {
                validate_bool_mask(self.name, mask)?;
                Ok(DecodedFieldValue::Bool(
                    read_byte(image, self.name, self.offset)? & mask != 0,
                ))
            }
            FieldCodec::BitField {
                mask,
                shift,
                min,
                max,
            } => {
                validate_bit_codec(self.name, mask, shift, min, max)?;
                let byte = read_byte(image, self.name, self.offset)?;
                Ok(DecodedFieldValue::Unsigned(u64::from(
                    (byte & mask) >> shift,
                )))
            }
            FieldCodec::FixedString {
                len,
                encoding,
                padding,
            } => {
                let bytes = read_range(image, self.name, self.offset, len)?;
                let end = bytes
                    .iter()
                    .rposition(|&byte| byte != padding)
                    .map_or(0, |index| index + 1);
                let trimmed = bytes.get(..end).ok_or(SchemaError::OutOfBounds {
                    field: self.name,
                    offset: self.offset,
                    len,
                    image_len: image.len(),
                })?;
                if encoding == StringEncoding::MemoryMap && !trimmed.is_ascii() {
                    return Err(SchemaError::UnsupportedMemoryMapText { field: self.name });
                }
                let text = std::str::from_utf8(trimmed)
                    .map_err(|_| SchemaError::InvalidText { field: self.name })?;
                Ok(DecodedFieldValue::Text(text.to_owned()))
            }
            FieldCodec::Unsigned { width, endian, .. } => {
                let width = validate_width(self.name, width)?;
                let bytes = read_range(image, self.name, self.offset, width)?;
                Ok(DecodedFieldValue::Unsigned(decode_unsigned(bytes, endian)))
            }
            FieldCodec::Signed { width, endian, .. } => {
                let width = validate_width(self.name, width)?;
                let bytes = read_range(image, self.name, self.offset, width)?;
                Ok(DecodedFieldValue::Signed(decode_signed(bytes, endian)))
            }
            FieldCodec::Bytes { len } => Ok(DecodedFieldValue::Bytes(
                read_range(image, self.name, self.offset, len)?.to_vec(),
            )),
        }
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
    const fn kind_name(self) -> &'static str {
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
    /// Text is too long for a fixed-width field.
    TextTooLong {
        /// Field name.
        field: &'static str,
        /// Encoded byte count.
        actual: usize,
        /// Maximum byte count.
        max: usize,
    },
    /// Non-ASCII text was supplied for a model-dependent memory-map field.
    UnsupportedMemoryMapText {
        /// Field name.
        field: &'static str,
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
    /// An offset cannot be represented by a 16-bit MCP page number.
    OffsetTooLarge {
        /// Field name.
        field: &'static str,
        /// Absolute byte offset.
        offset: usize,
    },
    /// Integer width is zero or greater than eight bytes.
    InvalidIntegerWidth {
        /// Field name.
        field: &'static str,
        /// Invalid width.
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
            Self::TextTooLong { field, actual, max } => {
                write!(f, "field {field} text is {actual} bytes (maximum {max})")
            }
            Self::UnsupportedMemoryMapText { field } => write!(
                f,
                "field {field} uses model-dependent text encoding; only ASCII is safe"
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
            } => write!(
                f,
                "field {field} range 0x{offset:X}..+{len} exceeds image length {image_len}"
            ),
            Self::OffsetTooLarge { field, offset } => {
                write!(
                    f,
                    "field {field} offset 0x{offset:X} exceeds MCP addressing"
                )
            }
            Self::InvalidIntegerWidth { field, width } => {
                write!(f, "field {field} has invalid integer width {width}")
            }
            Self::InvalidBitField { field, mask, shift } => write!(
                f,
                "field {field} has invalid bit field mask 0x{mask:02X}, shift {shift}"
            ),
            Self::PatchConflict { offset, mask } => write!(
                f,
                "conflicting patch at MCP offset 0x{offset:X}, mask 0x{mask:02X}"
            ),
        }
    }
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

    /// Desired values already positioned under [`mask`](Self::mask).
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

/// Masked changes for one 256-byte MCP page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePatch {
    page: u16,
    bytes: Vec<BytePatch>,
}

impl PagePatch {
    /// MCP page address.
    #[must_use]
    pub const fn page(&self) -> u16 {
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
                *byte = (*byte & !patch.mask) | (patch.value & patch.mask);
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

    /// Iterate over the sorted MCP page addresses.
    pub fn pages(&self) -> impl Iterator<Item = u16> + '_ {
        self.pages.iter().map(PagePatch::page)
    }

    /// Find the patch for one MCP page.
    #[must_use]
    pub fn page(&self, page: u16) -> Option<&PagePatch> {
        self.pages.iter().find(|patch| patch.page == page)
    }

    /// Apply every patch to a complete raw MCP image.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OutOfBounds`] if the image does not contain a
    /// touched byte.
    pub fn apply_to_image(&self, image: &mut [u8]) -> Result<(), SchemaError> {
        for page_patch in &self.pages {
            let page_start = usize::from(page_patch.page) * programming::PAGE_SIZE;
            for patch in &page_patch.bytes {
                let absolute = page_start + usize::from(patch.offset);
                let image_len = image.len();
                let byte = image.get_mut(absolute).ok_or(SchemaError::OutOfBounds {
                    field: "patch set",
                    offset: absolute,
                    len: 1,
                    image_len,
                })?;
                *byte = (*byte & !patch.mask) | (patch.value & patch.mask);
            }
        }
        Ok(())
    }
}

/// Builds a [`PatchSet`] without requiring a cached memory image.
#[derive(Debug, Default)]
pub struct PatchPlanner {
    bytes: BTreeMap<usize, (u8, u8)>,
}

impl PatchPlanner {
    /// Create an empty planner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: BTreeMap::new(),
        }
    }

    /// Add or replace a requested menu field value.
    ///
    /// Non-overlapping bits at the same byte are coalesced.  Overlapping bits
    /// may be repeated only when both assignments request the same value.
    ///
    /// # Errors
    ///
    /// Returns an error for a type mismatch, out-of-domain value, malformed
    /// descriptor, oversized value, or conflicting overlapping patch.
    pub fn set(
        &mut self,
        field: &FieldDescriptor,
        value: FieldValue<'_>,
    ) -> Result<&mut Self, SchemaError> {
        let encoded = encode_field(field, value)?;
        for (absolute, mask, bits) in encoded {
            self.merge_byte(absolute, mask, bits)?;
        }
        Ok(self)
    }

    /// Finish and return patches grouped by ascending MCP page.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OffsetTooLarge`] if a patch cannot be addressed
    /// by a 16-bit MCP page number.
    pub fn finish(self) -> Result<PatchSet, SchemaError> {
        let mut pages: BTreeMap<u16, Vec<BytePatch>> = BTreeMap::new();
        for (absolute, (mask, value)) in self.bytes {
            let page_number = absolute / programming::PAGE_SIZE;
            let page = u16::try_from(page_number).map_err(|_| SchemaError::OffsetTooLarge {
                field: "patch set",
                offset: absolute,
            })?;
            let in_page = absolute % programming::PAGE_SIZE;
            let offset = u8::try_from(in_page).map_err(|_| SchemaError::OffsetTooLarge {
                field: "patch set",
                offset: absolute,
            })?;
            pages.entry(page).or_default().push(BytePatch {
                offset,
                mask,
                value,
            });
        }
        Ok(PatchSet {
            pages: pages
                .into_iter()
                .map(|(page, bytes)| PagePatch { page, bytes })
                .collect(),
        })
    }

    fn merge_byte(&mut self, offset: usize, mask: u8, value: u8) -> Result<(), SchemaError> {
        if let Some((existing_mask, existing_value)) = self.bytes.get_mut(&offset) {
            let overlap = *existing_mask & mask;
            if ((*existing_value ^ value) & overlap) != 0 {
                return Err(SchemaError::PatchConflict {
                    offset,
                    mask: overlap,
                });
            }
            *existing_value = (*existing_value & !mask) | (value & mask);
            *existing_mask |= mask;
        } else {
            let _previous = self.bytes.insert(offset, (mask, value & mask));
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
) -> Result<Vec<(usize, u8, u8)>, SchemaError> {
    match (field.codec, value) {
        (FieldCodec::Byte { min, max }, FieldValue::Unsigned(value)) => {
            let byte = u8::try_from(value).map_err(|_| SchemaError::UnsignedOutOfRange {
                field: field.name,
                value,
                min: u64::from(min),
                max: u64::from(max),
            })?;
            validate_unsigned(field.name, value, u64::from(min), u64::from(max))?;
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
            validate_unsigned(field.name, value, u64::from(min), u64::from(max))?;
            let byte = u8::try_from(value).map_err(|_| SchemaError::UnsignedOutOfRange {
                field: field.name,
                value,
                min: u64::from(min),
                max: u64::from(max),
            })?;
            let shifted = byte.checked_shl(u32::from(shift)).unwrap_or(0) & mask;
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
            if encoding == StringEncoding::MemoryMap && !bytes.is_ascii() {
                return Err(SchemaError::UnsupportedMemoryMapText { field: field.name });
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
            validate_unsigned(field.name, value, min, max)?;
            let width = validate_width(field.name, width)?;
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
            validate_signed(field.name, value, min, max)?;
            let width = validate_width(field.name, width)?;
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

fn encode_integer_bytes(
    field: &FieldDescriptor,
    bytes: [u8; 8],
    width: usize,
    endian: Endian,
) -> Result<Vec<(usize, u8, u8)>, SchemaError> {
    let selected = match endian {
        Endian::Little => bytes.get(..width),
        Endian::Big => bytes.get(8usize.saturating_sub(width)..),
    }
    .ok_or_else(|| SchemaError::InvalidIntegerWidth {
        field: field.name,
        width: u8::try_from(width).unwrap_or(u8::MAX),
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

fn validate_width(field: &'static str, width: u8) -> Result<usize, SchemaError> {
    if !(1..=8).contains(&width) {
        return Err(SchemaError::InvalidIntegerWidth { field, width });
    }
    Ok(usize::from(width))
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

fn decode_signed(bytes: &[u8], endian: Endian) -> i64 {
    let unsigned = decode_unsigned(bytes, endian);
    let bit_count = bytes.len().saturating_mul(8);
    if bit_count == 0 || bit_count >= 64 {
        return i64::from_ne_bytes(unsigned.to_ne_bytes());
    }
    let sign_bit = 1_u64 << (bit_count - 1);
    if unsigned & sign_bit == 0 {
        i64::try_from(unsigned).unwrap_or(i64::MAX)
    } else {
        let extension = u64::MAX << bit_count;
        i64::from_ne_bytes((unsigned | extension).to_ne_bytes())
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
    fn bit_patch_preserves_fresh_unrelated_bits() -> TestResult {
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&ENABLE, FieldValue::Unsigned(1))?;
        let patches = planner.finish()?;
        let page_patch = patches.page(0x10).ok_or("page 0x10 missing")?;
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
            .page(0x10)
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
        let bytes = patches.page(0x10).ok_or("page missing")?.bytes();
        assert_eq!(bytes.len(), 1);
        assert_eq!(bytes.first().map(|patch| patch.mask()), Some(0x0C));

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
        assert!(matches!(
            conflict.set(&contradictory, FieldValue::Unsigned(0)),
            Err(SchemaError::PatchConflict { .. })
        ));
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
        assert_eq!(patches.pages().collect::<Vec<_>>(), vec![0x10, 0x11]);
        let mut image = vec![0xFF; 0x1200];
        patches.apply_to_image(&mut image)?;
        assert_eq!(image.get(0x10FE..0x1103), Some(&b"OK\0\0\0"[..]));
        assert_eq!(field.read(&image)?, DecodedFieldValue::Text("OK".into()));
        Ok(())
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
}
