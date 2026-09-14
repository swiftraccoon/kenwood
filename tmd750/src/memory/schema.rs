//! Model-bound descriptors and region planning over shared scalar codecs.
//!
//! [`FieldDescriptor`] combines a storage codec with an address of the form
//! `base + sum(stride * slot)`. A [`SlotIndex`] of zero denotes PM Off;
//! slots one through five denote PM1 through PM5. Global descriptors need no
//! slot. Use [`super::menu_field`] to select compiled metadata, and
//! [`crate::radio::menu::ScopedMenuField`] when an explicit global/per-PM scope
//! must be validated without silently accepting an irrelevant slot.
//!
//! # Reading and planning
//!
//! [`FieldDescriptor::read`] preserves stored numeric values outside the menu's
//! writable domain, interprets nonzero boolean bytes as true, and stops text
//! at its first NUL or configured padding byte. It checks the supplied span,
//! not whether those bytes were actually captured. For sparse evidence, use
//! [`crate::radio::menu::MenuFieldSnapshot::value`].
//!
//! [`FieldDescriptor::encode`] instead checks the desired writable domain and
//! returns field-relative [`kenwood_schema::MaskedByte`] assignments.
//! [`PatchPlanner`] resolves absolute addresses, validates writable regions, and
//! merges equal overlapping assignments idempotently. A conflicting assignment
//! fails atomically; earlier accepted claims remain intact. [`PatchSet`] groups
//! the result into complete transfer-page scopes without reading or writing them.
//!
//! These storage checks do not establish firmware compatibility, current state,
//! or an ordinary setting's lifecycle policy. Live registered updates require
//! [`crate::radio::menu::MenuUpdatePlan`] and its fresh-page session comparison.
//! Persistent Gateway changes use [`crate::radio::terminal::TerminalPlan`].
//! The shared `kenwood-schema` crate owns scalar encoding and bit claims; this
//! facade owns catalog integrity, PM addressing, image bounds, and page geometry.

use std::collections::BTreeMap;

use kenwood_schema::{
    BooleanDecoding, ByteClaims, DecodeOptions, MaskedByte, TextPolicy, ValueDomain,
};

use crate::error::SchemaError;
use crate::protocol::mcp::regions::writable_page_for;
use crate::protocol::mcp::{BytePatch, PagePatch};
use crate::types::{Address, IMAGE_LENGTH, SLOT_STRIDE, SlotIndex};

pub use kenwood_schema::{DecodedFieldValue, Endian, FieldCodec, FieldValue, StringEncoding};

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

    fn validate(&self) -> Result<(), SchemaError> {
        self.codec.validate().map_err(|source| SchemaError::Codec {
            field: self.name,
            source,
        })?;
        if self.codec.encoded_len() > IMAGE_LENGTH {
            return Err(SchemaError::OutOfBounds {
                field: self.name,
                address: u64::from(self.base),
                len: self.codec.encoded_len(),
                image_length: IMAGE_LENGTH,
            });
        }
        if let Some(registered) = super::menu_field(self.name)
            && registered.descriptor != *self
        {
            return Err(SchemaError::CatalogDescriptorMismatch { field: self.name });
        }
        Ok(())
    }

    /// Resolve the absolute address for `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::SlotRequired`] for a per-slot field without a
    /// slot, [`SchemaError::UnknownDimension`] for a term other than
    /// `pm_slot`, and [`SchemaError::OutOfBounds`] past the image. Malformed
    /// codec shapes and altered registered descriptors are rejected first.
    pub fn address(&self, slot: Option<SlotIndex>) -> Result<Address, SchemaError> {
        self.validate()?;
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
            address = address
                .checked_add(u64::from(term.stride) * u64::from(slot.index()))
                .ok_or_else(|| SchemaError::OutOfBounds {
                    field: self.name,
                    address: u64::MAX,
                    len: self.codec.encoded_len(),
                    image_length: IMAGE_LENGTH,
                })?;
        }
        let len = self.codec.encoded_len();
        let out_of_bounds = SchemaError::OutOfBounds {
            field: self.name,
            address,
            len,
            image_length: IMAGE_LENGTH,
        };
        let end = address
            .checked_add(u64::try_from(len).unwrap_or(u64::MAX))
            .ok_or_else(|| out_of_bounds.clone())?;
        if end > u64::try_from(IMAGE_LENGTH).unwrap_or(u64::MAX) {
            return Err(out_of_bounds);
        }
        u32::try_from(address)
            .ok()
            .and_then(|value| Address::new(value).ok())
            .ok_or(out_of_bounds)
    }

    /// Decode stored bytes without imposing the current writable menu domain.
    ///
    /// Boolean bytes retain the model's nonzero interpretation. Text ends at
    /// the first NUL or configured padding byte; trailing bytes are not treated
    /// as selectable text. Neither policy authorizes a corresponding write.
    ///
    /// # Errors
    ///
    /// Returns address, registry-integrity, and codec errors. A value outside
    /// the current menu's numeric or finite-choice domain remains readable.
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
        self.codec
            .decode(
                bytes,
                DecodeOptions {
                    domain: ValueDomain::Stored,
                    boolean: BooleanDecoding::NonZero,
                    text: TextPolicy::FirstTerminator,
                },
            )
            .map_err(|source| SchemaError::Codec {
                field: self.name,
                source,
            })
    }

    /// Encode one writable value as masked bytes relative to the field start.
    ///
    /// # Errors
    ///
    /// Returns malformed-codec, registry-integrity, type, range, and text errors.
    /// Registered finite writable domains apply even to direct descriptors.
    /// Registered blobs remain available for offline encoding; the planner
    /// separately enforces radio-write admission. Text containing NUL or the
    /// configured padding byte is rejected before any byte is returned.
    pub fn encode(&self, value: FieldValue<'_>) -> Result<Vec<MaskedByte>, SchemaError> {
        self.validate()?;
        if let Some(registered) = super::menu_field(self.name) {
            registered.validate_value_domain(value)?;
        }
        self.codec
            .encode(value, ValueDomain::Writable, TextPolicy::FirstTerminator)
            .map_err(|source| SchemaError::Codec {
                field: self.name,
                source,
            })
    }
}

/// Collects field values into region-aligned masked page patches.
#[derive(Debug, Default)]
pub struct PatchPlanner {
    claims: ByteClaims,
}

impl PatchPlanner {
    /// An empty plan.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            claims: ByteClaims::new(),
        }
    }

    /// Plan `value` for `field` in `slot`.
    ///
    /// The complete assignment is validated before any claims change. Repeated
    /// assignments to the same bits are idempotent when their values agree;
    /// differing overlapping bits are rejected without retaining a prefix.
    /// Registered descriptors enforce their compiled writable domains.
    ///
    /// # Errors
    ///
    /// Returns encode errors, [`SchemaError::NotWritable`] outside the model's
    /// writable regions, and [`SchemaError::Patch`] for conflicting assignments.
    pub fn set(
        &mut self,
        field: &FieldDescriptor,
        slot: Option<SlotIndex>,
        value: FieldValue<'_>,
    ) -> Result<&mut Self, SchemaError> {
        let start = field.address(slot)?;
        if let Some(registered) = super::menu_field(field.name) {
            registered.validate_patch_value(value)?;
        }
        let mut pending = Vec::new();
        for byte in field.encode(value)? {
            let address = start
                .checked_add(u32::try_from(byte.offset()).unwrap_or(u32::MAX))
                .map_err(|_| SchemaError::OutOfBounds {
                    field: field.name,
                    address: u64::from(start.as_u32())
                        + u64::try_from(byte.offset()).unwrap_or(u64::MAX),
                    len: 1,
                    image_length: IMAGE_LENGTH,
                })?;
            if writable_page_for(address).is_none() {
                return Err(SchemaError::NotWritable {
                    field: field.name,
                    address: address.as_u32(),
                });
            }
            pending.push(byte.with_offset(address.as_usize()));
        }
        self.claims.merge_atomic(field.name, &pending)?;
        Ok(self)
    }

    /// Group accepted claims into pages of the model's writable region walk.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::NotWritable`] if a claim has no page, which cannot
    /// occur after [`PatchPlanner::set`] accepted the assignment.
    pub fn finish(self) -> Result<PatchSet, SchemaError> {
        let mut pages: BTreeMap<u32, (crate::types::Page, Vec<BytePatch>)> = BTreeMap::new();
        for (owner, claim) in self.claims.into_claims() {
            let address = u32::try_from(claim.offset()).map_err(|_| SchemaError::OutOfBounds {
                field: owner,
                address: u64::try_from(claim.offset()).unwrap_or(u64::MAX),
                len: 1,
                image_length: IMAGE_LENGTH,
            })?;
            let page = Address::new(address)
                .ok()
                .and_then(writable_page_for)
                .ok_or(SchemaError::NotWritable {
                    field: owner,
                    address,
                })?;
            let entry = pages
                .entry(page.address().as_u32())
                .or_insert_with(|| (page, Vec::new()));
            let offset = u8::try_from(address - page.address().as_u32()).map_err(|_| {
                SchemaError::OutOfBounds {
                    field: owner,
                    address: u64::from(address),
                    len: 1,
                    image_length: IMAGE_LENGTH,
                }
            })?;
            entry
                .1
                .push(BytePatch::new(offset, claim.mask(), claim.value())?);
        }
        Ok(PatchSet {
            pages: pages
                .into_values()
                .map(|(page, bytes)| PagePatch::new(page, bytes))
                .collect::<Result<_, _>>()?,
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

    /// Apply every patch only when the image contains every complete page.
    ///
    /// All bounds are checked before the first mutation. A failed application
    /// leaves the image unchanged, including pages preceding the missing one.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OutOfBounds`] if any complete target page is absent.
    pub fn apply_to_image(&self, image: &mut [u8]) -> Result<(), SchemaError> {
        let image_length = image.len();
        let missing = |page: crate::types::Page| SchemaError::OutOfBounds {
            field: "patch set",
            address: u64::from(page.address().as_u32()),
            len: page.len(),
            image_length,
        };
        for patch in &self.pages {
            let page = patch.page();
            let start = page.address().as_usize();
            let _complete = image
                .get(start..start + page.len())
                .ok_or_else(|| missing(page))?;
        }
        for patch in &self.pages {
            let page = patch.page();
            let start = page.address().as_usize();
            let window = image
                .get_mut(start..start + page.len())
                .ok_or_else(|| missing(page))?;
            patch.apply(window)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "schema_regression_tests.rs"]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_schema::{CodecError, PatchError};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const GLOBAL: FieldDescriptor =
        FieldDescriptor::new("test.Global", 323_593, FieldCodec::Byte { min: 0, max: 5 });
    const PER_SLOT: FieldDescriptor = FieldDescriptor::with_terms(
        "test.PerSlot",
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
        for patch in NAME.encode(FieldValue::Text("HOME"))? {
            let index = NAME.address(slot)?.as_usize() + patch.offset();
            if let Some(byte) = image.get_mut(index) {
                *byte = patch.apply(*byte);
            }
        }
        assert_eq!(
            NAME.read(&image, slot)?,
            DecodedFieldValue::Text("HOME".to_owned())
        );
        let signed = FieldDescriptor::new(
            "test.SignedAltitude",
            329_232,
            FieldCodec::Signed {
                width: 4,
                endian: Endian::Little,
                min: -500,
                max: 15_000,
            },
        );
        for patch in signed.encode(FieldValue::Signed(-500))? {
            if let Some(byte) = image.get_mut(329_232 + patch.offset()) {
                *byte = patch.apply(*byte);
            }
        }
        assert_eq!(signed.read(&image, None)?, DecodedFieldValue::Signed(-500));
        let too_big = PER_SLOT.encode(FieldValue::Unsigned(3));
        assert!(
            matches!(
                too_big,
                Err(SchemaError::Codec {
                    source: CodecError::UnsignedOutOfRange { value: 3, .. },
                    ..
                })
            ),
            "{too_big:?}"
        );
        let wrong_kind = PER_SLOT.encode(FieldValue::Text("x"));
        assert!(
            matches!(
                wrong_kind,
                Err(SchemaError::Codec {
                    source: CodecError::TypeMismatch { .. },
                    ..
                })
            ),
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
        assert_eq!(first_page.page().address().as_u32(), 327_936 + 1024);
        let bit_a = FieldDescriptor::new("radio.A", 8, FieldCodec::BitBool { mask: 0x01 });
        let bit_b = FieldDescriptor::new("radio.B", 8, FieldCodec::BitBool { mask: 0x01 });
        let mut clash = PatchPlanner::new();
        let _claimed = clash.set(&bit_a, None, FieldValue::Bool(true))?;
        let conflict = clash.set(&bit_b, None, FieldValue::Bool(false));
        assert!(
            matches!(
                conflict,
                Err(SchemaError::Patch(PatchError::Conflict { offset: 8, .. }))
            ),
            "{conflict:?}"
        );
        let bitmap = FieldDescriptor::new("test.Bitmap", 393_216, FieldCodec::Bytes { len: 2 });
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
