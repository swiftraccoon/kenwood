//! D-STAR voice stream identifier (non-zero u16).
//!
//! Stream ID 0 is reserved by the D-STAR protocol as the
//! sentinel for "no active stream". Wrapping `NonZeroU16` makes the
//! invalid value unrepresentable.

use std::num::NonZeroU16;

/// D-STAR voice stream identifier.
///
/// # Invariants
///
/// The wrapped `NonZeroU16` makes the zero case unrepresentable;
/// any code path holding a `StreamId` is statically guaranteed not
/// to be carrying a malformed stream id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc(alias = "stream-id")]
#[doc(alias = "voice-stream-id")]
pub struct StreamId(NonZeroU16);

impl StreamId {
    /// Construct a `StreamId`, returning `None` if `n == 0`.
    #[must_use]
    pub const fn new(n: u16) -> Option<Self> {
        match NonZeroU16::new(n) {
            Some(nz) => Some(Self(nz)),
            None => None,
        }
    }

    /// Return the raw u16.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    /// Derive a valid stream ID from arbitrary seed bits.
    ///
    /// Non-zero seeds map to themselves; the protocol-reserved zero
    /// maps to `1`. The caller selects seeds and handles collisions
    /// with other streams on the same link.
    #[must_use]
    pub const fn from_seed(seed: u16) -> Self {
        match NonZeroU16::new(seed) {
            Some(nz) => Self(nz),
            None => Self(NonZeroU16::MIN),
        }
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:04X}", self.0.get())
    }
}

impl std::fmt::Binary for StreamId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Binary::fmt(&self.0, formatter)
    }
}

impl std::fmt::Octal for StreamId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Octal::fmt(&self.0, formatter)
    }
}

impl std::fmt::LowerHex for StreamId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl std::fmt::UpperHex for StreamId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperHex::fmt(&self.0, formatter)
    }
}

impl TryFrom<u16> for StreamId {
    type Error = std::num::TryFromIntError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        NonZeroU16::try_from(value).map(Self)
    }
}

impl From<StreamId> for u16 {
    fn from(value: StreamId) -> Self {
        value.get()
    }
}

impl From<NonZeroU16> for StreamId {
    fn from(value: NonZeroU16) -> Self {
        Self(value)
    }
}

impl From<StreamId> for NonZeroU16 {
    fn from(value: StreamId) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn stream_id_rejects_zero() {
        assert!(StreamId::new(0).is_none());
    }

    #[test]
    fn stream_id_accepts_non_zero() -> TestResult {
        let sid = StreamId::new(0x1234).ok_or("non-zero must be accepted")?;
        assert_eq!(sid.get(), 0x1234);
        Ok(())
    }

    #[test]
    fn stream_id_display_hex() -> TestResult {
        let sid = StreamId::new(0x00AB).ok_or("non-zero must be accepted")?;
        assert_eq!(format!("{sid}"), "0x00AB");
        Ok(())
    }

    #[test]
    fn from_seed_preserves_non_zero_seeds() {
        assert_eq!(StreamId::from_seed(0x0001).get(), 0x0001);
        assert_eq!(StreamId::from_seed(0x1234).get(), 0x1234);
        assert_eq!(StreamId::from_seed(0xFFFF).get(), 0xFFFF);
    }

    #[test]
    fn from_seed_maps_zero_to_a_valid_id() {
        assert_eq!(StreamId::from_seed(0).get(), 1);
    }
}
