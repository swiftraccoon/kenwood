//! Schema-driven MCP menu fields and safe masked patch planning.
//!
//! The official MCP-D75 application serializes menu properties into a raw
//! 500,480-byte image.  [`FieldDescriptor`] models those serializer writes,
//! while [`PatchPlanner`] converts requested values into byte masks that can
//! be applied to freshly-read radio pages.  Bit fields therefore preserve
//! unrelated bits even when the caller does not hold a current full image.
//!
//! Scalar codecs and atomic bit ownership belong to `kenwood-schema`.
//! This facade selects canonical booleans and exact text padding, validates
//! catalog identity and menu domains, and maps claims onto writable TH-D75
//! pages. It performs no protocol I/O or connection recovery.

use std::collections::BTreeMap;
use std::fmt;

use kenwood_schema::{
    BooleanDecoding, ByteClaims, CodecError, DecodeOptions, MaskedByte, PatchError, TextPolicy,
    ValueDomain,
};
pub use kenwood_schema::{DecodedFieldValue, Endian, FieldCodec, FieldValue, StringEncoding};

use crate::protocol::programming::{self, McpPage, WritableMcpPage};

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
    /// Returns [`SchemaError::OffsetTooLarge`] when the span overflows the
    /// host's address space, or
    /// [`SchemaError::OutOfBounds`] when the span leaves the physical image.
    /// Malformed storage metadata returns [`SchemaError::Codec`] before allocation.
    pub fn pages(self) -> Result<Vec<McpPage>, SchemaError> {
        self.codec
            .validate()
            .map_err(|source| self.codec_error(source))?;
        let len = self.codec.encoded_len();
        let last_offset = self
            .offset
            .checked_add(len - 1)
            .ok_or(SchemaError::OffsetTooLarge {
                field: self.name,
                offset: self.offset,
            })?;
        if last_offset >= programming::TOTAL_SIZE {
            return Err(SchemaError::OutOfBounds {
                field: self.name,
                offset: self.offset,
                len,
                image_len: programming::TOTAL_SIZE,
            });
        }
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
    /// [`CodecError::FixedStringDataAfterNul`] identifies an ambiguous
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
        self.codec
            .validate()
            .map_err(|source| self.codec_error(source))?;
        let bytes = read_range(image, self.name, self.offset, self.codec.encoded_len())?;
        let decoded = self
            .codec
            .decode(
                bytes,
                DecodeOptions {
                    domain,
                    boolean: BooleanDecoding::Canonical,
                    text: TextPolicy::ExactPadding,
                },
            )
            .map_err(|source| self.codec_error(source))?;

        if domain == ValueDomain::Writable
            && let Some(field) = menu_field
        {
            field.validate_patch_value(decoded.as_field_value())?;
        }

        Ok(decoded)
    }

    const fn codec_error(self, source: CodecError) -> SchemaError {
        SchemaError::Codec {
            field: self.name,
            source,
        }
    }
}

/// Failure while validating, encoding, merging, or applying schema patches.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaError {
    /// Scalar storage validation failed, retaining the shared typed cause.
    Codec {
        /// Field whose storage representation was rejected.
        field: &'static str,
        /// Model-neutral codec failure with field-relative byte positions.
        source: CodecError,
    },
    /// Bit ownership validation failed before the plan was changed.
    Patch(PatchError),
    /// A finite menu domain requires a different value kind.
    TypeMismatch {
        /// Field name.
        field: &'static str,
        /// Codec's expected value kind.
        expected: &'static str,
        /// Supplied value kind.
        actual: &'static str,
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
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec { field, source } => write!(f, "field {field}: {source}"),
            Self::Patch(source) => source.fmt(f),
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => write!(f, "field {field} expects {expected}, received {actual}"),
            Self::DisallowedValue { field, value } => {
                write!(f, "field {field} does not allow raw value {value}")
            }
            Self::CatalogDescriptorMismatch {
                field,
                offset,
                expected_offset,
            } => fmt_catalog_descriptor_mismatch(f, field, *offset, *expected_offset),
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

impl std::error::Error for SchemaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec { source, .. } => Some(source),
            Self::Patch(source) => Some(source),
            _ => None,
        }
    }
}

/// Masked changes for one 256-byte MCP page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePatch {
    page: WritableMcpPage,
    bytes: Vec<MaskedByte>,
}

impl PagePatch {
    /// Validated writable MCP page address.
    #[must_use]
    pub const fn page(&self) -> WritableMcpPage {
        self.page
    }

    /// Sorted byte patches within this page.
    #[must_use]
    pub fn bytes(&self) -> &[MaskedByte] {
        &self.bytes
    }

    /// Apply this patch to a freshly-read page.
    ///
    /// Every byte update is masked so unrelated bits remain unchanged.
    pub fn apply_to_page(&self, page: &mut [u8; programming::PAGE_SIZE]) {
        for patch in &self.bytes {
            if let Some(byte) = page.get_mut(patch.offset()) {
                *byte = patch.apply(*byte);
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
                let absolute = page_start + patch.offset();
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
                let absolute = page_start + patch.offset();
                if let Some(byte) = image.get_mut(absolute) {
                    *byte = patch.apply(*byte);
                }
            }
        }
        Ok(())
    }
}

/// Builds a [`PatchSet`] without requiring a cached memory image.
#[derive(Debug, Default)]
pub struct PatchPlanner {
    bytes: ByteClaims,
}

impl PatchPlanner {
    /// Create an empty planner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: ByteClaims::new(),
        }
    }

    /// Add a requested menu field value.
    ///
    /// Non-overlapping bits at the same byte are coalesced.  Overlapping bits
    /// may be repeated only when both assignments request the same value;
    /// assigning a different value to already-claimed bits is a
    /// [`PatchError::Conflict`]. A later assignment therefore never
    /// silently replaces an earlier one. Every assignment is checked in full
    /// before merging; an error leaves all previously planned bytes and bit
    /// claims unchanged, and the planner remains usable.
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
        if let Some(menu_field) = field.validate_catalog_descriptor()? {
            menu_field.validate_patch_value(value)?;
        }
        let encoded = encode_field(field, value, ValueDomain::Writable)?;
        self.bytes
            .merge_atomic(field.name, &encoded)
            .map_err(SchemaError::Patch)?;
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
        let mut pages: BTreeMap<WritableMcpPage, Vec<MaskedByte>> = BTreeMap::new();
        for (owner, claim) in self.bytes.into_claims() {
            let absolute = claim.offset();
            if absolute >= programming::TOTAL_SIZE {
                return Err(SchemaError::OutOfBounds {
                    field: owner,
                    offset: absolute,
                    len: 1,
                    image_len: programming::TOTAL_SIZE,
                });
            }
            let page_number = absolute / programming::PAGE_SIZE;
            let page = u16::try_from(page_number).map_err(|_| SchemaError::OffsetTooLarge {
                field: owner,
                offset: absolute,
            })?;
            let physical_page = McpPage::new(page).map_err(|_| SchemaError::OutOfBounds {
                field: owner,
                offset: absolute,
                len: 1,
                image_len: programming::TOTAL_SIZE,
            })?;
            let writable_page = WritableMcpPage::from_page(physical_page).map_err(|_| {
                SchemaError::WriteProtected {
                    field: owner,
                    page: physical_page,
                }
            })?;
            let in_page = absolute % programming::PAGE_SIZE;
            pages
                .entry(writable_page)
                .or_default()
                .push(claim.with_offset(in_page));
        }
        Ok(PatchSet {
            pages: pages
                .into_iter()
                .map(|(page, bytes)| PagePatch { page, bytes })
                .collect(),
        })
    }
}

fn encode_field(
    field: &FieldDescriptor,
    value: FieldValue<'_>,
    domain: ValueDomain,
) -> Result<Vec<MaskedByte>, SchemaError> {
    field
        .codec
        .validate()
        .map_err(|source| field.codec_error(source))?;
    let len = field.codec.encoded_len();
    if len > programming::TOTAL_SIZE {
        return Err(SchemaError::OutOfBounds {
            field: field.name,
            offset: field.offset,
            len,
            image_len: programming::TOTAL_SIZE,
        });
    }
    let _last_offset = checked_offset(field, len - 1)?;
    field
        .codec
        .encode(value, domain, TextPolicy::ExactPadding)
        .map_err(|source| field.codec_error(source))?
        .into_iter()
        .map(|byte| Ok(byte.with_offset(checked_offset(field, byte.offset())?)))
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
    use kenwood_schema::FieldCodec as SharedFieldCodec;

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
    fn stored_boolean_stays_canonical_with_a_typed_shared_error() -> TestResult {
        let codec: SharedFieldCodec = FieldCodec::Bool;
        let field = FieldDescriptor::new("test.bool", 0, codec);
        let Err(error) = field.read_stored(&[2]) else {
            return Err("stored decoding must not normalize an invalid boolean".into());
        };
        assert_eq!(
            error,
            SchemaError::Codec {
                field: "test.bool",
                source: CodecError::NonCanonicalBoolean { value: 2 },
            }
        );
        assert!(
            std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<CodecError>())
                .is_some_and(|source| *source == CodecError::NonCanonicalBoolean { value: 2 })
        );
        Ok(())
    }

    #[test]
    fn malformed_spans_are_rejected_before_allocation_or_plan_mutation() -> TestResult {
        let mut planner = PatchPlanner::new();
        let _planner = planner.set(&ENABLE, FieldValue::Unsigned(1))?;
        let empty = FieldDescriptor::new("test.empty", 1, FieldCodec::Bytes { len: 0 });
        assert!(matches!(empty.pages(), Err(SchemaError::Codec { .. })));
        assert!(matches!(
            planner.set(&empty, FieldValue::Bytes(&[])),
            Err(SchemaError::Codec { .. })
        ));
        for len in [programming::TOTAL_SIZE + 1, usize::MAX] {
            let huge = FieldDescriptor::new(
                "test.huge",
                0,
                FieldCodec::FixedString {
                    len,
                    encoding: StringEncoding::Utf8,
                    padding: 0,
                },
            );
            assert!(matches!(huge.pages(), Err(SchemaError::OutOfBounds { .. })));
            assert!(matches!(
                planner.set(&huge, FieldValue::Text("")),
                Err(SchemaError::OutOfBounds { .. })
            ));
        }
        let mut expected = PatchPlanner::new();
        let _expected = expected.set(&ENABLE, FieldValue::Unsigned(1))?;
        assert_eq!(planner.finish()?, expected.finish()?);
        Ok(())
    }

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
        assert_eq!(bytes.first().map(|patch| patch.value()), Some(0x0C));

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
                Err(SchemaError::Patch(PatchError::Conflict {
                    owner: "test.contradictory",
                    existing: "test.enable",
                    ..
                }))
            ),
            "conflict must name both fields: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn rejected_assignment_preserves_the_entire_existing_plan() -> TestResult {
        let anchor =
            FieldDescriptor::new("test.anchor", 0x1011, FieldCodec::Byte { min: 0, max: 255 });
        let crossing = FieldDescriptor::new("test.crossing", 0x1010, FieldCodec::Bytes { len: 2 });
        for prefix_claimed in [false, true] {
            let mut planner = PatchPlanner::new();
            let mut expected = PatchPlanner::new();
            if prefix_claimed {
                let _planner = planner.set(&ENABLE, FieldValue::Unsigned(0))?;
                let _expected = expected.set(&ENABLE, FieldValue::Unsigned(0))?;
            }
            let _planner = planner.set(&anchor, FieldValue::Unsigned(1))?;
            let _expected = expected.set(&anchor, FieldValue::Unsigned(1))?;

            let result = planner.set(&crossing, FieldValue::Bytes(&[0x08, 0]));
            assert!(
                matches!(
                    result,
                    Err(SchemaError::Patch(PatchError::Conflict {
                        owner: "test.crossing",
                        existing: "test.anchor",
                        offset: 0x1011,
                        ..
                    }))
                ),
                "the later byte must reject the entire assignment: {result:?}"
            );
            assert_eq!(
                planner.finish()?,
                expected.finish()?,
                "failed assignment must neither insert bytes nor expand existing bit claims"
            );
        }
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
                Err(SchemaError::Codec {
                    field: "test.nul_text",
                    source: CodecError::TextContainsNul { offset: 1 },
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
                Err(SchemaError::Codec {
                    field: "test.nul_image",
                    source: CodecError::FixedStringDataAfterNul {
                        terminator_offset: 1,
                        offset: 2,
                        value: b'B',
                    },
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
                Err(SchemaError::Codec {
                    field: "test.space_text",
                    source: CodecError::TextEndsWithPadding {
                        offset: 2,
                        padding: b' '
                    },
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
                Err(SchemaError::Codec {
                    field: "test.memory_map_text",
                    source: CodecError::InvalidMemoryMapTextByte {
                        offset: actual_offset,
                        value: actual_value,
                    },
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
            Err(SchemaError::Codec {
                field: "test.memory_map_text",
                source: CodecError::InvalidMemoryMapTextByte {
                    offset: 1,
                    value: 0x1F
                },
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
                Err(SchemaError::Codec {
                    field: "test.other_padding",
                    source: CodecError::TextEndsWithPadding {
                        offset: 3,
                        padding: b'~'
                    },
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
                    Err(SchemaError::Codec {
                        field: error_field,
                        source: CodecError::InvalidIntegerWidth { width },
                    }) if error_field == field.name && width == expected_width
                ),
                "invalid width must be rejected before reading the image: {read:?}"
            );

            let mut planner = PatchPlanner::new();
            let write = planner.set(&field, value);
            assert!(
                matches!(
                    write,
                    Err(SchemaError::Codec {
                        field: error_field,
                        source: CodecError::InvalidIntegerWidth { width },
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
            Err(SchemaError::Codec {
                source: CodecError::UnsignedOutOfRange { .. },
                ..
            })
        ));
        assert!(matches!(
            planner.set(&byte, FieldValue::Bool(true)),
            Err(SchemaError::Codec {
                source: CodecError::TypeMismatch { .. },
                ..
            })
        ));
        assert!(matches!(
            planner.set(&bytes, FieldValue::Bytes(&[1])),
            Err(SchemaError::Codec {
                source: CodecError::ByteLength { .. },
                ..
            })
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

        for field in [byte, bit_field, unsigned] {
            assert!(matches!(
                field.read(&image),
                Err(SchemaError::Codec {
                    source: CodecError::UnsignedOutOfRange { .. },
                    ..
                })
            ));
        }
        assert!(matches!(
            boolean.read(&image),
            Err(SchemaError::Codec {
                source: CodecError::NonCanonicalBoolean { value: 2 },
                ..
            })
        ));
        assert!(matches!(
            signed.read(&image),
            Err(SchemaError::Codec {
                source: CodecError::SignedOutOfRange { .. },
                ..
            })
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
                Err(SchemaError::Codec {
                    field: "test.wide_unsigned",
                    source: CodecError::DomainExceedsWidth { width: 1 },
                })
            ),
            "an over-wide unsigned domain must not truncate: {unsigned_result:?}"
        );
        let signed_result = planner.set(&signed, FieldValue::Signed(-40_000));
        assert!(
            matches!(
                signed_result,
                Err(SchemaError::Codec {
                    field: "test.wide_signed",
                    source: CodecError::DomainExceedsWidth { width: 2 },
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
