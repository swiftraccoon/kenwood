//! Checked nonzero identifiers with primitive foreign representations.

use std::fmt;
use std::num::NonZeroU64;

/// A zero candidate rejected before it can identify an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation identifier must be nonzero")]
pub struct IdentifierError;

/// One-use screen token, matched against the actor's retained checked frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScreenLeaseId(NonZeroU64);

impl ScreenLeaseId {
    /// Construct a nonzero token; membership and freshness are checked by the actor.
    ///
    /// # Errors
    ///
    /// Returns [`IdentifierError`] for zero.
    pub const fn new(value: u64) -> Result<Self, IdentifierError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(IdentifierError),
        }
    }
    pub(crate) const fn from_nonzero(value: NonZeroU64) -> Self {
        Self(value)
    }
    /// Return the exact foreign token representation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for ScreenLeaseId {
    type Error = IdentifierError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ScreenLeaseId> for u64 {
    fn from(value: ScreenLeaseId) -> Self {
        value.get()
    }
}
impl fmt::Display for ScreenLeaseId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Nonzero retained setting snapshot, checked for membership at execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingSnapshotId(NonZeroU64);

impl SettingSnapshotId {
    /// Construct a nonzero token; execution requires the matching retained snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`IdentifierError`] for zero.
    pub const fn new(value: u64) -> Result<Self, IdentifierError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(IdentifierError),
        }
    }
    pub(crate) const fn from_nonzero(value: NonZeroU64) -> Self {
        Self(value)
    }
    /// Return the exact foreign token representation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for SettingSnapshotId {
    type Error = IdentifierError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<SettingSnapshotId> for u64 {
    fn from(value: SettingSnapshotId) -> Self {
        value.get()
    }
}
impl fmt::Display for SettingSnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

uniffi::custom_type!(ScreenLeaseId, u64);
uniffi::custom_type!(SettingSnapshotId, u64);

#[cfg(test)]
mod tests {
    use super::{ScreenLeaseId, SettingSnapshotId};

    #[test]
    fn foreign_lifting_rejects_zero_before_the_operation_runs()
    -> Result<(), Box<dyn std::error::Error>> {
        let screen = <ScreenLeaseId as uniffi::Lift<crate::UniFfiTag>>::try_lift(0);
        let settings = <SettingSnapshotId as uniffi::Lift<crate::UniFfiTag>>::try_lift(0);
        assert!(screen.is_err(), "zero screen lease crossed FFI validation");
        assert!(
            settings.is_err(),
            "zero setting snapshot crossed FFI validation"
        );
        assert_eq!(
            <ScreenLeaseId as uniffi::Lift<crate::UniFfiTag>>::try_lift(1)?.get(),
            1
        );
        assert_eq!(
            <SettingSnapshotId as uniffi::Lift<crate::UniFfiTag>>::try_lift(u64::MAX)?.get(),
            u64::MAX
        );
        Ok(())
    }
}
