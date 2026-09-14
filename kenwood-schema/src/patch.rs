//! Atomic, address-neutral masked-byte ownership.

use std::collections::BTreeMap;

/// A validated assignment to selected bits of one byte.
///
/// An offset is relative to the caller's buffer or address space. Constructing
/// an assignment does not establish that its address exists or is writable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskedByte {
    pub(crate) offset: usize,
    pub(crate) mask: u8,
    pub(crate) value: u8,
}

impl MaskedByte {
    /// Construct a nonempty assignment whose value contains only owned bits.
    ///
    /// # Errors
    ///
    /// Returns [`PatchError::InvalidMask`] for an empty mask or unowned bits.
    pub const fn new(offset: usize, mask: u8, value: u8) -> Result<Self, PatchError> {
        if mask == 0 || value & !mask != 0 {
            return Err(PatchError::InvalidMask {
                offset,
                mask,
                value,
            });
        }
        Ok(Self {
            offset,
            mask,
            value,
        })
    }

    /// Byte offset in the caller's chosen address space.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Bits owned by this assignment.
    #[must_use]
    pub const fn mask(self) -> u8 {
        self.mask
    }

    /// Requested values of the owned bits; unowned bits are zero.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }

    /// Relocate an assignment after the caller resolves its address.
    ///
    /// No arithmetic, address validation, or write admission is performed.
    #[must_use]
    pub const fn with_offset(self, offset: usize) -> Self {
        Self { offset, ..self }
    }

    /// Apply owned bits to an original byte, preserving every unowned bit.
    #[must_use]
    pub const fn apply(self, original: u8) -> u8 {
        (original & !self.mask) | self.value
    }
}

/// A malformed assignment or disagreement over previously claimed bits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// An assignment is empty or requests values outside its mask.
    #[error("byte {offset} has invalid mask 0x{mask:02X} or value 0x{value:02X}")]
    InvalidMask {
        /// Assignment's byte offset.
        offset: usize,
        /// Rejected mask.
        mask: u8,
        /// Rejected value.
        value: u8,
    },
    /// An assignment disagrees with at least one already owned bit.
    #[error("field {owner} conflicts with {existing} at byte {offset}, mask 0x{mask:02X}")]
    Conflict {
        /// Field whose assignment was rejected.
        owner: &'static str,
        /// First owner of the lowest conflicting bit.
        existing: &'static str,
        /// Byte containing the disagreement.
        offset: usize,
        /// Overlapping bits whose requested values differ.
        mask: u8,
    },
}

const BITS: [u8; 8] = [1, 2, 4, 8, 16, 32, 64, 128];

#[derive(Debug, Clone)]
struct Claim {
    first_owner: &'static str,
    owners: [&'static str; 8],
    mask: u8,
    value: u8,
}

impl Claim {
    const fn empty(owner: &'static str) -> Self {
        Self {
            first_owner: owner,
            owners: [owner; 8],
            mask: 0,
            value: 0,
        }
    }

    fn merge(&mut self, owner: &'static str, byte: MaskedByte) -> Result<(), PatchError> {
        let differing = (self.value ^ byte.value) & self.mask & byte.mask;
        for (existing, bit) in self.owners.iter().zip(BITS) {
            if differing & bit != 0 {
                return Err(PatchError::Conflict {
                    owner,
                    existing,
                    offset: byte.offset,
                    mask: differing,
                });
            }
        }
        let newly_owned = byte.mask & !self.mask;
        for (existing, bit) in self.owners.iter_mut().zip(BITS) {
            if newly_owned & bit != 0 {
                *existing = owner;
            }
        }
        self.value = byte.apply(self.value);
        self.mask |= byte.mask;
        Ok(())
    }
}

/// Atomic masked assignments, ordered by byte offset.
///
/// Equal repeated assignments are idempotent, including assignments from
/// different fields. Disjoint bits coalesce. Differing overlapping bits fail
/// without modifying any previous assignment. The caller retains descriptor,
/// address, region, and page validation; this planner knows no radio model.
#[derive(Debug, Default)]
pub struct ByteClaims {
    bytes: BTreeMap<usize, Claim>,
}

impl ByteClaims {
    /// Construct an empty set of claims.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: BTreeMap::new(),
        }
    }

    /// Merge one completely validated field assignment atomically.
    ///
    /// Every input byte is checked against earlier claims and earlier bytes
    /// in this same batch. Empty batches are no-ops. A rejected batch leaves
    /// existing claims and their diagnostic ownership unchanged.
    ///
    /// # Examples
    ///
    /// The first byte of a rejected batch must not survive its later conflict.
    /// A subsequent valid request can claim that byte with another value.
    ///
    /// ```
    /// use kenwood_schema::{ByteClaims, MaskedByte, PatchError};
    ///
    /// let mut claims = ByteClaims::new();
    /// let lower = MaskedByte::new(8, 0x0F, 0x05)?;
    /// claims.merge_atomic("lower", &[lower])?;
    /// claims.merge_atomic("same request", &[lower])?;
    /// let rejected = [
    ///     MaskedByte::new(9, 0xFF, 0xAA)?,
    ///     MaskedByte::new(8, 0x0F, 0x06)?,
    /// ];
    /// assert_eq!(claims.merge_atomic("rejected", &rejected), Err(PatchError::Conflict {
    ///     owner: "rejected", existing: "lower", offset: 8, mask: 0x03,
    /// }));
    /// claims.merge_atomic("upper", &[MaskedByte::new(8, 0xF0, 0xA0)?])?;
    /// claims.merge_atomic("replacement", &[MaskedByte::new(9, 0xFF, 0x55)?])?;
    /// let merged: Vec<_> = claims.into_claims().collect();
    /// assert_eq!(merged, [
    ///     ("lower", MaskedByte::new(8, 0xFF, 0xA5)?),
    ///     ("replacement", MaskedByte::new(9, 0xFF, 0x55)?),
    /// ]);
    /// # Ok::<(), PatchError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`PatchError::Conflict`] for differing overlapping bits.
    pub fn merge_atomic(
        &mut self,
        owner: &'static str,
        bytes: &[MaskedByte],
    ) -> Result<(), PatchError> {
        let mut pending = BTreeMap::new();
        for &byte in bytes {
            let claim = pending.entry(byte.offset).or_insert_with(|| {
                self.bytes
                    .get(&byte.offset)
                    .cloned()
                    .unwrap_or_else(|| Claim::empty(owner))
            });
            claim.merge(owner, byte)?;
        }
        self.bytes.append(&mut pending);
        Ok(())
    }

    /// Consume claims in ascending offset order, one assignment per byte.
    ///
    /// Each tuple names the first field to claim any bit at that byte. Conflict
    /// errors retain more precise per-bit ownership. Page grouping and memory
    /// protection remain the caller's responsibility.
    pub fn into_claims(self) -> impl Iterator<Item = (&'static str, MaskedByte)> {
        self.bytes.into_iter().map(|(offset, claim)| {
            (
                claim.first_owner,
                MaskedByte {
                    offset,
                    mask: claim.mask,
                    value: claim.value,
                },
            )
        })
    }
}

#[cfg(test)]
mod tests;
