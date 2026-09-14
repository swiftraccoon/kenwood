//! Scalar storage codecs with explicit read and text interpretation policies.

use crate::MaskedByte;

/// Byte order for a multi-byte integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// Least-significant byte first.
    Little,
    /// Most-significant byte first.
    Big,
}

/// Encoding of the semantic bytes in a fixed-width string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringEncoding {
    /// UTF-8 text, with padding handled separately.
    Utf8,
    /// Printable ASCII only (`0x20..=0x7E`).
    ///
    /// Model-dependent extended encodings require a separately established
    /// contract; this codec never guesses a code page or replaces invalid text.
    MemoryMap,
}

/// Storage representation and writable numeric range of one scalar field.
///
/// Metadata is validated before every encode or decode. Offsets in encoded
/// [`MaskedByte`] values are relative to the start of this field. Models own
/// absolute addresses, memory geometry, finite enum domains, and write policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldCodec {
    /// One unsigned byte.
    Byte {
        /// Smallest writable value.
        min: u8,
        /// Largest writable value.
        max: u8,
    },
    /// One boolean byte, encoded canonically as zero or one.
    Bool,
    /// One boolean bit in a byte shared with other fields.
    BitBool {
        /// Exactly one owned bit.
        mask: u8,
    },
    /// Contiguous owned bits containing an unsigned value.
    BitField {
        /// Nonzero contiguous mask of owned bits.
        mask: u8,
        /// Position of the least-significant owned bit.
        shift: u8,
        /// Smallest writable value after shifting.
        min: u8,
        /// Largest writable value after shifting.
        max: u8,
    },
    /// A fixed-width string interpreted using an explicit [`TextPolicy`].
    FixedString {
        /// Positive byte width, including trailing padding.
        len: usize,
        /// Encoding of the semantic text before padding.
        encoding: StringEncoding,
        /// Byte used to fill unused storage.
        padding: u8,
    },
    /// An unsigned integer occupying one to eight bytes.
    Unsigned {
        /// Encoded width in bytes.
        width: u8,
        /// Byte order.
        endian: Endian,
        /// Smallest writable value.
        min: u64,
        /// Largest writable value.
        max: u64,
    },
    /// A two's-complement signed integer occupying one to eight bytes.
    Signed {
        /// Encoded width in bytes.
        width: u8,
        /// Byte order.
        endian: Endian,
        /// Smallest writable value.
        min: i64,
        /// Largest writable value.
        max: i64,
    },
    /// An exact-length uninterpreted byte sequence.
    Bytes {
        /// Positive byte count.
        len: usize,
    },
}

/// Numeric domain to enforce after validating codec metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueDomain {
    /// Accept every value representable by the codec's storage width or mask.
    Stored,
    /// Also require the codec's declared numeric range.
    Writable,
}

/// Interpretation of a stored full-byte boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanDecoding {
    /// Reject any stored byte other than zero or one.
    Canonical,
    /// Decode zero as false and every nonzero byte as true.
    ///
    /// This interpretation is intentionally lossy. Encoding the decoded true
    /// value produces one, not the original nonzero byte.
    NonZero,
}

/// Semantic boundary of a fixed-width padded string.
///
/// # Examples
///
/// A NUL followed by semantic data is malformed under exact padding. The
/// first-terminator policy deliberately ignores that suffix, so decoding and
/// re-encoding need not preserve the original stored bytes.
///
/// ```
/// use kenwood_schema::{
///     BooleanDecoding, CodecError, DecodeOptions, DecodedFieldValue, FieldCodec,
///     StringEncoding, TextPolicy, ValueDomain,
/// };
///
/// let codec = FieldCodec::FixedString {
///     len: 4, encoding: StringEncoding::MemoryMap, padding: 0,
/// };
/// let stored = b"A\0B\0";
/// let exact = DecodeOptions {
///     domain: ValueDomain::Stored,
///     boolean: BooleanDecoding::Canonical,
///     text: TextPolicy::ExactPadding,
/// };
/// assert_eq!(codec.decode(stored, exact), Err(CodecError::FixedStringDataAfterNul {
///     terminator_offset: 1, offset: 2, value: b'B',
/// }));
/// let decoded = codec.decode(stored, DecodeOptions {
///     text: TextPolicy::FirstTerminator, ..exact
/// })?;
/// assert_eq!(decoded, DecodedFieldValue::Text("A".to_owned()));
/// let encoded = codec.encode(
///     decoded.as_field_value(), ValueDomain::Stored, TextPolicy::FirstTerminator,
/// )?;
/// let rewritten: Vec<_> = encoded.into_iter().map(|byte| byte.apply(0)).collect();
/// assert_eq!(rewritten, b"A\0\0\0");
/// assert_ne!(rewritten, stored);
/// # Ok::<(), CodecError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextPolicy {
    /// Require exact round trips for accepted semantic text.
    ///
    /// With NUL padding, decoding rejects non-NUL data after the first NUL;
    /// encoding rejects embedded NULs. With other padding, decoding strips
    /// only trailing padding and encoding rejects a trailing padding byte.
    /// Interior non-NUL padding bytes remain semantic text.
    ExactPadding,
    /// Stop decoding at the first NUL or declared padding byte.
    ///
    /// Remaining stored bytes are ignored. Encoding rejects every embedded
    /// NUL or padding byte, so accepted input text is not shortened on decode.
    FirstTerminator,
}

/// Independent policies for decoding one exact field-sized byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Whether numeric values must also satisfy the writable range.
    pub domain: ValueDomain,
    /// Interpretation of full-byte booleans; bit booleans are always masked.
    pub boolean: BooleanDecoding,
    /// Semantic boundary of padded strings.
    pub text: TextPolicy,
}

/// Borrowed caller-supplied scalar value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldValue<'a> {
    /// Unsigned byte, masked field, or multi-byte integer.
    Unsigned(u64),
    /// Signed multi-byte integer.
    Signed(i64),
    /// Boolean byte or bit.
    Bool(bool),
    /// Semantic text without storage padding.
    Text(&'a str),
    /// Exact raw bytes.
    Bytes(&'a [u8]),
}

impl FieldValue<'_> {
    /// Stable human-readable name for this value kind.
    #[must_use]
    pub const fn kind_name(self) -> &'static str {
        match self {
            Self::Unsigned(_) => "unsigned",
            Self::Signed(_) => "signed",
            Self::Bool(_) => "boolean",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
        }
    }
}

/// Owned value decoded from a field-sized byte slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedFieldValue {
    /// Unsigned byte, masked field, or multi-byte integer.
    Unsigned(u64),
    /// Signed multi-byte integer.
    Signed(i64),
    /// Boolean byte or bit.
    Bool(bool),
    /// Decoded semantic text.
    Text(String),
    /// Uninterpreted raw bytes.
    Bytes(Vec<u8>),
}

impl DecodedFieldValue {
    /// Borrow this value for validation or encoding without copying its data.
    #[must_use]
    pub const fn as_field_value(&self) -> FieldValue<'_> {
        match self {
            Self::Unsigned(value) => FieldValue::Unsigned(*value),
            Self::Signed(value) => FieldValue::Signed(*value),
            Self::Bool(value) => FieldValue::Bool(*value),
            Self::Text(value) => FieldValue::Text(value.as_str()),
            Self::Bytes(value) => FieldValue::Bytes(value.as_slice()),
        }
    }

    /// Stable human-readable name for this value kind.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        self.as_field_value().kind_name()
    }
}

/// Field-relative failure in scalar metadata, representation, or value validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    /// A value's variant does not match its codec.
    #[error("expected {expected}, received {actual}")]
    TypeMismatch {
        /// Codec's expected kind.
        expected: &'static str,
        /// Supplied value kind.
        actual: &'static str,
    },
    /// An unsigned value is outside the selected domain.
    #[error("unsigned value {value} is outside {min}..={max}")]
    UnsignedOutOfRange {
        /// Supplied or decoded value.
        value: u64,
        /// Smallest accepted value.
        min: u64,
        /// Largest accepted value.
        max: u64,
    },
    /// A signed value is outside the selected domain.
    #[error("signed value {value} is outside {min}..={max}")]
    SignedOutOfRange {
        /// Supplied or decoded value.
        value: i64,
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
    },
    /// Unsigned codec metadata declares an inverted range.
    #[error("unsigned range {min}..={max} is inverted")]
    InvalidUnsignedRange {
        /// Declared minimum.
        min: u64,
        /// Declared maximum.
        max: u64,
    },
    /// Signed codec metadata declares an inverted range.
    #[error("signed range {min}..={max} is inverted")]
    InvalidSignedRange {
        /// Declared minimum.
        min: i64,
        /// Declared maximum.
        max: i64,
    },
    /// Integer storage width is outside one to eight bytes.
    #[error("integer width {width} is outside 1..=8 bytes")]
    InvalidIntegerWidth {
        /// Invalid width.
        width: u8,
    },
    /// The declared integer domain cannot fit in its storage width.
    #[error("integer domain exceeds its {width}-byte storage width")]
    DomainExceedsWidth {
        /// Declared width.
        width: u8,
    },
    /// Mask, shift, or numeric range does not describe a usable bit field.
    #[error("invalid bit field with mask 0x{mask:02X} and shift {shift}")]
    InvalidBitField {
        /// Declared mask.
        mask: u8,
        /// Declared shift, or zero for a bit boolean.
        shift: u8,
    },
    /// Raw-byte or text storage has zero length.
    #[error("encoded length {len} must be positive")]
    InvalidEncodedLength {
        /// Invalid length.
        len: usize,
    },
    /// The exact supplied field slice or raw byte value has a different length.
    #[error("byte length is {actual}, expected exactly {expected}")]
    ByteLength {
        /// Supplied byte count.
        actual: usize,
        /// Required byte count.
        expected: usize,
    },
    /// A stored boolean is not zero or one under canonical decoding.
    #[error("boolean byte {value} is not canonical zero or one")]
    NonCanonicalBoolean {
        /// Invalid stored byte.
        value: u8,
    },
    /// Semantic text exceeds the fixed storage width.
    #[error("text is {actual} bytes, exceeding the maximum {max}")]
    TextTooLong {
        /// UTF-8 byte count, not character count.
        actual: usize,
        /// Maximum encoded bytes.
        max: usize,
    },
    /// Semantic text contains a NUL that its exact-padding decoder would strip.
    #[error("text contains a NUL terminator at byte {offset}")]
    TextContainsNul {
        /// Byte offset within semantic text.
        offset: usize,
    },
    /// Semantic text ends in the declared non-NUL padding byte.
    #[error("text ends with padding 0x{padding:02X} at byte {offset}")]
    TextEndsWithPadding {
        /// Byte offset within semantic text.
        offset: usize,
        /// Declared padding byte.
        padding: u8,
    },
    /// Semantic text contains a byte that first-terminator decoding would strip.
    #[error("text byte 0x{value:02X} at {offset} is NUL or padding 0x{padding:02X}")]
    TextTerminator {
        /// Byte offset within semantic text.
        offset: usize,
        /// Encountered terminator byte.
        value: u8,
        /// Declared padding byte.
        padding: u8,
    },
    /// Exact NUL padding contains a later non-NUL byte.
    #[error("byte {offset} is 0x{value:02X} after the NUL terminator at {terminator_offset}")]
    FixedStringDataAfterNul {
        /// Offset of the first NUL.
        terminator_offset: usize,
        /// Offset of the first later non-NUL byte.
        offset: usize,
        /// Unexpected stored byte.
        value: u8,
    },
    /// Semantic memory-map text contains a non-printable-ASCII byte.
    #[error("text byte 0x{value:02X} at {offset} is outside printable ASCII 0x20..=0x7E")]
    InvalidMemoryMapTextByte {
        /// Byte offset within semantic text.
        offset: usize,
        /// Invalid byte.
        value: u8,
    },
    /// Semantic text is not complete, valid UTF-8.
    #[error("invalid UTF-8 at byte {valid_up_to} (invalid sequence length {error_len:?})")]
    InvalidUtf8 {
        /// Number of valid bytes preceding the invalid sequence.
        valid_up_to: usize,
        /// Invalid sequence length, absent for an incomplete final sequence.
        error_len: Option<usize>,
    },
}

impl FieldCodec {
    /// Number of bytes occupied, before metadata validation.
    ///
    /// Bit-level fields occupy one shared byte. Call [`Self::validate`] before
    /// treating externally supplied codec metadata as a usable storage span.
    #[must_use]
    pub const fn encoded_len(self) -> usize {
        match self {
            Self::Byte { .. } | Self::Bool | Self::BitBool { .. } | Self::BitField { .. } => 1,
            Self::FixedString { len, .. } | Self::Bytes { len } => len,
            Self::Unsigned { width, .. } | Self::Signed { width, .. } => width as usize,
        }
    }

    /// Stable human-readable kind of the value accepted by this codec.
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

    /// Validate representation metadata without reading or allocating a field.
    ///
    /// No model-specific memory size is imposed. Declared numeric ranges must
    /// be ordered and representable even when interpreting stored values.
    /// Any positive [`Self::FixedString`] or [`Self::Bytes`] length is valid
    /// metadata; callers must separately bound [`Self::encoded_len`] for their
    /// address space and allocation budget before encoding or decoding.
    ///
    /// # Errors
    ///
    /// Returns an error for zero lengths, invalid integer widths, inverted or
    /// unrepresentable domains, or unusable bit masks and shifts.
    pub fn validate(self) -> Result<(), CodecError> {
        match self {
            Self::Byte { min, max } => validate_unsigned_range(u64::from(min), u64::from(max)),
            Self::Bool => Ok(()),
            Self::BitBool { mask } => {
                if mask.is_power_of_two() {
                    Ok(())
                } else {
                    Err(CodecError::InvalidBitField { mask, shift: 0 })
                }
            }
            Self::BitField {
                mask,
                shift,
                min,
                max,
            } => validate_bit_field(mask, shift, min, max),
            Self::FixedString { len, .. } | Self::Bytes { len } => {
                if len == 0 {
                    Err(CodecError::InvalidEncodedLength { len })
                } else {
                    Ok(())
                }
            }
            Self::Unsigned {
                width, min, max, ..
            } => {
                let storage = IntegerWidth::new(width)?;
                validate_unsigned_range(min, max)?;
                if max > storage.unsigned_max() {
                    Err(CodecError::DomainExceedsWidth { width })
                } else {
                    Ok(())
                }
            }
            Self::Signed {
                width, min, max, ..
            } => {
                let storage = IntegerWidth::new(width)?;
                if min > max {
                    return Err(CodecError::InvalidSignedRange { min, max });
                }
                let (storage_min, storage_max) = storage.signed_bounds();
                if min < storage_min || max > storage_max {
                    Err(CodecError::DomainExceedsWidth { width })
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Decode exactly one field-sized byte slice using explicit policies.
    ///
    /// This method neither searches an image nor ignores trailing bytes beyond
    /// the field span. Stored numeric values may exceed the writable range;
    /// metadata validity, exact length, and text validity are always enforced.
    /// Text and raw-byte results allocate owned copies. Metadata validation
    /// does not impose a resource cap; admit runtime-supplied lengths against
    /// an application budget before calling this method.
    ///
    /// # Errors
    ///
    /// Returns metadata, exact-length, selected-domain, boolean, or text errors.
    pub fn decode(
        self,
        bytes: &[u8],
        options: DecodeOptions,
    ) -> Result<DecodedFieldValue, CodecError> {
        self.validate()?;
        validate_length(bytes.len(), self.encoded_len())?;
        let first = bytes.first().copied().unwrap_or_default();
        Ok(match self {
            Self::Byte { min, max } => {
                let value = u64::from(first);
                if options.domain == ValueDomain::Writable {
                    validate_unsigned(value, u64::from(min), u64::from(max))?;
                }
                DecodedFieldValue::Unsigned(value)
            }
            Self::Bool => {
                if options.boolean == BooleanDecoding::Canonical && first > 1 {
                    return Err(CodecError::NonCanonicalBoolean { value: first });
                }
                DecodedFieldValue::Bool(first != 0)
            }
            Self::BitBool { mask } => DecodedFieldValue::Bool(first & mask != 0),
            Self::BitField {
                mask,
                shift,
                min,
                max,
            } => {
                let value = u64::from((first & mask) >> shift);
                if options.domain == ValueDomain::Writable {
                    validate_unsigned(value, u64::from(min), u64::from(max))?;
                }
                DecodedFieldValue::Unsigned(value)
            }
            Self::FixedString {
                encoding, padding, ..
            } => {
                let semantic = semantic_text(bytes, padding, options.text)?;
                validate_text_encoding(semantic, encoding)?;
                let text =
                    std::str::from_utf8(semantic).map_err(|error| CodecError::InvalidUtf8 {
                        valid_up_to: error.valid_up_to(),
                        error_len: error.error_len(),
                    })?;
                DecodedFieldValue::Text(text.to_owned())
            }
            Self::Unsigned {
                endian, min, max, ..
            } => {
                let value = decode_unsigned(bytes, endian);
                if options.domain == ValueDomain::Writable {
                    validate_unsigned(value, min, max)?;
                }
                DecodedFieldValue::Unsigned(value)
            }
            Self::Signed {
                width,
                endian,
                min,
                max,
            } => {
                let value = decode_signed(bytes, IntegerWidth::new(width)?, endian);
                if options.domain == ValueDomain::Writable {
                    validate_signed(value, min, max)?;
                }
                DecodedFieldValue::Signed(value)
            }
            Self::Bytes { .. } => DecodedFieldValue::Bytes(bytes.to_vec()),
        })
    }

    /// Encode a value into ordered field-relative masked bytes.
    ///
    /// Numeric values must fit the storage representation in either domain.
    /// Writable encoding additionally enforces the declared numeric range.
    /// Booleans always encode canonically, and unowned bits are always zero.
    /// Every validation completes before any result is returned.
    ///
    /// # Allocation
    ///
    /// The result contains one [`MaskedByte`] per storage byte, including
    /// string padding. [`Self::validate`] accepts any positive text or raw-byte
    /// length, without an image-size or allocation limit. Callers must cap
    /// runtime-supplied [`Self::encoded_len`] before encoding. These are ordinary
    /// vector allocations; allocation failure is not a [`CodecError`].
    ///
    /// # Errors
    ///
    /// Returns metadata, type, domain, exact-byte-length, or text errors.
    pub fn encode(
        self,
        value: FieldValue<'_>,
        domain: ValueDomain,
        text: TextPolicy,
    ) -> Result<Vec<MaskedByte>, CodecError> {
        self.validate()?;
        match (self, value) {
            (Self::Byte { min, max }, FieldValue::Unsigned(raw)) => {
                let byte = bounded_byte(raw, min, max, u8::MAX, domain)?;
                Ok(vec![masked_byte(u8::MAX, byte)])
            }
            (Self::Bool, FieldValue::Bool(flag)) => Ok(vec![masked_byte(u8::MAX, u8::from(flag))]),
            (Self::BitBool { mask }, FieldValue::Bool(flag)) => {
                Ok(vec![masked_byte(mask, if flag { mask } else { 0 })])
            }
            (
                Self::BitField {
                    mask,
                    shift,
                    min,
                    max,
                },
                FieldValue::Unsigned(raw),
            ) => {
                let byte = bounded_byte(raw, min, max, mask >> shift, domain)?;
                Ok(vec![masked_byte(mask, (byte << shift) & mask)])
            }
            (
                Self::FixedString {
                    len,
                    encoding,
                    padding,
                },
                FieldValue::Text(value),
            ) => encode_text(value, len, encoding, padding, text),
            (
                Self::Unsigned {
                    width,
                    endian,
                    min,
                    max,
                },
                FieldValue::Unsigned(raw),
            ) => {
                let width = IntegerWidth::new(width)?;
                let (min, max) = match domain {
                    ValueDomain::Stored => (0, width.unsigned_max()),
                    ValueDomain::Writable => (min, max),
                };
                validate_unsigned(raw, min, max)?;
                encode_integer(raw, width, endian)
            }
            (
                Self::Signed {
                    width,
                    endian,
                    min,
                    max,
                },
                FieldValue::Signed(raw),
            ) => {
                let width = IntegerWidth::new(width)?;
                let (min, max) = match domain {
                    ValueDomain::Stored => width.signed_bounds(),
                    ValueDomain::Writable => (min, max),
                };
                validate_signed(raw, min, max)?;
                encode_integer(u64::from_ne_bytes(raw.to_ne_bytes()), width, endian)
            }
            (Self::Bytes { len }, FieldValue::Bytes(bytes)) => {
                validate_length(bytes.len(), len)?;
                Ok(full_bytes(bytes.iter().copied()))
            }
            (codec, other) => Err(CodecError::TypeMismatch {
                expected: codec.value_kind(),
                actual: other.kind_name(),
            }),
        }
    }
}

const fn masked_byte(mask: u8, value: u8) -> MaskedByte {
    MaskedByte {
        offset: 0,
        mask,
        value,
    }
}

fn full_bytes(bytes: impl Iterator<Item = u8>) -> Vec<MaskedByte> {
    bytes
        .enumerate()
        .map(|(offset, value)| MaskedByte {
            offset,
            mask: u8::MAX,
            value,
        })
        .collect()
}

const fn validate_length(actual: usize, expected: usize) -> Result<(), CodecError> {
    if actual == expected {
        Ok(())
    } else {
        Err(CodecError::ByteLength { actual, expected })
    }
}

const fn validate_unsigned_range(min: u64, max: u64) -> Result<(), CodecError> {
    if min <= max {
        Ok(())
    } else {
        Err(CodecError::InvalidUnsignedRange { min, max })
    }
}

const fn validate_unsigned(value: u64, min: u64, max: u64) -> Result<(), CodecError> {
    if value < min || value > max {
        Err(CodecError::UnsignedOutOfRange { value, min, max })
    } else {
        Ok(())
    }
}

const fn validate_signed(value: i64, min: i64, max: i64) -> Result<(), CodecError> {
    if value < min || value > max {
        Err(CodecError::SignedOutOfRange { value, min, max })
    } else {
        Ok(())
    }
}

fn bounded_byte(
    value: u64,
    min: u8,
    max: u8,
    storage_max: u8,
    domain: ValueDomain,
) -> Result<u8, CodecError> {
    let (min, max) = match domain {
        ValueDomain::Stored => (0, u64::from(storage_max)),
        ValueDomain::Writable => (u64::from(min), u64::from(max)),
    };
    validate_unsigned(value, min, max)?;
    u8::try_from(value).map_err(|_| CodecError::UnsignedOutOfRange { value, min, max })
}

const fn validate_bit_field(mask: u8, shift: u8, min: u8, max: u8) -> Result<(), CodecError> {
    let shifted = if shift < 8 { mask >> shift } else { 0 };
    let lower = if shift == 0 || shift >= 8 {
        0
    } else {
        u8::MAX >> (8 - shift)
    };
    if mask == 0
        || shift >= 8
        || mask & lower != 0
        || shifted & shifted.wrapping_add(1) != 0
        || min > max
        || max > shifted
    {
        Err(CodecError::InvalidBitField { mask, shift })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct IntegerWidth(u8);

impl IntegerWidth {
    const fn new(width: u8) -> Result<Self, CodecError> {
        if width == 0 || width > 8 {
            Err(CodecError::InvalidIntegerWidth { width })
        } else {
            Ok(Self(width))
        }
    }

    fn unsigned_max(self) -> u64 {
        if self.0 == 8 {
            u64::MAX
        } else {
            (1_u64 << (u32::from(self.0) * 8)) - 1
        }
    }

    fn signed_bounds(self) -> (i64, i64) {
        if self.0 == 8 {
            (i64::MIN, i64::MAX)
        } else {
            let half = 1_i64 << (u32::from(self.0) * 8 - 1);
            (-half, half - 1)
        }
    }
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
    let raw = decode_unsigned(bytes, endian);
    let bits = u32::from(width.0) * 8;
    let extended = if bits < 64 && raw & (1_u64 << (bits - 1)) != 0 {
        raw | (u64::MAX << bits)
    } else {
        raw
    };
    i64::from_ne_bytes(extended.to_ne_bytes())
}

fn encode_integer(
    raw: u64,
    width: IntegerWidth,
    endian: Endian,
) -> Result<Vec<MaskedByte>, CodecError> {
    let all = raw.to_le_bytes();
    let bytes = all
        .get(..usize::from(width.0))
        .ok_or(CodecError::InvalidIntegerWidth { width: width.0 })?;
    Ok(match endian {
        Endian::Little => full_bytes(bytes.iter().copied()),
        Endian::Big => full_bytes(bytes.iter().rev().copied()),
    })
}

fn semantic_text(bytes: &[u8], padding: u8, policy: TextPolicy) -> Result<&[u8], CodecError> {
    let end = match policy {
        TextPolicy::FirstTerminator => bytes
            .iter()
            .position(|&byte| byte == 0 || byte == padding)
            .unwrap_or(bytes.len()),
        TextPolicy::ExactPadding if padding == 0 => {
            let Some(terminator_offset) = bytes.iter().position(|&byte| byte == 0) else {
                return Ok(bytes);
            };
            if let Some((offset, &value)) = bytes
                .iter()
                .enumerate()
                .skip(terminator_offset + 1)
                .find(|(_, value)| **value != 0)
            {
                return Err(CodecError::FixedStringDataAfterNul {
                    terminator_offset,
                    offset,
                    value,
                });
            }
            terminator_offset
        }
        TextPolicy::ExactPadding => bytes
            .iter()
            .rposition(|&byte| byte != padding)
            .map_or(0, |offset| offset + 1),
    };
    bytes.get(..end).ok_or(CodecError::ByteLength {
        actual: bytes.len(),
        expected: end,
    })
}

fn validate_text_encoding(bytes: &[u8], encoding: StringEncoding) -> Result<(), CodecError> {
    if encoding == StringEncoding::MemoryMap
        && let Some((offset, &value)) = bytes
            .iter()
            .enumerate()
            .find(|(_, value)| !(b' '..=b'~').contains(value))
    {
        return Err(CodecError::InvalidMemoryMapTextByte { offset, value });
    }
    Ok(())
}

fn encode_text(
    value: &str,
    len: usize,
    encoding: StringEncoding,
    padding: u8,
    policy: TextPolicy,
) -> Result<Vec<MaskedByte>, CodecError> {
    let bytes = value.as_bytes();
    if bytes.len() > len {
        return Err(CodecError::TextTooLong {
            actual: bytes.len(),
            max: len,
        });
    }
    match policy {
        TextPolicy::FirstTerminator => {
            if let Some((offset, &value)) = bytes
                .iter()
                .enumerate()
                .find(|(_, value)| **value == 0 || **value == padding)
            {
                return Err(CodecError::TextTerminator {
                    offset,
                    value,
                    padding,
                });
            }
        }
        TextPolicy::ExactPadding if padding == 0 => {
            if let Some(offset) = bytes.iter().position(|&byte| byte == 0) {
                return Err(CodecError::TextContainsNul { offset });
            }
        }
        TextPolicy::ExactPadding => {
            if let Some((offset, &last)) = bytes.iter().enumerate().next_back()
                && last == padding
            {
                return Err(CodecError::TextEndsWithPadding { offset, padding });
            }
        }
    }
    validate_text_encoding(bytes, encoding)?;
    Ok(full_bytes((0..len).map(|offset| {
        bytes.get(offset).copied().unwrap_or(padding)
    })))
}

#[cfg(test)]
mod tests;
