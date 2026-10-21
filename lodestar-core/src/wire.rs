//! Checked foreign representations of D-STAR stream and superframe values.

use std::fmt;

use dstar_gateway_core::{FrameSequence, StreamId};

/// A nonzero D-STAR voice stream identifier, lifted from a foreign `u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoiceStreamId(StreamId);

impl VoiceStreamId {
    /// Validate a stream identifier before queueing a transmission.
    ///
    /// # Errors
    ///
    /// Returns [`WireValueError::ZeroStreamId`] for zero.
    pub fn new(raw: u16) -> Result<Self, WireValueError> {
        StreamId::new(raw)
            .map(Self)
            .ok_or(WireValueError::ZeroStreamId)
    }

    /// The nonzero wire value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    pub(crate) const fn core(self) -> StreamId {
        self.0
    }
}

impl TryFrom<u16> for VoiceStreamId {
    type Error = WireValueError;
    fn try_from(raw: u16) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

impl From<VoiceStreamId> for u16 {
    fn from(value: VoiceStreamId) -> Self {
        value.get()
    }
}

uniffi::custom_type!(VoiceStreamId, u16, {
    lower: |value| value.get(),
    try_lift: |raw| Ok(VoiceStreamId::new(raw)?),
});

/// A D-STAR index in `0..=20`, without header or end-of-transmission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VoiceSequence(FrameSequence);

impl VoiceSequence {
    /// The synchronization index at the beginning of a superframe.
    pub const ZERO: Self = Self(FrameSequence::ZERO);

    /// Validate an index without normalizing flag bits or malformed values.
    ///
    /// # Errors
    ///
    /// Returns [`WireValueError::InvalidSequence`] for values above 20.
    pub fn new(raw: u8) -> Result<Self, WireValueError> {
        FrameSequence::new(raw)
            .map(Self)
            .ok_or(WireValueError::InvalidSequence { raw })
    }

    /// The index within its 21-frame superframe.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }

    /// The next index, wrapping from 20 to zero.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.next())
    }

    pub(crate) const fn core(self) -> FrameSequence {
        self.0
    }
}

impl TryFrom<u8> for VoiceSequence {
    type Error = WireValueError;
    fn try_from(raw: u8) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

impl From<VoiceSequence> for u8 {
    fn from(value: VoiceSequence) -> Self {
        value.get()
    }
}

uniffi::custom_type!(VoiceSequence, u8, {
    lower: |value| value.get(),
    try_lift: |raw| Ok(VoiceSequence::new(raw)?),
});

impl fmt::Display for VoiceStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.get(), f)
    }
}
impl fmt::Binary for VoiceStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.get(), f)
    }
}
impl fmt::Octal for VoiceStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.get(), f)
    }
}
impl fmt::LowerHex for VoiceStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.get(), f)
    }
}
impl fmt::UpperHex for VoiceStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.get(), f)
    }
}
impl fmt::Display for VoiceSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.get(), f)
    }
}
impl fmt::Binary for VoiceSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.get(), f)
    }
}
impl fmt::Octal for VoiceSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.get(), f)
    }
}
impl fmt::LowerHex for VoiceSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.get(), f)
    }
}
impl fmt::UpperHex for VoiceSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.get(), f)
    }
}

/// Invalid D-STAR values rejected before protocol work is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireValueError {
    /// Zero cannot identify a voice stream.
    #[error("voice stream identifier must be nonzero")]
    ZeroStreamId,
    /// The candidate includes flags or lies outside the superframe.
    #[error("voice sequence {raw} is outside 0..=20")]
    InvalidSequence {
        /// Rejected original byte.
        raw: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_values_use_the_same_checked_construction() -> Result<(), Box<dyn std::error::Error>>
    {
        assert!(
            <VoiceStreamId as uniffi::Lift<crate::UniFfiTag>>::try_lift(0).is_err(),
            "foreign zero stream was admitted"
        );
        for raw in [21, 0x40, 0x80, 255] {
            assert!(
                <VoiceSequence as uniffi::Lift<crate::UniFfiTag>>::try_lift(raw).is_err(),
                "foreign sequence was normalized: {raw}"
            );
        }
        assert_eq!(VoiceStreamId::new(u16::MAX)?.get(), u16::MAX);
        assert_eq!(VoiceSequence::new(20)?.next(), VoiceSequence::ZERO);
        let value = VoiceSequence::new(20)?;
        assert_eq!(
            format!("{value} {value:b} {value:o} {value:x} {value:X}"),
            "20 10100 24 14 14"
        );
        Ok(())
    }
}
