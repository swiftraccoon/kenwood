//! D-STAR voice stream identifier (non-zero u16).
//!
//! Stream ID 0 is reserved by the D-STAR protocol — it is the
//! sentinel for "no active stream". Wrapping `NonZeroU16` makes the
//! invalid value unrepresentable.

use std::num::NonZeroU16;

/// D-STAR voice stream identifier.
///
/// # Invariants
///
/// The wrapped `NonZeroU16` makes the zero case unrepresentable —
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
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:04X}", self.0.get())
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
}
