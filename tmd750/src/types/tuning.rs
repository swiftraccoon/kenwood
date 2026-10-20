//! Tuning mode, repeater shift, memory addresses and band roles.
//!
//! Wire domains were established on firmware 1.02 by writing every value and
//! reading it back; a value is named only where the radio's own replies or the
//! User Manual identify it.

use std::fmt;

use crate::error::{SettingName, ValidationError};
use crate::types::Band;

/// A regular memory-channel number from 0 through 999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegularChannel(u16);

integer_formats!(RegularChannel, get);

impl RegularChannel {
    /// Highest regular memory-channel number.
    pub const MAX: u16 = 999;

    /// Check a regular memory-channel number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u16) -> Result<Self, ValidationError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(ValidationError::SettingOutOfRange {
                setting: SettingName::MemoryChannel,
                value: value as u64,
                max: Self::MAX as u64,
            })
        }
    }

    /// The zero-based channel number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for RegularChannel {
    type Error = ValidationError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RegularChannel> for u16 {
    fn from(value: RegularChannel) -> Self {
        value.get()
    }
}

impl fmt::Display for RegularChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A program-scan lower/upper pair index from 0 through 49.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgramScanIndex(u8);

integer_formats!(ProgramScanIndex, get);

impl ProgramScanIndex {
    /// Highest program-scan pair index.
    pub const MAX: u8 = 49;

    /// Check a program-scan pair index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(ValidationError::SettingOutOfRange {
                setting: SettingName::ProgramScanIndex,
                value: value as u64,
                max: Self::MAX as u64,
            })
        }
    }

    /// The zero-based program-scan pair index.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ProgramScanIndex {
    type Error = ValidationError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProgramScanIndex> for u8 {
    fn from(value: ProgramScanIndex) -> Self {
        value.get()
    }
}

impl fmt::Display for ProgramScanIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Tuning mode selected and reported by `VM`.
///
/// `VM band,3` selects the D-STAR repeater list: the band then reports the
/// listed repeater's frequency and offset and `MD` reports DR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TuningMode {
    /// VFO (`0`).
    Vfo = 0,
    /// Memory channel (`1`); `MR` reports the selected channel.
    Memory = 1,
    /// CALL channel (`2`).
    Call = 2,
    /// DR, the D-STAR repeater list (`3`).
    DStarRepeater = 3,
}

impl TuningMode {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for TuningMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Vfo),
            1 => Ok(Self::Memory),
            2 => Ok(Self::Call),
            3 => Ok(Self::DStarRepeater),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: SettingName::TuningMode,
                value: u64::from(value),
                max: 3,
            }),
        }
    }
}

impl fmt::Display for TuningMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Vfo => "VFO",
            Self::Memory => "Memory",
            Self::Call => "CALL",
            Self::DStarRepeater => "DR",
        })
    }
}

/// Direction of the repeater offset in a channel record.
///
/// `0` was read on simplex settings and `2` on the 145.190 MHz and
/// 448.375 MHz repeater settings, both in ranges the User Manual's Auto
/// Repeater Offset table assigns a negative offset; `1` is the remaining
/// direction and has not been read from the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShiftDirection {
    /// Transmit on the receive frequency (`0`).
    Simplex = 0,
    /// Transmit above the receive frequency (`1`).
    Plus = 1,
    /// Transmit below the receive frequency (`2`).
    Minus = 2,
}

impl ShiftDirection {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for ShiftDirection {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Simplex),
            1 => Ok(Self::Plus),
            2 => Ok(Self::Minus),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: SettingName::ShiftDirection,
                value: u64::from(value),
                max: 2,
            }),
        }
    }
}

impl fmt::Display for ShiftDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Simplex => "simplex",
            Self::Plus => "+",
            Self::Minus => "-",
        })
    }
}

/// A memory channel address accepted by `ME` and `MR`.
///
/// The wire form is exactly three characters: `000`-`999` for the 1,000
/// regular channels, `L00`-`L49` and `U00`-`U49` for the program scan limits,
/// and `Pri` for the priority channel (User Manual, Memory Channels). An empty
/// channel is answered `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryChannelAddress {
    /// Regular channel 0 through 999.
    Regular(RegularChannel),
    /// Program scan lower limit L0 through L49.
    ProgramScanLower(ProgramScanIndex),
    /// Program scan upper limit U0 through U49.
    ProgramScanUpper(ProgramScanIndex),
    /// The priority scan channel.
    Priority,
}

impl MemoryChannelAddress {
    /// Highest regular channel number.
    pub const MAX_REGULAR: u16 = 999;
    /// Highest program scan pair index.
    pub const MAX_PROGRAM_SCAN: u8 = 49;

    /// A regular channel.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above
    /// [`Self::MAX_REGULAR`].
    pub fn regular(channel: u16) -> Result<Self, ValidationError> {
        match RegularChannel::new(channel) {
            Ok(channel) => Ok(Self::Regular(channel)),
            Err(error) => Err(error),
        }
    }

    /// A program scan lower limit.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above
    /// [`Self::MAX_PROGRAM_SCAN`].
    pub fn program_scan_lower(index: u8) -> Result<Self, ValidationError> {
        match ProgramScanIndex::new(index) {
            Ok(index) => Ok(Self::ProgramScanLower(index)),
            Err(error) => Err(error),
        }
    }

    /// A program scan upper limit.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above
    /// [`Self::MAX_PROGRAM_SCAN`].
    pub fn program_scan_upper(index: u8) -> Result<Self, ValidationError> {
        match ProgramScanIndex::new(index) {
            Ok(index) => Ok(Self::ProgramScanUpper(index)),
            Err(error) => Err(error),
        }
    }

    /// The three-character wire form.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        match self {
            Self::Regular(channel) => format!("{channel:03}"),
            Self::ProgramScanLower(index) => format!("L{index:02}"),
            Self::ProgramScanUpper(index) => format!("U{index:02}"),
            Self::Priority => "Pri".to_owned(),
        }
    }

    /// Parse the three-character wire form.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidMemoryAddress`] for any other text.
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        let invalid = || ValidationError::InvalidMemoryAddress {
            text: text.to_owned(),
        };
        if text == "Pri" {
            return Ok(Self::Priority);
        }
        let bytes = text.as_bytes();
        let [first, second, third] = bytes else {
            return Err(invalid());
        };
        let digits = |high: u8, low: u8| -> Option<u8> {
            (high.is_ascii_digit() && low.is_ascii_digit())
                .then(|| (high - b'0') * 10 + (low - b'0'))
        };
        match first {
            b'L' => digits(*second, *third)
                .ok_or_else(invalid)
                .and_then(|index| Self::program_scan_lower(index).map_err(|_| invalid())),
            b'U' => digits(*second, *third)
                .ok_or_else(invalid)
                .and_then(|index| Self::program_scan_upper(index).map_err(|_| invalid())),
            _ if first.is_ascii_digit() => digits(*second, *third)
                .map(|low| u16::from(*first - b'0') * 100 + u16::from(low))
                .ok_or_else(invalid)
                .and_then(|channel| Self::regular(channel).map_err(|_| invalid())),
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for MemoryChannelAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_wire_string())
    }
}

/// The channel `MR band` reports in Memory mode.
///
/// The APRS frequency channel is reported as `AP ` (with a trailing space) and
/// is neither readable with `ME` nor recallable with `MR`; both answer `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrentMemorySelector {
    /// An address that `ME` and `MR` also accept.
    Address(MemoryChannelAddress),
    /// The APRS frequency channel.
    Aprs,
}

impl CurrentMemorySelector {
    /// Wire form of the APRS channel, including its trailing space.
    pub const APRS_WIRE: &str = "AP ";

    /// Parse the three-character wire form.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidMemoryAddress`] for any other text.
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        if text == Self::APRS_WIRE {
            return Ok(Self::Aprs);
        }
        MemoryChannelAddress::from_wire_str(text).map(Self::Address)
    }

    /// The address when the channel can be read or recalled.
    #[must_use]
    pub const fn address(self) -> Option<MemoryChannelAddress> {
        match self {
            Self::Address(address) => Some(address),
            Self::Aprs => None,
        }
    }
}

impl fmt::Display for CurrentMemorySelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(address) => address.fmt(formatter),
            Self::Aprs => formatter.write_str("APRS"),
        }
    }
}

/// Band roles reported and selected by `BC`.
///
/// `UP` and `DW` act on the control band, which identifies the first field;
/// the second field is the PTT band, the only other band role on the panel,
/// and was not exercised by transmitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BandControl {
    /// Band that panel tuning and `UP`/`DW` act on.
    pub control: Band,
    /// Band the PTT keys.
    pub ptt: Band,
}

impl fmt::Display for BandControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "control {}, PTT {}", self.control, self.ptt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn tuning_and_shift_values_round_trip() -> TestResult {
        for raw in 0..=3 {
            assert_eq!(TuningMode::try_from(raw)?.as_raw(), raw);
        }
        assert!(TuningMode::try_from(4).is_err());
        assert_eq!(TuningMode::DStarRepeater.to_string(), "DR");
        for raw in 0..=2 {
            assert_eq!(ShiftDirection::try_from(raw)?.as_raw(), raw);
        }
        assert!(ShiftDirection::try_from(3).is_err());
        Ok(())
    }

    #[test]
    fn memory_addresses_have_exactly_three_wire_characters() -> TestResult {
        for (text, address) in [
            ("000", MemoryChannelAddress::regular(0)?),
            ("999", MemoryChannelAddress::regular(999)?),
            ("L00", MemoryChannelAddress::program_scan_lower(0)?),
            ("U49", MemoryChannelAddress::program_scan_upper(49)?),
            ("Pri", MemoryChannelAddress::Priority),
        ] {
            assert_eq!(MemoryChannelAddress::from_wire_str(text)?, address);
            assert_eq!(address.to_wire_string(), text);
        }
        for text in [
            "0", "1", "0001", "1000", "L0", "L50", "U50", "PRI", "AP ", "", "0A0",
        ] {
            let result = MemoryChannelAddress::from_wire_str(text);
            assert!(
                matches!(result, Err(ValidationError::InvalidMemoryAddress { .. })),
                "{text:?}: {result:?}"
            );
        }
        assert!(MemoryChannelAddress::regular(1000).is_err());
        assert!(MemoryChannelAddress::program_scan_lower(50).is_err());
        assert!(MemoryChannelAddress::program_scan_upper(50).is_err());
        assert!(RegularChannel::new(1000).is_err());
        assert!(ProgramScanIndex::new(50).is_err());
        Ok(())
    }

    #[test]
    fn checked_address_payloads_preserve_every_physical_channel_boundary() -> TestResult {
        use crate::types::PhysicalChannel;
        for raw in 0..=RegularChannel::MAX {
            let address = MemoryChannelAddress::Regular(RegularChannel::try_from(raw)?);
            assert_eq!(
                PhysicalChannel::from_address(address).selector().address(),
                Some(address)
            );
        }
        for raw in 0..=ProgramScanIndex::MAX {
            for address in [
                MemoryChannelAddress::ProgramScanLower(ProgramScanIndex::try_from(raw)?),
                MemoryChannelAddress::ProgramScanUpper(ProgramScanIndex::try_from(raw)?),
            ] {
                assert_eq!(
                    PhysicalChannel::from_address(address).selector().address(),
                    Some(address)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn current_selector_names_the_aprs_channel() -> TestResult {
        assert_eq!(
            CurrentMemorySelector::from_wire_str("AP ")?,
            CurrentMemorySelector::Aprs
        );
        assert_eq!(CurrentMemorySelector::Aprs.address(), None);
        assert_eq!(
            CurrentMemorySelector::from_wire_str("021")?.address(),
            Some(MemoryChannelAddress::regular(21)?)
        );
        assert!(CurrentMemorySelector::from_wire_str("AP").is_err());
        assert_eq!(CurrentMemorySelector::Aprs.to_string(), "APRS");
        Ok(())
    }

    #[test]
    fn band_control_displays_both_roles() {
        let roles = BandControl {
            control: Band::B,
            ptt: Band::A,
        };
        assert_eq!(roles.to_string(), "control B, PTT A");
    }
}
