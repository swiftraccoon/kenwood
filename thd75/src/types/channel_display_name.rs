//! Validated memory-channel identifiers and display names.

use std::fmt;

use crate::error::ValidationError;

/// A regular memory channel number in the inclusive range 0-999.
///
/// Special memories such as program-scan edges and the priority channel use
/// other selectors and cannot be represented by this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegularChannel(u16);

impl RegularChannel {
    /// Lowest regular memory channel number.
    pub const MIN: u16 = 0;

    /// Highest regular memory channel number.
    pub const MAX: u16 = 999;

    /// Number of regular memory channels.
    pub const COUNT: usize = 1_000;

    /// Construct a regular memory channel number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ChannelOutOfRange`] when `value` exceeds
    /// channel 999.
    pub const fn new(value: u16) -> Result<Self, ValidationError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(ValidationError::ChannelOutOfRange {
                channel: value,
                max: Self::MAX,
            })
        }
    }

    /// Return the channel number.
    #[must_use]
    pub const fn as_raw(self) -> u16 {
        self.0
    }

    /// Iterate over every regular memory channel in numeric order.
    pub fn all() -> impl ExactSizeIterator<Item = Self> + DoubleEndedIterator {
        (Self::MIN..=Self::MAX).map(Self)
    }

    /// Iterate over an inclusive range of regular memory channels.
    ///
    /// The iterator is empty when `first` is greater than `last`.
    pub fn range_inclusive(
        first: Self,
        last: Self,
    ) -> impl ExactSizeIterator<Item = Self> + DoubleEndedIterator {
        (first.0..=last.0).map(Self)
    }
}

impl TryFrom<u16> for RegularChannel {
    type Error = ValidationError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RegularChannel> for u16 {
    fn from(channel: RegularChannel) -> Self {
        channel.as_raw()
    }
}

impl From<RegularChannel> for usize {
    fn from(channel: RegularChannel) -> Self {
        Self::from(channel.as_raw())
    }
}

impl fmt::Display for RegularChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A memory channel's user-visible name.
///
/// The TH-D75 stores each name in a 16-byte, NUL-padded field. Valid names are
/// zero to sixteen printable ASCII bytes. Spaces are data, including leading
/// and trailing spaces, and are never trimmed by this type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ChannelDisplayName(String);

impl ChannelDisplayName {
    /// Maximum encoded name length.
    pub const MAX_LEN: usize = 16;

    /// Width of the binary memory-image field.
    pub const WIRE_LEN: usize = 16;

    /// Construct a channel display name from user text.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ChannelDisplayNameTooLong`] when `name`
    /// exceeds sixteen bytes. Returns
    /// [`ValidationError::InvalidChannelDisplayNameByte`] when any byte is
    /// outside printable ASCII (`0x20`-`0x7E`).
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        if name.len() > Self::MAX_LEN {
            return Err(ValidationError::ChannelDisplayNameTooLong { len: name.len() });
        }

        if let Some((offset, value)) = name
            .bytes()
            .enumerate()
            .find(|(_, value)| !is_printable_ascii(*value))
        {
            return Err(ValidationError::InvalidChannelDisplayNameByte { offset, value });
        }

        Ok(Self(name.to_owned()))
    }

    /// Decode a 16-byte memory-image name field.
    ///
    /// A field may contain sixteen printable bytes with no terminator, or a
    /// printable prefix followed exclusively by NUL padding.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidChannelDisplayNameByte`] when the
    /// name contains a byte outside printable ASCII. Returns
    /// [`ValidationError::ChannelDisplayNameDataAfterNul`] when any nonzero
    /// byte follows the first NUL.
    pub fn try_from_wire(bytes: [u8; Self::WIRE_LEN]) -> Result<Self, ValidationError> {
        let name_len = bytes
            .iter()
            .position(|&value| value == 0)
            .unwrap_or(Self::WIRE_LEN);

        if let Some((offset, &value)) = bytes
            .iter()
            .enumerate()
            .skip(name_len.saturating_add(1))
            .find(|(_, value)| **value != 0)
        {
            return Err(ValidationError::ChannelDisplayNameDataAfterNul { offset, value });
        }

        if let Some((offset, &value)) = bytes
            .iter()
            .take(name_len)
            .enumerate()
            .find(|(_, value)| !is_printable_ascii(**value))
        {
            return Err(ValidationError::InvalidChannelDisplayNameByte { offset, value });
        }

        let name = bytes
            .iter()
            .take(name_len)
            .map(|&value| char::from(value))
            .collect();
        Ok(Self(name))
    }

    /// Encode the name as a 16-byte, NUL-padded memory-image field.
    #[must_use]
    pub fn to_wire_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut bytes = [0; Self::WIRE_LEN];
        bytes
            .iter_mut()
            .zip(self.0.bytes())
            .for_each(|(destination, source)| *destination = source);
        bytes
    }

    /// Return the name as text without trimming any spaces.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return `true` when the name contains no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return the encoded length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for ChannelDisplayName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for ChannelDisplayName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

const fn is_printable_ascii(value: u8) -> bool {
    value == b' ' || value.is_ascii_graphic()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn regular_channel_accepts_exact_boundaries() -> TestResult {
        let minimum = RegularChannel::new(RegularChannel::MIN)?;
        let maximum = RegularChannel::try_from(RegularChannel::MAX)?;

        assert_eq!(minimum.as_raw(), 0, "minimum channel should be zero");
        assert_eq!(maximum.as_raw(), 999, "maximum channel should be 999");
        assert_eq!(RegularChannel::COUNT, 1_000, "channel count should be 1000");
        assert_eq!(maximum.to_string(), "999", "display should use decimal");
        Ok(())
    }

    #[test]
    fn regular_channel_rejects_first_value_above_range() -> TestResult {
        let error = RegularChannel::new(1_000)
            .err()
            .ok_or("channel 1000 should have been rejected")?;

        assert!(
            matches!(
                error,
                ValidationError::ChannelOutOfRange {
                    channel: 1_000,
                    max: 999
                }
            ),
            "wrong validation error: {error:?}"
        );
        Ok(())
    }

    #[test]
    fn regular_channel_iterators_preserve_the_valid_domain() -> TestResult {
        let all: Vec<_> = RegularChannel::all().collect();
        assert_eq!(all.len(), RegularChannel::COUNT);
        assert_eq!(all.first().copied(), Some(RegularChannel::new(0)?));
        assert_eq!(all.last().copied(), Some(RegularChannel::new(999)?));

        let range: Vec<_> =
            RegularChannel::range_inclusive(RegularChannel::new(998)?, RegularChannel::new(999)?)
                .collect();
        assert_eq!(
            range,
            [RegularChannel::new(998)?, RegularChannel::new(999)?]
        );
        assert!(
            RegularChannel::range_inclusive(RegularChannel::new(10)?, RegularChannel::new(9)?)
                .next()
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn display_name_accepts_empty_and_full_width_values() -> TestResult {
        let empty = ChannelDisplayName::new("")?;
        let full = ChannelDisplayName::new("0123456789ABCDEF")?;

        assert!(empty.is_empty(), "empty input should remain empty");
        assert_eq!(empty.len(), 0, "empty name should have length zero");
        assert_eq!(full.len(), 16, "full-width name should have length 16");
        assert_eq!(
            full.as_str(),
            "0123456789ABCDEF",
            "name should be unchanged"
        );
        assert_eq!(
            full.as_ref(),
            "0123456789ABCDEF",
            "AsRef should expose the name"
        );
        assert_eq!(
            full.to_string(),
            "0123456789ABCDEF",
            "Display should expose the name"
        );
        Ok(())
    }

    #[test]
    fn display_name_preserves_every_space() -> TestResult {
        let name = ChannelDisplayName::new("  LOCAL   ")?;

        assert_eq!(name.as_str(), "  LOCAL   ", "spaces must not be trimmed");
        assert_eq!(
            ChannelDisplayName::try_from_wire(name.to_wire_bytes())?,
            name,
            "wire round trip must preserve spaces"
        );
        Ok(())
    }

    #[test]
    fn display_name_rejects_values_over_sixteen_bytes() -> TestResult {
        let error = ChannelDisplayName::new("0123456789ABCDEFG")
            .err()
            .ok_or("17-byte name should have been rejected")?;

        assert!(
            matches!(
                error,
                ValidationError::ChannelDisplayNameTooLong { len: 17 }
            ),
            "wrong validation error: {error:?}"
        );
        Ok(())
    }

    #[test]
    fn display_name_rejects_nul_control_and_non_ascii_input() -> TestResult {
        for (name, expected_offset, expected_value) in
            [("A\0B", 1, 0), ("A\rB", 1, b'\r'), ("Aé", 1, 0xC3)]
        {
            let error = ChannelDisplayName::new(name)
                .err()
                .ok_or("invalid name should have been rejected")?;
            assert!(
                matches!(
                    error,
                    ValidationError::InvalidChannelDisplayNameByte { offset, value }
                        if offset == expected_offset && value == expected_value
                ),
                "wrong validation error for {name:?}: {error:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn display_name_decodes_nul_padding_and_full_width_fields() -> TestResult {
        let mut padded = [0; ChannelDisplayName::WIRE_LEN];
        padded
            .iter_mut()
            .zip(b"CALL".iter().copied())
            .for_each(|(destination, source)| *destination = source);

        let short = ChannelDisplayName::try_from_wire(padded)?;
        let full = ChannelDisplayName::try_from_wire(*b"0123456789ABCDEF")?;

        assert_eq!(short.as_str(), "CALL", "NUL padding should be removed");
        assert_eq!(
            ChannelDisplayName::new("CALL")?.to_wire_bytes(),
            padded,
            "short names should encode with NUL padding"
        );
        assert_eq!(
            full.as_str(),
            "0123456789ABCDEF",
            "all 16 bytes are name data"
        );
        assert_eq!(
            ChannelDisplayName::default().to_wire_bytes(),
            [0; ChannelDisplayName::WIRE_LEN],
            "empty names should encode as all NUL bytes"
        );
        Ok(())
    }

    #[test]
    fn display_name_rejects_invalid_wire_byte() -> TestResult {
        let mut bytes = [0; ChannelDisplayName::WIRE_LEN];
        let first = bytes
            .first_mut()
            .ok_or("16-byte field should have a first byte")?;
        *first = 0x1F;

        let error = ChannelDisplayName::try_from_wire(bytes)
            .err()
            .ok_or("invalid wire byte should have been rejected")?;
        assert!(
            matches!(
                error,
                ValidationError::InvalidChannelDisplayNameByte {
                    offset: 0,
                    value: 0x1F
                }
            ),
            "wrong validation error: {error:?}"
        );
        Ok(())
    }

    #[test]
    fn display_name_rejects_data_after_first_nul() -> TestResult {
        let mut bytes = [0; ChannelDisplayName::WIRE_LEN];
        let unexpected = bytes
            .get_mut(7)
            .ok_or("16-byte field should contain offset 7")?;
        *unexpected = b'X';

        let error = ChannelDisplayName::try_from_wire(bytes)
            .err()
            .ok_or("data after NUL should have been rejected")?;
        assert!(
            matches!(
                error,
                ValidationError::ChannelDisplayNameDataAfterNul {
                    offset: 7,
                    value: b'X'
                }
            ),
            "wrong validation error: {error:?}"
        );
        Ok(())
    }
}
