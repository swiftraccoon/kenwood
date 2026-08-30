//! Field descriptors with slot terms, on-image codecs, and patch planning.

use std::collections::BTreeMap;

use crate::error::SchemaError;
use crate::protocol::mcp::regions::writable_page_for;
use crate::protocol::mcp::{BytePatch, PagePatch};
use crate::types::{Address, IMAGE_LENGTH, SLOT_STRIDE, SlotIndex};

/// Byte order of a multi-byte integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// Least-significant byte first.
    Little,
    /// Most-significant byte first.
    Big,
}

/// Encoding of a fixed-width string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringEncoding {
    /// UTF-8 bytes.
    Utf8,
    /// The program's memory-map text: printable ASCII only.
    MemoryMap,
}

impl StringEncoding {
    const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf8",
            Self::MemoryMap => "memory_map",
        }
    }
}

/// How a field is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldCodec {
    /// One byte within `min..=max`.
    Byte {
        /// Minimum.
        min: u8,
        /// Maximum.
        max: u8,
    },
    /// One byte, zero or non-zero.
    Bool,
    /// One masked bit as a boolean.
    BitBool {
        /// The bit.
        mask: u8,
    },
    /// Masked bits shifted down, within `min..=max`.
    BitField {
        /// Bits owned.
        mask: u8,
        /// Shift of the lowest owned bit.
        shift: u8,
        /// Minimum.
        min: u8,
        /// Maximum.
        max: u8,
    },
    /// A fixed-width string padded with `padding`.
    FixedString {
        /// Width in bytes.
        len: usize,
        /// Encoding.
        encoding: StringEncoding,
        /// Padding byte after the text.
        padding: u8,
    },
    /// An unsigned integer of `width` bytes.
    Unsigned {
        /// Width in bytes (1..=8).
        width: u8,
        /// Byte order.
        endian: Endian,
        /// Minimum.
        min: u64,
        /// Maximum.
        max: u64,
    },
    /// A signed integer of `width` bytes.
    Signed {
        /// Width in bytes (1..=8).
        width: u8,
        /// Byte order.
        endian: Endian,
        /// Minimum.
        min: i64,
        /// Maximum.
        max: i64,
    },
    /// Raw bytes.
    Bytes {
        /// Length.
        len: usize,
    },
}

impl FieldCodec {
    /// Bytes the field occupies.
    #[must_use]
    pub const fn encoded_len(self) -> usize {
        match self {
            Self::Byte { .. } | Self::Bool | Self::BitBool { .. } | Self::BitField { .. } => 1,
            Self::FixedString { len, .. } | Self::Bytes { len } => len,
            Self::Unsigned { width, .. } | Self::Signed { width, .. } => width as usize,
        }
    }

    /// The value kind the codec accepts.
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

/// One stride-scaled dimension index of a field address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    /// Dimension name (`pm_slot`).
    pub dimension: &'static str,
    /// Bytes per index step.
    pub stride: u32,
}

/// The Programmable-Memory slot term of the menu blocks.
pub const SLOT_TERM: Term = Term {
    dimension: "pm_slot",
    stride: SLOT_STRIDE,
};

/// Where and how a field is stored: `base + sum(stride * slot)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDescriptor {
    /// Qualified name (`menu.Field`).
    pub name: &'static str,
    /// Address when every index is zero.
    pub base: u32,
    /// Dimension terms (empty for a global field).
    pub terms: &'static [Term],
    /// Storage codec.
    pub codec: FieldCodec,
}

impl FieldDescriptor {
    /// A global field.
    #[must_use]
    pub const fn new(name: &'static str, base: u32, codec: FieldCodec) -> Self {
        Self {
            name,
            base,
            terms: &[],
            codec,
        }
    }

    /// A field with dimension terms.
    #[must_use]
    pub const fn with_terms(
        name: &'static str,
        base: u32,
        terms: &'static [Term],
        codec: FieldCodec,
    ) -> Self {
        Self {
            name,
            base,
            terms,
            codec,
        }
    }

    /// Whether the field needs a slot.
    #[must_use]
    pub const fn is_per_slot(&self) -> bool {
        !self.terms.is_empty()
    }

    /// Resolve the absolute address for `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::SlotRequired`] for a per-slot field without a
    /// slot, [`SchemaError::UnknownDimension`] for a term other than
    /// `pm_slot`, and [`SchemaError::OutOfBounds`] past the image.
    pub fn address(&self, slot: Option<SlotIndex>) -> Result<Address, SchemaError> {
        let mut address = u64::from(self.base);
        for term in self.terms {
            if term.dimension != SLOT_TERM.dimension {
                return Err(SchemaError::UnknownDimension {
                    field: self.name,
                    dimension: term.dimension,
                });
            }
            let slot = slot.ok_or(SchemaError::SlotRequired {
                field: self.name,
                dimension: term.dimension,
            })?;
            address += u64::from(term.stride) * u64::from(slot.index());
        }
        let len = self.codec.encoded_len();
        let out_of_bounds = SchemaError::OutOfBounds {
            field: self.name,
            address,
            len,
            image_length: IMAGE_LENGTH,
        };
        let end = address + u64::try_from(len).unwrap_or(u64::MAX);
        if end > u64::try_from(IMAGE_LENGTH).unwrap_or(u64::MAX) {
            return Err(out_of_bounds);
        }
        u32::try_from(address)
            .ok()
            .and_then(|value| Address::new(value).ok())
            .ok_or(out_of_bounds)
    }

    /// Decode the field from `image` for `slot`.
    ///
    /// # Errors
    ///
    /// Address errors as in [`FieldDescriptor::address`]; text errors for
    /// bytes the encoding cannot represent.
    pub fn read(
        &self,
        image: &[u8],
        slot: Option<SlotIndex>,
    ) -> Result<DecodedFieldValue, SchemaError> {
        let start = self.address(slot)?.as_usize();
        let len = self.codec.encoded_len();
        let bytes = image
            .get(start..start + len)
            .ok_or_else(|| SchemaError::OutOfBounds {
                field: self.name,
                address: u64::try_from(start).unwrap_or(u64::MAX),
                len,
                image_length: image.len(),
            })?;
        Ok(match self.codec {
            FieldCodec::Byte { .. } => DecodedFieldValue::Unsigned(u64::from(first(bytes))),
            FieldCodec::Bool => DecodedFieldValue::Bool(first(bytes) != 0),
            FieldCodec::BitBool { mask } => DecodedFieldValue::Bool(first(bytes) & mask != 0),
            FieldCodec::BitField { mask, shift, .. } => {
                DecodedFieldValue::Unsigned(u64::from((first(bytes) & mask) >> shift))
            }
            FieldCodec::FixedString {
                padding, encoding, ..
            } => {
                let text_end = bytes
                    .iter()
                    .position(|&byte| byte == padding || byte == 0)
                    .unwrap_or(bytes.len());
                let text = bytes.get(..text_end).unwrap_or_default();
                match encoding {
                    StringEncoding::Utf8 => DecodedFieldValue::Text(
                        String::from_utf8(text.to_vec()).map_err(|error| {
                            SchemaError::TextByte {
                                field: self.name,
                                encoding: encoding.name(),
                                value: text
                                    .get(error.utf8_error().valid_up_to())
                                    .copied()
                                    .unwrap_or(0),
                            }
                        })?,
                    ),
                    StringEncoding::MemoryMap => {
                        if let Some(&bad) = text
                            .iter()
                            .find(|byte| !byte.is_ascii_graphic() && **byte != b' ')
                        {
                            return Err(SchemaError::TextByte {
                                field: self.name,
                                encoding: encoding.name(),
                                value: bad,
                            });
                        }
                        DecodedFieldValue::Text(String::from_utf8_lossy(text).into_owned())
                    }
                }
            }
            FieldCodec::Unsigned { endian, .. } => {
                DecodedFieldValue::Unsigned(unsigned(bytes, endian))
            }
            FieldCodec::Signed { endian, width, .. } => {
                let raw = unsigned(bytes, endian);
                let bits = u32::from(width) * 8;
                let extended = if bits < 64 && raw & (1 << (bits - 1)) != 0 {
                    raw | (u64::MAX << bits)
                } else {
                    raw
                };
                DecodedFieldValue::Signed(i64::from_ne_bytes(extended.to_ne_bytes()))
            }
            FieldCodec::Bytes { .. } => DecodedFieldValue::Bytes(bytes.to_vec()),
        })
    }

    /// Encode `value` as `(offset, mask, bits)` triples relative to the field start.
    ///
    /// # Errors
    ///
    /// Returns type, range, and text errors.
    pub fn encode(&self, value: FieldValue<'_>) -> Result<Vec<(usize, u8, u8)>, SchemaError> {
        let mismatch = |actual: &'static str| SchemaError::TypeMismatch {
            field: self.name,
            expected: self.codec.value_kind(),
            actual,
        };
        Ok(match (self.codec, value) {
            (FieldCodec::Byte { min, max }, FieldValue::Unsigned(raw)) => {
                let byte = self.bounded_u8(raw, min, max)?;
                vec![(0, 0xFF, byte)]
            }
            (FieldCodec::Bool, FieldValue::Bool(flag)) => vec![(0, 0xFF, u8::from(flag))],
            (FieldCodec::BitBool { mask }, FieldValue::Bool(flag)) => {
                vec![(0, mask, if flag { mask } else { 0 })]
            }
            (
                FieldCodec::BitField {
                    mask,
                    shift,
                    min,
                    max,
                },
                FieldValue::Unsigned(raw),
            ) => {
                let bits = self.bounded_u8(raw, min, max)?;
                vec![(0, mask, (bits << shift) & mask)]
            }
            (
                FieldCodec::FixedString {
                    len,
                    encoding,
                    padding,
                },
                FieldValue::Text(text),
            ) => self.encode_text(len, encoding, padding, text)?,
            (
                FieldCodec::Unsigned {
                    width,
                    endian,
                    min,
                    max,
                },
                FieldValue::Unsigned(raw),
            ) => {
                if !(min..=max).contains(&raw) {
                    return Err(SchemaError::UnsignedOutOfRange {
                        field: self.name,
                        value: raw,
                        min,
                        max,
                    });
                }
                integer_bytes(raw, width, endian)
            }
            (
                FieldCodec::Signed {
                    width,
                    endian,
                    min,
                    max,
                },
                FieldValue::Signed(raw),
            ) => {
                if !(min..=max).contains(&raw) {
                    return Err(SchemaError::SignedOutOfRange {
                        field: self.name,
                        value: raw,
                        min,
                        max,
                    });
                }
                integer_bytes(u64::from_ne_bytes(raw.to_ne_bytes()), width, endian)
            }
            (FieldCodec::Bytes { len }, FieldValue::Bytes(bytes)) => {
                if bytes.len() != len {
                    return Err(SchemaError::BytesLength {
                        field: self.name,
                        len: bytes.len(),
                        expected: len,
                    });
                }
                bytes
                    .iter()
                    .enumerate()
                    .map(|(offset, &byte)| (offset, 0xFF, byte))
                    .collect()
            }
            (_, other) => return Err(mismatch(other.kind_name())),
        })
    }

    fn bounded_u8(&self, raw: u64, min: u8, max: u8) -> Result<u8, SchemaError> {
        let byte = u8::try_from(raw)
            .ok()
            .filter(|byte| (min..=max).contains(byte));
        byte.ok_or_else(|| SchemaError::UnsignedOutOfRange {
            field: self.name,
            value: raw,
            min: u64::from(min),
            max: u64::from(max),
        })
    }

    fn encode_text(
        &self,
        len: usize,
        encoding: StringEncoding,
        padding: u8,
        text: &str,
    ) -> Result<Vec<(usize, u8, u8)>, SchemaError> {
        if text.len() > len {
            return Err(SchemaError::TextTooLong {
                field: self.name,
                len: text.len(),
                max: len,
            });
        }
        if encoding == StringEncoding::MemoryMap
            && let Some(&bad) = text
                .as_bytes()
                .iter()
                .find(|byte| !byte.is_ascii_graphic() && **byte != b' ')
        {
            return Err(SchemaError::TextByte {
                field: self.name,
                encoding: encoding.name(),
                value: bad,
            });
        }
        let mut bytes: Vec<u8> = text.as_bytes().to_vec();
        bytes.resize(len, padding);
        Ok(bytes
            .into_iter()
            .enumerate()
            .map(|(offset, byte)| (offset, 0xFF, byte))
            .collect())
    }
}

fn first(bytes: &[u8]) -> u8 {
    bytes.first().copied().unwrap_or_default()
}

fn unsigned(bytes: &[u8], endian: Endian) -> u64 {
    let mut value = 0u64;
    match endian {
        Endian::Little => {
            for &byte in bytes.iter().rev() {
                value = (value << 8) | u64::from(byte);
            }
        }
        Endian::Big => {
            for &byte in bytes {
                value = (value << 8) | u64::from(byte);
            }
        }
    }
    value
}

fn integer_bytes(raw: u64, width: u8, endian: Endian) -> Vec<(usize, u8, u8)> {
    let mut bytes: Vec<u8> = raw
        .to_le_bytes()
        .get(..usize::from(width))
        .unwrap_or_default()
        .to_vec();
    if endian == Endian::Big {
        bytes.reverse();
    }
    bytes
        .into_iter()
        .enumerate()
        .map(|(offset, byte)| (offset, 0xFF, byte))
        .collect()
}

/// A value to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldValue<'a> {
    /// Unsigned integer.
    Unsigned(u64),
    /// Signed integer.
    Signed(i64),
    /// Boolean.
    Bool(bool),
    /// Text.
    Text(&'a str),
    /// Raw bytes.
    Bytes(&'a [u8]),
}

impl FieldValue<'_> {
    /// The value kind name.
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

/// A value read from an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedFieldValue {
    /// Unsigned integer.
    Unsigned(u64),
    /// Signed integer.
    Signed(i64),
    /// Boolean.
    Bool(bool),
    /// Text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Copy)]
struct ByteClaim {
    owner: &'static str,
    mask: u8,
    value: u8,
}

/// Collects field values into region-aligned masked page patches.
#[derive(Debug, Default)]
pub struct PatchPlanner {
    claims: BTreeMap<u32, ByteClaim>,
}

impl PatchPlanner {
    /// An empty plan.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            claims: BTreeMap::new(),
        }
    }

    /// Plan `value` for `field` in `slot`.
    ///
    /// # Errors
    ///
    /// Returns encode errors, [`SchemaError::NotWritable`] outside the
    /// writable regions, and [`SchemaError::ByteConflict`] when two fields
    /// claim the same bits.
    pub fn set(
        &mut self,
        field: &FieldDescriptor,
        slot: Option<SlotIndex>,
        value: FieldValue<'_>,
    ) -> Result<&mut Self, SchemaError> {
        let start = field.address(slot)?;
        for (offset, mask, bits) in field.encode(value)? {
            let address = start
                .checked_add(u32::try_from(offset).unwrap_or(u32::MAX))
                .map_err(|_| SchemaError::OutOfBounds {
                    field: field.name,
                    address: u64::from(start.as_u32()) + u64::try_from(offset).unwrap_or(u64::MAX),
                    len: 1,
                    image_length: IMAGE_LENGTH,
                })?;
            if writable_page_for(address).is_none() {
                return Err(SchemaError::NotWritable {
                    field: field.name,
                    address: address.as_u32(),
                });
            }
            let key = address.as_u32();
            match self.claims.get_mut(&key) {
                Some(claim) if claim.mask & mask != 0 => {
                    return Err(SchemaError::ByteConflict {
                        first: claim.owner,
                        second: field.name,
                        address: key,
                    });
                }
                Some(claim) => {
                    claim.mask |= mask;
                    claim.value = (claim.value & !mask) | (bits & mask);
                }
                None => {
                    let _fresh = self.claims.insert(
                        key,
                        ByteClaim {
                            owner: field.name,
                            mask,
                            value: bits & mask,
                        },
                    );
                }
            }
        }
        Ok(self)
    }

    /// Group the claims into pages of the writable region walk.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::NotWritable`] if a claim has no page (cannot
    /// happen after [`PatchPlanner::set`] accepted it).
    pub fn finish(self) -> Result<PatchSet, SchemaError> {
        let mut pages: BTreeMap<u32, PagePatch> = BTreeMap::new();
        for (address, claim) in self.claims {
            let page = Address::new(address)
                .ok()
                .and_then(writable_page_for)
                .ok_or(SchemaError::NotWritable {
                    field: claim.owner,
                    address,
                })?;
            let entry = pages
                .entry(page.address().as_u32())
                .or_insert_with(|| PagePatch {
                    page,
                    bytes: Vec::new(),
                });
            entry.bytes.push(BytePatch {
                offset: u8::try_from(address - page.address().as_u32()).unwrap_or(u8::MAX),
                mask: claim.mask,
                value: claim.value,
            });
        }
        Ok(PatchSet {
            pages: pages.into_values().collect(),
        })
    }
}

/// Region-aligned masked page patches, in address order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSet {
    pages: Vec<PagePatch>,
}

impl PatchSet {
    /// The page patches.
    #[must_use]
    pub fn pages(&self) -> &[PagePatch] {
        &self.pages
    }

    /// Whether nothing is planned.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Number of pages touched.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.pages.len()
    }

    /// Apply every patch to a full image buffer.
    pub fn apply_to_image(&self, image: &mut [u8]) {
        for patch in &self.pages {
            let start = patch.page.address().as_usize();
            if let Some(window) = image.get_mut(start..start + patch.page.len()) {
                patch.apply(window);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GLOBAL: FieldDescriptor =
        FieldDescriptor::new("pm.PmSelect", 323_593, FieldCodec::Byte { min: 0, max: 5 });
    const PER_SLOT: FieldDescriptor = FieldDescriptor::with_terms(
        "radio.MeterType",
        328_995,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 2 },
    );
    const NAME: FieldDescriptor = FieldDescriptor::with_terms(
        "gps.MyPositionList[0].Name",
        329_246,
        &[SLOT_TERM],
        FieldCodec::FixedString {
            len: 8,
            encoding: StringEncoding::MemoryMap,
            padding: 0,
        },
    );

    #[test]
    fn addresses_resolve_per_slot() -> TestResult {
        assert_eq!(GLOBAL.address(None)?.as_u32(), 323_593);
        assert_eq!(GLOBAL.address(Some(SlotIndex::new(3)?))?.as_u32(), 323_593);
        assert_eq!(
            PER_SLOT.address(Some(SlotIndex::new(2)?))?.as_u32(),
            328_995 + 16_384
        );
        let missing = PER_SLOT.address(None);
        assert!(
            matches!(missing, Err(SchemaError::SlotRequired { .. })),
            "{missing:?}"
        );
        Ok(())
    }

    #[test]
    fn codecs_round_trip_through_an_image() -> TestResult {
        let mut image = vec![0u8; IMAGE_LENGTH];
        let slot = Some(SlotIndex::new(1)?);
        for (offset, mask, bits) in NAME.encode(FieldValue::Text("HOME"))? {
            let index = NAME.address(slot)?.as_usize() + offset;
            if let Some(byte) = image.get_mut(index) {
                *byte = (*byte & !mask) | bits;
            }
        }
        assert_eq!(
            NAME.read(&image, slot)?,
            DecodedFieldValue::Text("HOME".to_owned())
        );
        let signed = FieldDescriptor::new(
            "gps.MyPositionList[0].Altitude",
            329_232,
            FieldCodec::Signed {
                width: 4,
                endian: Endian::Little,
                min: -500,
                max: 15_000,
            },
        );
        for (offset, _, bits) in signed.encode(FieldValue::Signed(-500))? {
            if let Some(byte) = image.get_mut(329_232 + offset) {
                *byte = bits;
            }
        }
        assert_eq!(signed.read(&image, None)?, DecodedFieldValue::Signed(-500));
        let too_big = PER_SLOT.encode(FieldValue::Unsigned(3));
        assert!(
            matches!(
                too_big,
                Err(SchemaError::UnsignedOutOfRange { value: 3, .. })
            ),
            "{too_big:?}"
        );
        let wrong_kind = PER_SLOT.encode(FieldValue::Text("x"));
        assert!(
            matches!(wrong_kind, Err(SchemaError::TypeMismatch { .. })),
            "{wrong_kind:?}"
        );
        Ok(())
    }

    #[test]
    fn planner_groups_claims_into_region_pages_and_refuses_conflicts() -> TestResult {
        let mut planner = PatchPlanner::new();
        let slot = Some(SlotIndex::new(0)?);
        let _first = planner.set(&PER_SLOT, slot, FieldValue::Unsigned(1))?;
        let _second = planner.set(&NAME, slot, FieldValue::Text("HOME"))?;
        let set = planner.finish()?;
        assert_eq!(set.len(), 2);
        let first_page = set.pages().first().ok_or("no page")?;
        assert_eq!(first_page.page.address().as_u32(), 327_936 + 1024);
        let bit_a = FieldDescriptor::new("radio.A", 8, FieldCodec::BitBool { mask: 0x01 });
        let bit_b = FieldDescriptor::new("radio.B", 8, FieldCodec::BitBool { mask: 0x01 });
        let mut clash = PatchPlanner::new();
        let _claimed = clash.set(&bit_a, None, FieldValue::Bool(true))?;
        let conflict = clash.set(&bit_b, None, FieldValue::Bool(false));
        assert!(
            matches!(conflict, Err(SchemaError::ByteConflict { address: 8, .. })),
            "{conflict:?}"
        );
        let bitmap =
            FieldDescriptor::new("radio.PoweronBitmap", 393_216, FieldCodec::Bytes { len: 2 });
        let outside = PatchPlanner::new()
            .set(&bitmap, None, FieldValue::Bytes(&[0, 0]))
            .map(|_| ());
        assert!(
            matches!(
                outside,
                Err(SchemaError::NotWritable {
                    address: 393_216,
                    ..
                })
            ),
            "{outside:?}"
        );
        Ok(())
    }
}
