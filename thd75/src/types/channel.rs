//! Channel URCALL and memory types for the TH-D75 transceiver.
//!
//! This module contains three exact channel representations:
//!
//! - [`CatChannelRecord`]: the 20 shared textual fields returned by `FO` and
//!   `ME`.
//! - [`CatMemoryChannelRecord`]: an `ME` record plus its split and scan-lockout
//!   fields.
//! - [`StoredChannel`]: the 40-byte stored memory format (MCP/SD card), used
//!   by the memory and SD card modules for binary image parsing.
//!
//! CAT records are comma-separated text and never have a binary form. Stored
//! records use semantic types for established fields and exact typed storage
//! for bits whose meanings have not yet been established.
//!
//! # Memory architecture (per User Manual Chapter 8)
//!
//! A total of 1101 memory channels are available:
//!
//! - **0-999**: standard memory channels (simplex, repeater, or odd-split)
//! - **L0/U0 through L49/U49**: 100 program scan edge memories (50 pairs)
//! - **Pri**: 1 priority scan memory channel
//! - **A1-A10**: weather channels (TH-D75A only)
//! - **C**: call channels (one per band/mode combination)
//!
//! Each channel can store: RX frequency, TX frequency (odd-split),
//! step size, offset direction, tone/CTCSS/DCS/cross-tone settings,
//! shift, reverse, lockout, demodulation mode, fine mode, memory name
//! (up to 16 characters), and D-STAR digital squelch/callsign data.
//!
//! Memory channels can be used as simplex/repeater (one frequency +
//! optional offset) or odd-split (separate TX/RX frequencies for
//! non-standard repeater offsets). Odd-split channels show "+-" on
//! the display.
//!
//! # Memory groups (per Operating Tips §5.11, User Manual Chapter 8)
//!
//! The TH-D75 supports 30 memory groups (GRP-0 through GRP-29). By
//! default, channels 0-99 belong to GRP-0, 100-199 to GRP-1, and so on
//! up to 900-999 in GRP-9. Groups 10-29 are empty by default. Each group
//! can be given a name of up to 16 characters via Menu No. 201.
//! Groups without any registered channels are skipped during group
//! switching.
//!
//! # Memory recall (per User Manual Chapter 8)
//!
//! Menu No. 202 controls the recall method:
//! - `All Bands`: recall all programmed memory channels.
//! - `Current Band`: recall only channels with frequencies in the
//!   current frequency band. This also affects memory scan and group
//!   link scan.
//!
//! # Memory shift (per User Manual Chapter 8)
//!
//! Memory channel or call channel contents can be copied to VFO via
//! `[F]`, `[VFO]`. The entire contents (frequency, mode, tone, etc.)
//! are transferred. To copy the TX frequency from an odd-split channel,
//! turn on Reverse first.

use std::fmt;

use crate::error::{ProtocolError, ValidationError};
use crate::types::RegularChannel;
use crate::types::dstar::{DigitalSquelchCode, DigitalSquelchType, DstarCallsign};
use crate::types::frequency::Frequency;
use crate::types::mode::{ChannelMode, ShiftDirection, StepSize};
use crate::types::tone::{CtcssCode, DcsCode, ToneCode, ToneMode};

/// A memory group in the inclusive range 0-29.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryGroup(u8);

impl MemoryGroup {
    /// Lowest memory-group index.
    pub const MIN: u8 = 0;

    /// Highest memory-group index.
    pub const MAX: u8 = 29;

    /// Number of memory groups.
    pub const COUNT: usize = 30;

    /// Construct a memory-group index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::MemoryGroupOutOfRange`] above group 29.
    pub const fn new(group: u8) -> Result<Self, ValidationError> {
        if group <= Self::MAX {
            Ok(Self(group))
        } else {
            Err(ValidationError::MemoryGroupOutOfRange { group })
        }
    }

    /// Return the zero-based group index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// Iterate over every memory group in numeric order.
    pub fn all() -> impl ExactSizeIterator<Item = Self> + DoubleEndedIterator {
        (Self::MIN..=Self::MAX).map(Self)
    }
}

impl TryFrom<u8> for MemoryGroup {
    type Error = ValidationError;

    fn try_from(group: u8) -> Result<Self, Self::Error> {
        Self::new(group)
    }
}

impl From<MemoryGroup> for u8 {
    fn from(group: MemoryGroup) -> Self {
        group.as_raw()
    }
}

impl From<MemoryGroup> for usize {
    fn from(group: MemoryGroup) -> Self {
        Self::from(group.as_raw())
    }
}

impl fmt::Display for MemoryGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// Band marker stored in a populated memory channel's flag record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryChannelBand {
    /// VHF channel band code (`0x00`).
    Vhf,
    /// 220 MHz channel band code (`0x01`).
    Band220,
    /// UHF channel band code (`0x02`).
    Uhf,
    /// 50 MHz channel band code (`0x05`).
    Band50MHz,
}

impl MemoryChannelBand {
    /// Decode the three-bit band code stored at the bottom of flag byte zero.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::MemoryChannelBandOutOfRange`] for a code
    /// that has not been verified in a physical TH-D75 memory image.
    pub const fn from_wire_code(marker: u8) -> Result<Self, ValidationError> {
        match marker {
            0x00 => Ok(Self::Vhf),
            0x01 => Ok(Self::Band220),
            0x02 => Ok(Self::Uhf),
            0x05 => Ok(Self::Band50MHz),
            _ => Err(ValidationError::MemoryChannelBandOutOfRange { marker }),
        }
    }

    /// Return the three-bit code used by the radio's memory-channel flag.
    #[must_use]
    pub const fn to_wire_code(self) -> u8 {
        match self {
            Self::Vhf => 0x00,
            Self::Band220 => 0x01,
            Self::Uhf => 0x02,
            Self::Band50MHz => 0x05,
        }
    }
}

impl TryFrom<u8> for MemoryChannelBand {
    type Error = ValidationError;

    fn try_from(marker: u8) -> Result<Self, Self::Error> {
        Self::from_wire_code(marker)
    }
}

impl From<MemoryChannelBand> for u8 {
    fn from(band: MemoryChannelBand) -> Self {
        band.to_wire_code()
    }
}

/// Lossless four-byte flag record for a regular memory channel.
///
/// Physical radio images prove that byte zero is not a standalone band
/// value: channel 1 in the validation fixture stores `0x08` while remaining
/// a VHF memory. Only its low three bits are decoded as the band code. Every
/// other bit is retained verbatim so reading and writing a channel cannot
/// erase fields whose meaning has not yet been established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredChannelFlag {
    wire: [u8; Self::WIRE_SIZE],
    state: StoredChannelFlagState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StoredChannelFlagState {
    Empty,
    Programmed {
        band: MemoryChannelBand,
        group: MemoryGroup,
        scan_lockout: bool,
    },
}

impl StoredChannelFlag {
    /// Width of a memory-channel flag record.
    pub const WIRE_SIZE: usize = 4;

    const BAND_CODE_MASK: u8 = 0x07;
    const SCAN_LOCKOUT_MASK: u8 = 0x01;
    const EMPTY_MARKER: u8 = 0xFF;

    /// Construct the canonical empty-channel flag with group byte zero.
    ///
    /// Use [`Self::empty_for_regular_channel`] when clearing a regular slot;
    /// physical radio images retain the channel's default group byte even
    /// while byte zero marks the slot empty.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            wire: [Self::EMPTY_MARKER, 0x00, 0x00, 0xFF],
            state: StoredChannelFlagState::Empty,
        }
    }

    /// Construct the observed empty flag for a regular channel.
    #[must_use]
    pub const fn empty_for_regular_channel(channel: RegularChannel) -> Self {
        let [default_group, _] = (channel.as_raw() / 100).to_le_bytes();
        Self {
            wire: [Self::EMPTY_MARKER, 0x00, default_group, 0xFF],
            state: StoredChannelFlagState::Empty,
        }
    }

    /// Construct a canonical programmed-channel flag.
    #[must_use]
    pub const fn programmed(
        band: MemoryChannelBand,
        group: MemoryGroup,
        scan_lockout: bool,
    ) -> Self {
        Self {
            wire: [
                band.to_wire_code(),
                if scan_lockout {
                    Self::SCAN_LOCKOUT_MASK
                } else {
                    0x00
                },
                group.as_raw(),
                0xFF,
            ],
            state: StoredChannelFlagState::Programmed {
                band,
                group,
                scan_lockout,
            },
        }
    }

    /// Decode and retain an exact flag record from a radio image.
    ///
    /// Empty records are retained without interpreting their other bytes.
    /// Programmed records validate the proven band code and memory-group
    /// fields while keeping all unknown bits intact.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::MemoryChannelBandOutOfRange`] for an
    /// unverified programmed-channel band code, or
    /// [`ValidationError::MemoryGroupOutOfRange`] for a programmed channel
    /// whose group is outside 0-29.
    pub fn try_from_wire(wire: [u8; Self::WIRE_SIZE]) -> Result<Self, ValidationError> {
        let state = if wire[0] == Self::EMPTY_MARKER {
            StoredChannelFlagState::Empty
        } else {
            StoredChannelFlagState::Programmed {
                band: MemoryChannelBand::from_wire_code(wire[0] & Self::BAND_CODE_MASK)?,
                group: MemoryGroup::new(wire[2])?,
                scan_lockout: wire[1] & Self::SCAN_LOCKOUT_MASK != 0,
            }
        };
        Ok(Self { wire, state })
    }

    /// Return whether the memory slot is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        matches!(self.state, StoredChannelFlagState::Empty)
    }

    /// Return whether the memory slot is programmed.
    #[must_use]
    pub const fn is_programmed(self) -> bool {
        !self.is_empty()
    }

    /// Return the stored band for a programmed slot.
    #[must_use]
    pub const fn band(self) -> Option<MemoryChannelBand> {
        match self.state {
            StoredChannelFlagState::Empty => None,
            StoredChannelFlagState::Programmed { band, .. } => Some(band),
        }
    }

    /// Return the group assigned to a programmed slot.
    #[must_use]
    pub const fn group(self) -> Option<MemoryGroup> {
        match self.state {
            StoredChannelFlagState::Empty => None,
            StoredChannelFlagState::Programmed { group, .. } => Some(group),
        }
    }

    /// Return whether a programmed slot is locked out of normal scans.
    #[must_use]
    pub const fn scan_lockout(self) -> Option<bool> {
        match self.state {
            StoredChannelFlagState::Empty => None,
            StoredChannelFlagState::Programmed { scan_lockout, .. } => Some(scan_lockout),
        }
    }

    /// Return the exact bytes read from, or to be written to, the radio.
    #[must_use]
    pub const fn to_wire_bytes(self) -> [u8; Self::WIRE_SIZE] {
        self.wire
    }
}

impl TryFrom<&[u8]> for StoredChannelFlag {
    type Error = ValidationError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let wire: [u8; Self::WIRE_SIZE] =
            bytes
                .try_into()
                .map_err(|_| ValidationError::StoredChannelFlagLength {
                    actual: bytes.len(),
                })?;
        Self::try_from_wire(wire)
    }
}

/// Exact three-character address accepted by ME reads and MR recalls.
///
/// A memory address is not always a decimal channel number. Firmware accepts
/// program-scan edges (`L00`-`L49` and `U00`-`U49`) and regional banks
/// (`T01`-`T30` and `A01`-`A10`) in addition to ordinary channels `000`-`999`.
/// The `Pri` label is deliberately absent: firmware emits it from an MR read,
/// but its shared input parser does not accept it as an ME or MR address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MemoryChannelAddressValue {
    Channel(RegularChannel),
    ProgramLower(u8),
    ProgramUpper(u8),
    RegionalT(u8),
    RegionalA(u8),
}

/// Validated address of a readable or recallable memory channel.
///
/// The representation is private so out-of-range forms such as `L50` or
/// `T00`, and the output-only `Pri` label cannot reach the infallible CAT
/// serializer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemoryChannelAddress(MemoryChannelAddressValue);

impl MemoryChannelAddress {
    /// Construct an ordinary memory-channel address.
    #[must_use]
    pub const fn regular(channel: RegularChannel) -> Self {
        Self(MemoryChannelAddressValue::Channel(channel))
    }

    /// Construct a lower program-scan edge (`L00`-`L49`).
    ///
    /// # Errors
    ///
    /// Returns a validation error above index 49.
    pub const fn program_lower(index: u8) -> Result<Self, ValidationError> {
        if index <= 49 {
            Ok(Self(MemoryChannelAddressValue::ProgramLower(index)))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "lower program-scan memory",
                value: index,
                detail: "must be L00-L49",
            })
        }
    }

    /// Construct an upper program-scan edge (`U00`-`U49`).
    ///
    /// # Errors
    ///
    /// Returns a validation error above index 49.
    pub const fn program_upper(index: u8) -> Result<Self, ValidationError> {
        if index <= 49 {
            Ok(Self(MemoryChannelAddressValue::ProgramUpper(index)))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "upper program-scan memory",
                value: index,
                detail: "must be U00-U49",
            })
        }
    }

    /// Construct a regional `T` selector (`T01`-`T30`).
    ///
    /// # Errors
    ///
    /// Returns a validation error outside 1-30.
    pub const fn regional_t(index: u8) -> Result<Self, ValidationError> {
        if index >= 1 && index <= 30 {
            Ok(Self(MemoryChannelAddressValue::RegionalT(index)))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "regional T memory",
                value: index,
                detail: "must be T01-T30",
            })
        }
    }

    /// Construct a regional `A` selector (`A01`-`A10`).
    ///
    /// # Errors
    ///
    /// Returns a validation error outside 1-10.
    pub const fn regional_a(index: u8) -> Result<Self, ValidationError> {
        if index >= 1 && index <= 10 {
            Ok(Self(MemoryChannelAddressValue::RegionalA(index)))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "regional A memory",
                value: index,
                detail: "must be A01-A10",
            })
        }
    }

    /// Return the ordinary numeric channel, if this is `000`-`999`.
    #[must_use]
    pub const fn regular_channel(self) -> Option<RegularChannel> {
        match self {
            Self(MemoryChannelAddressValue::Channel(channel)) => Some(channel),
            _ => None,
        }
    }

    fn invalid(selector: &str) -> ValidationError {
        ValidationError::InvalidMemorySelector {
            selector: selector.to_owned(),
            detail: "expected 000-999, L00-L49, U00-U49, T01-T30, or A01-A10",
        }
    }
}

impl TryFrom<u16> for MemoryChannelAddress {
    type Error = ValidationError;

    fn try_from(channel: u16) -> Result<Self, Self::Error> {
        RegularChannel::new(channel).map(Self::regular)
    }
}

impl From<RegularChannel> for MemoryChannelAddress {
    fn from(channel: RegularChannel) -> Self {
        Self::regular(channel)
    }
}

impl TryFrom<&str> for MemoryChannelAddress {
    type Error = ValidationError;

    fn try_from(selector: &str) -> Result<Self, Self::Error> {
        if selector.len() != 3 || !selector.is_ascii() {
            return Err(Self::invalid(selector));
        }

        let bytes = selector.as_bytes();
        if bytes.iter().all(u8::is_ascii_digit) {
            let channel = selector
                .parse::<u16>()
                .map_err(|_| Self::invalid(selector))?;
            return RegularChannel::new(channel).map(Self::regular);
        }
        let Some((&prefix, digits)) = bytes.split_first() else {
            return Err(Self::invalid(selector));
        };
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(Self::invalid(selector));
        }
        let number = std::str::from_utf8(digits)
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| Self::invalid(selector))?;

        let index = u8::try_from(number).map_err(|_| Self::invalid(selector))?;
        match prefix {
            b'L' if index <= 49 => Self::program_lower(index),
            b'U' if index <= 49 => Self::program_upper(index),
            b'T' if (1..=30).contains(&index) => Self::regional_t(index),
            b'A' if (1..=10).contains(&index) => Self::regional_a(index),
            _ => Err(Self::invalid(selector)),
        }
        .map_err(|_| Self::invalid(selector))
    }
}

impl fmt::Display for MemoryChannelAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            MemoryChannelAddressValue::Channel(channel) => {
                write!(f, "{:03}", channel.as_raw())
            }
            MemoryChannelAddressValue::ProgramLower(index) => write!(f, "L{index:02}"),
            MemoryChannelAddressValue::ProgramUpper(index) => write!(f, "U{index:02}"),
            MemoryChannelAddressValue::RegionalT(index) => write!(f, "T{index:02}"),
            MemoryChannelAddressValue::RegionalA(index) => write!(f, "A{index:02}"),
        }
    }
}

/// Selector reported by an MR current-channel read.
///
/// The radio can report its priority channel as `Pri` even though `Pri` is not
/// accepted by the memory-address input parser. Keeping that output-only state
/// separate prevents it from being fed back into an ME read or MR recall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrentMemorySelector {
    /// An address that can also be read or recalled.
    Address(MemoryChannelAddress),
    /// The output-only priority-channel label (`Pri`).
    Priority,
}

impl CurrentMemorySelector {
    /// Return an address suitable for ME reads and MR recalls, if available.
    #[must_use]
    pub const fn address(self) -> Option<MemoryChannelAddress> {
        match self {
            Self::Address(address) => Some(address),
            Self::Priority => None,
        }
    }

    /// Return the ordinary numeric channel, if this is `000`-`999`.
    #[must_use]
    pub const fn regular_channel(self) -> Option<RegularChannel> {
        match self.address() {
            Some(address) => address.regular_channel(),
            None => None,
        }
    }

    fn invalid(selector: &str) -> ValidationError {
        ValidationError::InvalidMemorySelector {
            selector: selector.to_owned(),
            detail: "expected 000-999, L00-L49, U00-U49, T01-T30, A01-A10, or Pri",
        }
    }
}

impl From<MemoryChannelAddress> for CurrentMemorySelector {
    fn from(address: MemoryChannelAddress) -> Self {
        Self::Address(address)
    }
}

impl From<RegularChannel> for CurrentMemorySelector {
    fn from(channel: RegularChannel) -> Self {
        Self::Address(channel.into())
    }
}

impl TryFrom<&str> for CurrentMemorySelector {
    type Error = ValidationError;

    fn try_from(selector: &str) -> Result<Self, Self::Error> {
        if selector == "Pri" {
            Ok(Self::Priority)
        } else {
            MemoryChannelAddress::try_from(selector)
                .map(Self::Address)
                .map_err(|_| Self::invalid(selector))
        }
    }
}

impl fmt::Display for CurrentMemorySelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(address) => address.fmt(formatter),
            Self::Priority => formatter.write_str("Pri"),
        }
    }
}

/// Exact unidentified high bits carried alongside channel code indices.
///
/// Firmware copies and formats the complete CTCSS, DCS, and digital-squelch
/// code bytes. Their established indices occupy `0x0C[5:0]`, `0x0D[6:0]`,
/// and `0x27[6:0]`; retained evidence does not establish meanings for the
/// remaining high bits. This type preserves those bits without inventing
/// semantic labels or weakening validation of the known indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelCodeUnidentifiedBits {
    ctcss_code_bits_7_to_6: u8,
    dcs_code_bit_7: bool,
    digital_squelch_code_bit_7: bool,
}

impl ChannelCodeUnidentifiedBits {
    /// Construct the exact unidentified code bits.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if the CTCSS component
    /// does not fit its two-bit field.
    pub const fn new(
        ctcss_code_bits_7_to_6: u8,
        dcs_code_bit_7: bool,
        digital_squelch_code_bit_7: bool,
    ) -> Result<Self, ValidationError> {
        if ctcss_code_bits_7_to_6 <= 3 {
            Ok(Self {
                ctcss_code_bits_7_to_6,
                dcs_code_bit_7,
                digital_squelch_code_bit_7,
            })
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "channel CTCSS-code unidentified bits 7:6",
                value: ctcss_code_bits_7_to_6,
                detail: "must fit two bits (0-3)",
            })
        }
    }

    /// Return the exact value from CTCSS-code bits 7:6.
    #[must_use]
    pub const fn ctcss_code_bits_7_to_6(self) -> u8 {
        self.ctcss_code_bits_7_to_6
    }

    /// Return the exact value from DCS-code bit 7.
    #[must_use]
    pub const fn dcs_code_bit_7(self) -> bool {
        self.dcs_code_bit_7
    }

    /// Return the exact value from digital-squelch-code bit 7.
    #[must_use]
    pub const fn digital_squelch_code_bit_7(self) -> bool {
        self.digital_squelch_code_bit_7
    }

    const fn ctcss_wire_value(self, code: CtcssCode) -> u8 {
        (self.ctcss_code_bits_7_to_6 << 6) | code.as_raw()
    }

    const fn dcs_wire_value(self, code: DcsCode) -> u8 {
        let high_bit = if self.dcs_code_bit_7 { 0x80 } else { 0 };
        high_bit | code.as_raw()
    }

    const fn digital_squelch_wire_value(self, code: DigitalSquelchCode) -> u8 {
        let high_bit = if self.digital_squelch_code_bit_7 {
            0x80
        } else {
            0
        };
        high_bit | code.as_raw()
    }
}

/// Meaning of a channel record's context-dependent transmit value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelTransmitValue {
    /// A repeater offset from the receive frequency.
    RepeaterOffset(Frequency),
    /// An independent absolute transmit frequency for an odd split.
    SplitTransmitFrequency(Frequency),
}

impl fmt::Display for ChannelTransmitValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RepeaterOffset(offset) => write!(formatter, "repeater offset {offset}"),
            Self::SplitTransmitFrequency(frequency) => {
                write!(formatter, "split transmit frequency {frequency}")
            }
        }
    }
}

/// Complete typed representation of the 20 shared FO/ME CAT fields.
///
/// CAT records are textual protocol records, not the radio's 40-byte MCP
/// memory layout. Keeping this type independent from [`StoredChannel`] avoids
/// fabricating bytes that CAT never returns and makes every transmitted field
/// visible by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatChannelRecord {
    /// Receive frequency in hertz.
    pub receive_frequency: Frequency,
    /// Repeater offset or absolute transmit frequency, depending on context.
    pub transmit_offset_or_frequency: Frequency,
    /// Receive tuning step.
    pub receive_step: StepSize,
    /// Transmit tuning step.
    pub transmit_step: StepSize,
    /// Operating mode encoded in the CAT channel record.
    pub mode: ChannelMode,
    /// Whether fine tuning is enabled.
    pub fine_tuning: bool,
    /// Fine-tuning step.
    pub fine_step: FineStep,
    /// The single active tone-signaling mode.
    pub tone_mode: ToneMode,
    /// Whether repeater reverse is enabled.
    pub reverse: bool,
    /// Repeater shift direction. Split is reported separately by ME.
    pub shift: ShiftDirection,
    /// Transmit tone or tone-burst index.
    pub tone_code: ToneCode,
    /// Receive CTCSS decoder index; unidentified high bits are stored below.
    pub ctcss_code: CtcssCode,
    /// DCS code index; its unidentified high bit is stored below.
    pub dcs_code: DcsCode,
    /// Exact cross-tone field, including its two unidentified high bits.
    pub cross_tone: CrossToneField,
    /// D-STAR destination callsign.
    pub ur_call: DstarCallsign,
    /// Per-channel D-STAR squelch selection.
    pub digital_squelch: DigitalSquelchType,
    /// Per-channel D-STAR squelch code; its unidentified high bit is stored below.
    pub digital_squelch_code: DigitalSquelchCode,
    /// Exact unidentified high bits carried by the three code fields.
    pub unidentified_code_bits: ChannelCodeUnidentifiedBits,
}

/// Complete ME memory-channel response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatMemoryChannelRecord {
    /// Shared FO/ME channel fields.
    pub channel: CatChannelRecord,
    /// Whether the stored transmit frequency is an independent split.
    pub split: bool,
    /// Whether scanning skips this memory channel.
    pub scan_lockout: bool,
}

impl CatMemoryChannelRecord {
    /// Interpret the shared transmit field using this ME record's split flag.
    #[must_use]
    pub const fn transmit_value(&self) -> ChannelTransmitValue {
        if self.split {
            ChannelTransmitValue::SplitTransmitFrequency(self.channel.transmit_offset_or_frequency)
        } else {
            ChannelTransmitValue::RepeaterOffset(self.channel.transmit_offset_or_frequency)
        }
    }
}

// ===========================================================================
// Stored channel types (MCP / SD card binary format)
// ===========================================================================

/// Cross-tone type as stored in MCP/SD-card byte 0x0E bits \[5:4\].
///
/// Determines how different tone/DCS codes are applied to TX vs RX
/// when cross-tone mode is active.
///
/// Per User Manual Chapter 10: cross tone allows separate signaling
/// types for TX and RX when accessing a repeater that uses different
/// encode/decode signaling. Activated by pressing `[TONE]` 4 times.
///
/// | Value | Encode (TX) | Decode (RX) | Display icon |
/// |-------|-------------|-------------|--------------|
/// | 0 | DCS | Off | D/O |
/// | 1 | Tone | DCS | T/D |
/// | 2 | DCS | CTCSS | D/C |
/// | 3 | Tone | CTCSS | T/C |
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrossToneType {
    /// DCS encode (TX), Off decode (RX). Display: D/O (value 0).
    DcsOff = 0,
    /// Tone encode (TX), DCS decode (RX). Display: T/D (value 1).
    ToneDcs = 1,
    /// DCS encode (TX), CTCSS decode (RX). Display: D/C (value 2).
    DcsCtcss = 2,
    /// Tone encode (TX), CTCSS decode (RX). Display: T/C (value 3).
    ToneCtcss = 3,
}

impl CrossToneType {
    /// Number of valid cross-tone type values (0-3).
    pub const COUNT: u8 = 4;
}

impl TryFrom<u8> for CrossToneType {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::DcsOff),
            1 => Ok(Self::ToneDcs),
            2 => Ok(Self::DcsCtcss),
            3 => Ok(Self::ToneCtcss),
            _ => Err(ValidationError::CrossToneTypeOutOfRange(value)),
        }
    }
}

impl From<CrossToneType> for u8 {
    fn from(ct: CrossToneType) -> Self {
        ct as Self
    }
}

/// Exact four-bit cross-tone field used by FO/ME and stored byte `0x0E`.
///
/// The low two bits select the documented [`CrossToneType`]. Firmware emits
/// the complete hexadecimal nibble, but the meaning of its upper two bits has
/// not been established. This type preserves those bits instead of silently
/// masking them or assigning an unsupported meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CrossToneField(u8);

impl CrossToneField {
    /// Largest value representable by the four-bit wire field.
    pub const MAX: u8 = 0x0F;

    /// Construct an exact cross-tone wire field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above `0x0F`.
    pub const fn new(raw: u8) -> Result<Self, ValidationError> {
        if raw <= Self::MAX {
            Ok(Self(raw))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "cross-tone field",
                value: raw,
                detail: "must fit one hexadecimal digit (0x0-0xF)",
            })
        }
    }

    /// Return the complete four-bit wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// Return the documented cross-tone selection from the low two bits.
    #[must_use]
    pub const fn tone_type(self) -> CrossToneType {
        let low_bits = self.0 & 0x03;
        if low_bits == 0 {
            CrossToneType::DcsOff
        } else if low_bits == 1 {
            CrossToneType::ToneDcs
        } else if low_bits == 2 {
            CrossToneType::DcsCtcss
        } else {
            CrossToneType::ToneCtcss
        }
    }

    /// Return the preserved, unidentified upper two bits as a value `0..=3`.
    #[must_use]
    pub const fn unidentified_bits(self) -> u8 {
        self.0 >> 2
    }
}

impl TryFrom<u8> for CrossToneField {
    type Error = ValidationError;

    fn try_from(raw: u8) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

impl From<CrossToneField> for u8 {
    fn from(field: CrossToneField) -> Self {
        field.as_raw()
    }
}

impl From<CrossToneType> for CrossToneField {
    fn from(tone_type: CrossToneType) -> Self {
        Self(u8::from(tone_type))
    }
}

/// Exact value stored in MCP/SD-card byte `0x0E` bits 3:2.
///
/// Retained radio images use all four possible values, but the repository has
/// no controlled serializer diff proving their semantic labels. The raw
/// two-bit value remains typed and range-checked without inventing a meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelByte0eBits3To2(u8);

impl ChannelByte0eBits3To2 {
    /// Number of values representable by the two-bit field.
    pub const COUNT: u8 = 4;

    /// Construct the exact two-bit field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above `3`.
    pub const fn new(raw: u8) -> Result<Self, ValidationError> {
        if raw < Self::COUNT {
            Ok(Self(raw))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "channel byte 0x0E bits 3:2",
                value: raw,
                detail: "must fit two bits (0-3)",
            })
        }
    }

    /// Return the exact two-bit value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ChannelByte0eBits3To2 {
    type Error = ValidationError;

    fn try_from(raw: u8) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

impl From<ChannelByte0eBits3To2> for u8 {
    fn from(bits: ChannelByte0eBits3To2) -> Self {
        bits.as_raw()
    }
}

/// Fine tuning step size stored at byte 0x09 bits \[1:0\].
///
/// Used in conjunction with the fine-mode flag (byte 0x09 bit 2) for
/// sub-kHz frequency adjustment.
///
/// Per User Manual Chapter 12: fine tuning is available only on Band B
/// in LSB, USB, CW, or AM modes. It does not work on Band A, or in
/// FM/DV modes. Activated via `[F]`, `[MHz]` -> On. While fine tuning
/// is active, the 100 Hz digit appears on the display, and step size,
/// MHz mode, and MHz scan are disabled. Turning fine tuning off does
/// not change the current frequency, but the next frequency change
/// uses the normal step size. The fine step can be set independently
/// per frequency band.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FineStep {
    /// 20 Hz fine step (value 0).
    Hz20 = 0,
    /// 100 Hz fine step (value 1).
    Hz100 = 1,
    /// 500 Hz fine step (value 2).
    Hz500 = 2,
    /// 1000 Hz fine step (value 3).
    Hz1000 = 3,
}

impl FineStep {
    /// Number of valid fine step values (0-3).
    pub const COUNT: u8 = 4;
}

impl TryFrom<u8> for FineStep {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hz20),
            1 => Ok(Self::Hz100),
            2 => Ok(Self::Hz500),
            3 => Ok(Self::Hz1000),
            _ => Err(ValidationError::FineStepOutOfRange(value)),
        }
    }
}

impl From<FineStep> for u8 {
    fn from(fs: FineStep) -> Self {
        fs as Self
    }
}

impl fmt::Display for FineStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hz20 => f.write_str("20 Hz"),
            Self::Hz100 => f.write_str("100 Hz"),
            Self::Hz500 => f.write_str("500 Hz"),
            Self::Hz1000 => f.write_str("1000 Hz"),
        }
    }
}

/// Exact 40-byte channel record used by MCP programming and `.d75` files.
///
/// Established wire fields have semantic types. Redundant and constrained
/// bits are derived during serialization and validated during parsing;
/// unidentified code bits are retained exactly without semantic guesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChannel {
    /// Receive frequency in hertz (bytes `0x00..0x04`).
    pub receive_frequency: Frequency,
    /// Repeater offset or absolute split transmit frequency (bytes `0x04..0x08`).
    pub transmit_offset_or_frequency: Frequency,
    /// Receive tuning step (byte `0x08` high nibble).
    pub receive_step: StepSize,
    /// Transmit tuning step (byte `0x08` low nibble).
    pub transmit_step: StepSize,
    /// Stored operating mode (byte `0x09` high nibble).
    pub mode: ChannelMode,
    /// Whether fine tuning is enabled (byte `0x09` bit 2).
    pub fine_tuning: bool,
    /// Fine-tuning step (byte `0x09` bits 1:0).
    pub fine_step: FineStep,
    /// The single active tone-signaling mode (byte `0x0A` high nibble).
    pub tone_mode: ToneMode,
    /// Whether repeater reverse is enabled (byte `0x0A` bit 3).
    pub reverse: bool,
    /// Whether the transmit value is an independent frequency (byte `0x0A` bit 2).
    pub split: bool,
    /// Repeater shift direction (byte `0x0A` bits 1:0).
    pub shift: ShiftDirection,
    /// Transmit tone or tone-burst index (byte `0x0B`).
    pub tone_code: ToneCode,
    /// Receive CTCSS decoder index (byte `0x0C` bits 5:0).
    pub ctcss_code: CtcssCode,
    /// DCS code index (byte `0x0D` bits 6:0).
    pub dcs_code: DcsCode,
    /// Exact cross-tone field, including unidentified bits (byte `0x0E` high nibble).
    pub cross_tone: CrossToneField,
    /// Exact, semantically unidentified value from byte `0x0E` bits 3:2.
    pub byte_0e_bits_3_to_2: ChannelByte0eBits3To2,
    /// Per-channel D-STAR squelch selection (byte `0x0E` bits 1:0).
    pub digital_squelch: DigitalSquelchType,
    /// D-STAR destination callsign (bytes `0x0F..0x17`).
    pub ur_call: DstarCallsign,
    /// D-STAR first repeater callsign (bytes `0x17..0x1F`).
    pub rpt1: DstarCallsign,
    /// D-STAR second repeater callsign (bytes `0x1F..0x27`).
    pub rpt2: DstarCallsign,
    /// Per-channel D-STAR squelch code (byte `0x27` bits 6:0).
    pub digital_squelch_code: DigitalSquelchCode,
    /// Exact unidentified high bits from bytes `0x0C`, `0x0D`, and `0x27`.
    pub unidentified_code_bits: ChannelCodeUnidentifiedBits,
}

impl StoredChannel {
    /// Size of one packed stored record.
    pub const BYTE_SIZE: usize = 40;

    /// Validate the receive-frequency marker for a programmed slot.
    ///
    /// A `StoredChannel` can be decoded without its separate flag record, so
    /// [`Self::from_bytes`] accepts the zero and erased markers used by empty
    /// storage. Once a flag classifies the record as programmed, those markers
    /// are invalid.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FrequencyOutOfRange`] when the receive
    /// frequency is zero or the erased `u32::MAX` marker.
    pub const fn validate_programmed(&self) -> Result<(), ValidationError> {
        let frequency = self.receive_frequency.as_hz();
        if frequency == 0 || frequency == u32::MAX {
            Err(ValidationError::FrequencyOutOfRange(frequency))
        } else {
            Ok(())
        }
    }

    /// Parse one exact stored-channel record.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FieldParse`] when the length is not exactly
    /// 40 bytes, a constrained field contains an invalid value, or the
    /// redundant NFM marker contradicts the stored mode.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let actual_length = bytes.len();
        let bytes: &[u8; Self::BYTE_SIZE] = bytes.try_into().map_err(|_| {
            Self::parse_error(
                "length",
                format!(
                    "expected exactly {} bytes, got {actual_length}",
                    Self::BYTE_SIZE
                ),
            )
        })?;

        let receive_frequency = Frequency::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let transmit_offset_or_frequency =
            Frequency::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        let receive_step = Self::parse_field("receive step", StepSize::try_from(bytes[0x08] >> 4))?;
        let transmit_step =
            Self::parse_field("transmit step", StepSize::try_from(bytes[0x08] & 0x0F))?;

        let mode_and_fine = bytes[0x09];
        let mode = Self::parse_field("mode", ChannelMode::try_from(mode_and_fine >> 4))?;
        let nfm_marker = mode_and_fine & 0x08 != 0;
        let expected_nfm_marker = mode == ChannelMode::Nfm;
        if nfm_marker != expected_nfm_marker {
            return Err(Self::parse_error(
                "NFM marker",
                format!(
                    "bit 3 is {}, but mode {mode} requires {}",
                    u8::from(nfm_marker),
                    u8::from(expected_nfm_marker),
                ),
            ));
        }
        let fine_tuning = mode_and_fine & 0x04 != 0;
        let fine_step = Self::parse_field("fine step", FineStep::try_from(mode_and_fine & 0x03))?;

        let tone_and_shift = bytes[0x0A];
        let tone_mode = Self::parse_field("tone mode", ToneMode::try_from(tone_and_shift >> 4))?;
        let reverse = tone_and_shift & 0x08 != 0;
        let split = tone_and_shift & 0x04 != 0;
        let shift = Self::parse_field("shift", ShiftDirection::try_from(tone_and_shift & 0x03))?;

        let transmit_tone_code = Self::parse_field("tone code", ToneCode::new(bytes[0x0B]))?;
        let ctcss_code = Self::parse_field("CTCSS code", CtcssCode::new(bytes[0x0C] & 0x3F))?;
        let dcs_code = Self::parse_field("DCS code", DcsCode::new(bytes[0x0D] & 0x7F))?;

        let cross_route_and_squelch = bytes[0x0E];
        let cross_tone = Self::parse_field(
            "cross-tone field",
            CrossToneField::new(cross_route_and_squelch >> 4),
        )?;
        let byte_0e_bits_3_to_2 = Self::parse_field(
            "byte 0x0E bits 3:2",
            ChannelByte0eBits3To2::new((cross_route_and_squelch >> 2) & 0x03),
        )?;
        let digital_squelch = Self::parse_field(
            "digital squelch",
            DigitalSquelchType::try_from(cross_route_and_squelch & 0x03),
        )?;

        let mut ur_call_bytes = [0_u8; DstarCallsign::WIRE_LEN];
        ur_call_bytes.copy_from_slice(&bytes[0x0F..0x17]);
        let ur_call =
            Self::parse_field("URCALL", DstarCallsign::try_from_flash_bytes(ur_call_bytes))?;

        let mut rpt1_bytes = [0_u8; DstarCallsign::WIRE_LEN];
        rpt1_bytes.copy_from_slice(&bytes[0x17..0x1F]);
        let rpt1 = Self::parse_field("RPT1", DstarCallsign::try_from_flash_bytes(rpt1_bytes))?;

        let mut rpt2_bytes = [0_u8; DstarCallsign::WIRE_LEN];
        rpt2_bytes.copy_from_slice(&bytes[0x1F..0x27]);
        let rpt2 = Self::parse_field("RPT2", DstarCallsign::try_from_flash_bytes(rpt2_bytes))?;

        let digital_squelch_code = Self::parse_field(
            "digital squelch code",
            DigitalSquelchCode::new(bytes[0x27] & 0x7F),
        )?;
        let unidentified_code_bits = ChannelCodeUnidentifiedBits {
            ctcss_code_bits_7_to_6: bytes[0x0C] >> 6,
            dcs_code_bit_7: bytes[0x0D] & 0x80 != 0,
            digital_squelch_code_bit_7: bytes[0x27] & 0x80 != 0,
        };

        Ok(Self {
            receive_frequency,
            transmit_offset_or_frequency,
            receive_step,
            transmit_step,
            mode,
            fine_tuning,
            fine_step,
            tone_mode,
            reverse,
            split,
            shift,
            tone_code: transmit_tone_code,
            ctcss_code,
            dcs_code,
            cross_tone,
            byte_0e_bits_3_to_2,
            digital_squelch,
            ur_call,
            rpt1,
            rpt2,
            digital_squelch_code,
            unidentified_code_bits,
        })
    }

    /// Serialize this channel to the exact 40-byte stored representation.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::BYTE_SIZE] {
        let mut bytes = [0_u8; Self::BYTE_SIZE];

        bytes[0x00..0x04].copy_from_slice(&self.receive_frequency.to_le_bytes());
        bytes[0x04..0x08].copy_from_slice(&self.transmit_offset_or_frequency.to_le_bytes());

        bytes[0x08] = (u8::from(self.receive_step) << 4) | u8::from(self.transmit_step);
        bytes[0x09] = (u8::from(self.mode) << 4)
            | (u8::from(self.mode == ChannelMode::Nfm) << 3)
            | (u8::from(self.fine_tuning) << 2)
            | u8::from(self.fine_step);
        bytes[0x0A] = (u8::from(self.tone_mode) << 4)
            | (u8::from(self.reverse) << 3)
            | (u8::from(self.split) << 2)
            | u8::from(self.shift);
        bytes[0x0B] = self.tone_code.as_raw();
        bytes[0x0C] = self
            .unidentified_code_bits
            .ctcss_wire_value(self.ctcss_code);
        bytes[0x0D] = self.unidentified_code_bits.dcs_wire_value(self.dcs_code);
        bytes[0x0E] = (u8::from(self.cross_tone) << 4)
            | (u8::from(self.byte_0e_bits_3_to_2) << 2)
            | u8::from(self.digital_squelch);

        bytes[0x0F..0x17].copy_from_slice(&self.ur_call.to_flash_bytes());
        bytes[0x17..0x1F].copy_from_slice(&self.rpt1.to_flash_bytes());
        bytes[0x1F..0x27].copy_from_slice(&self.rpt2.to_flash_bytes());
        bytes[0x27] = self
            .unidentified_code_bits
            .digital_squelch_wire_value(self.digital_squelch_code);

        bytes
    }

    fn parse_field<T, E>(field: &'static str, result: Result<T, E>) -> Result<T, ProtocolError>
    where
        E: fmt::Display,
    {
        result.map_err(|error| Self::parse_error(field, error))
    }

    fn parse_error(field: &'static str, detail: impl fmt::Display) -> ProtocolError {
        ProtocolError::FieldParse {
            command: "stored channel".to_owned(),
            field: field.to_owned(),
            detail: detail.to_string(),
        }
    }

    /// Interpret the stored transmit field using this record's split flag.
    #[must_use]
    pub const fn transmit_value(&self) -> ChannelTransmitValue {
        if self.split {
            ChannelTransmitValue::SplitTransmitFrequency(self.transmit_offset_or_frequency)
        } else {
            ChannelTransmitValue::RepeaterOffset(self.transmit_offset_or_frequency)
        }
    }
}

/// Exact channel-data storage classified by its separate flag record.
///
/// Programmed slots contain a [`StoredChannel`] whose receive frequency cannot
/// be either empty-storage marker. Unprogrammed slots retain their
/// uninterpreted 40 bytes because physical radio images use erased `0xFF`
/// records that are intentionally not valid programmed-channel data. Private
/// state variants prevent callers from pairing a programmed classification
/// with an empty frequency marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChannelData(StoredChannelDataState);

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredChannelDataState {
    Programmed(StoredChannel),
    Unprogrammed([u8; StoredChannel::BYTE_SIZE]),
}

impl StoredChannelData {
    /// Classify a decoded channel as programmed.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FrequencyOutOfRange`] when the receive
    /// frequency is zero or the erased `u32::MAX` marker.
    pub fn new_programmed(channel: StoredChannel) -> Result<Self, ValidationError> {
        channel.validate_programmed()?;
        Ok(Self(StoredChannelDataState::Programmed(channel)))
    }

    pub(crate) const fn new_unprogrammed(bytes: [u8; StoredChannel::BYTE_SIZE]) -> Self {
        Self(StoredChannelDataState::Unprogrammed(bytes))
    }

    /// Decode or retain one exact record according to its flag.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] if the record length is not exactly 40 bytes
    /// or a programmed record contains an invalid field.
    pub fn from_bytes(bytes: &[u8], flag: StoredChannelFlag) -> Result<Self, ProtocolError> {
        if flag.is_programmed() {
            let channel = StoredChannel::from_bytes(bytes)?;
            Self::new_programmed(channel)
                .map_err(|error| StoredChannel::parse_error("receive frequency", error))
        } else {
            let actual_length = bytes.len();
            let wire = bytes.try_into().map_err(|_| ProtocolError::FieldParse {
                command: "stored channel data".to_owned(),
                field: "length".to_owned(),
                detail: format!(
                    "expected exactly {} bytes, got {actual_length}",
                    StoredChannel::BYTE_SIZE,
                ),
            })?;
            Ok(Self::new_unprogrammed(wire))
        }
    }

    /// Return the decoded channel when the slot is programmed.
    #[must_use]
    pub const fn programmed(&self) -> Option<&StoredChannel> {
        match &self.0 {
            StoredChannelDataState::Programmed(channel) => Some(channel),
            StoredChannelDataState::Unprogrammed(_) => None,
        }
    }

    /// Return the preserved bytes when the slot is unprogrammed.
    #[must_use]
    pub const fn unprogrammed_bytes(&self) -> Option<&[u8; StoredChannel::BYTE_SIZE]> {
        match &self.0 {
            StoredChannelDataState::Programmed(_) => None,
            StoredChannelDataState::Unprogrammed(bytes) => Some(bytes),
        }
    }

    /// Serialize or return the exact 40-byte stored representation.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; StoredChannel::BYTE_SIZE] {
        match &self.0 {
            StoredChannelDataState::Programmed(channel) => channel.to_bytes(),
            StoredChannelDataState::Unprogrammed(bytes) => *bytes,
        }
    }
}

/// One physical MCP channel-data slot paired with its exact flag record.
///
/// The physical index is the zero-based position in the radio's 1,152-record
/// channel-data region. Indices 0-999 correspond to regular memories; the
/// remaining indices belong to special memories and therefore must not be
/// coerced into [`RegularChannel`]. Empty slots retain their uninterpreted
/// data bytes through [`StoredChannelData::unprogrammed_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChannelSlot {
    physical_index: usize,
    flag: StoredChannelFlag,
    data: StoredChannelData,
}

impl StoredChannelSlot {
    pub(crate) const fn new(
        physical_index: usize,
        flag: StoredChannelFlag,
        data: StoredChannelData,
    ) -> Self {
        Self {
            physical_index,
            flag,
            data,
        }
    }

    /// Return the zero-based index in the physical channel-data region.
    #[must_use]
    pub const fn physical_index(&self) -> usize {
        self.physical_index
    }

    /// Return the exact four-byte flag associated with this data slot.
    #[must_use]
    pub const fn flag(&self) -> StoredChannelFlag {
        self.flag
    }

    /// Return the decoded programmed record or preserved empty-slot bytes.
    #[must_use]
    pub const fn data(&self) -> &StoredChannelData {
        &self.data
    }

    /// Return whether the associated flag marks this slot as programmed.
    #[must_use]
    pub const fn is_programmed(&self) -> bool {
        self.flag.is_programmed()
    }

    /// Consume the slot and return its physical index, exact flag, and data.
    #[must_use]
    pub const fn into_parts(self) -> (usize, StoredChannelFlag, StoredChannelData) {
        (self.physical_index, self.flag, self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ValidationError;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn synthetic_stored_channel(receive_frequency: Frequency) -> StoredChannel {
        let mut wire = [0_u8; StoredChannel::BYTE_SIZE];
        wire[..4].copy_from_slice(&receive_frequency.to_le_bytes());
        StoredChannel::from_bytes(&wire).unwrap_or_else(|error| {
            unreachable!("fixed all-zero synthetic channel record must decode: {error}")
        })
    }

    #[test]
    fn stored_channel_flag_preserves_physical_opaque_bits() -> TestResult {
        let wire = [0x08, 0xA0, 0x00, 0x00];
        let flag = StoredChannelFlag::try_from_wire(wire)?;
        assert!(flag.is_programmed());
        assert_eq!(flag.band(), Some(MemoryChannelBand::Vhf));
        assert_eq!(flag.group(), Some(MemoryGroup::new(0)?));
        assert_eq!(flag.scan_lockout(), Some(false));
        assert_eq!(flag.to_wire_bytes(), wire);
        Ok(())
    }

    #[test]
    fn stored_channel_flag_decodes_verified_50_mhz_code() -> TestResult {
        let flag = StoredChannelFlag::try_from_wire([0x05, 0x01, 0x07, 0xA5])?;
        assert_eq!(flag.band(), Some(MemoryChannelBand::Band50MHz));
        assert_eq!(flag.group(), Some(MemoryGroup::new(7)?));
        assert_eq!(flag.scan_lockout(), Some(true));
        assert_eq!(flag.to_wire_bytes(), [0x05, 0x01, 0x07, 0xA5]);
        Ok(())
    }

    #[test]
    fn stored_channel_flag_rejects_unverified_programmed_fields() {
        assert!(matches!(
            StoredChannelFlag::try_from_wire([0x03, 0x00, 0x00, 0xFF]),
            Err(ValidationError::MemoryChannelBandOutOfRange { marker: 3 })
        ));
        assert!(matches!(
            StoredChannelFlag::try_from_wire([0x00, 0x00, 30, 0xFF]),
            Err(ValidationError::MemoryGroupOutOfRange { group: 30 })
        ));
    }

    #[test]
    fn stored_channel_flag_keeps_empty_record_opaque() -> TestResult {
        let wire = [0xFF, 0xA5, 0xFE, 0x00];
        let flag = StoredChannelFlag::try_from_wire(wire)?;
        assert!(flag.is_empty());
        assert_eq!(flag.band(), None);
        assert_eq!(flag.group(), None);
        assert_eq!(flag.scan_lockout(), None);
        assert_eq!(flag.to_wire_bytes(), wire);
        Ok(())
    }

    #[test]
    fn empty_regular_channel_flags_retain_the_default_hundreds_group() -> TestResult {
        let channel_140 = RegularChannel::new(140)?;
        let channel_999 = RegularChannel::new(999)?;

        assert_eq!(
            StoredChannelFlag::empty_for_regular_channel(channel_140).to_wire_bytes(),
            [0xFF, 0x00, 0x01, 0xFF],
        );
        assert_eq!(
            StoredChannelFlag::empty_for_regular_channel(channel_999).to_wire_bytes(),
            [0xFF, 0x00, 0x09, 0xFF],
        );
        Ok(())
    }

    #[test]
    fn stored_channel_data_preserves_every_unprogrammed_byte() -> TestResult {
        let mut wire = [0xFF; StoredChannel::BYTE_SIZE];
        wire[0x0E] = 0x6B;
        wire[0x27] = 0x80;
        let flag = StoredChannelFlag::empty_for_regular_channel(RegularChannel::new(140)?);

        let stored = StoredChannelData::from_bytes(&wire, flag)?;

        assert_eq!(stored.programmed(), None);
        assert_eq!(stored.unprogrammed_bytes(), Some(&wire));
        assert_eq!(stored.to_bytes(), wire);
        Ok(())
    }

    fn populated_stored_channel() -> Result<StoredChannel, ValidationError> {
        Ok(StoredChannel {
            receive_frequency: Frequency::new(145_670_000),
            transmit_offset_or_frequency: Frequency::new(600_000),
            receive_step: StepSize::Hz12500,
            transmit_step: StepSize::Hz25000,
            mode: ChannelMode::Lsb,
            fine_tuning: true,
            fine_step: FineStep::Hz500,
            tone_mode: ToneMode::CrossTone,
            reverse: true,
            split: true,
            shift: ShiftDirection::Plus,
            tone_code: ToneCode::new(50)?,
            ctcss_code: CtcssCode::new(12)?,
            dcs_code: DcsCode::new(42)?,
            cross_tone: CrossToneField::new(0x09)?,
            byte_0e_bits_3_to_2: ChannelByte0eBits3To2::new(2)?,
            digital_squelch: DigitalSquelchType::CallsignSquelch,
            ur_call: DstarCallsign::new("CQCQCQ")?,
            rpt1: DstarCallsign::new("KQ4NIT B")?,
            rpt2: DstarCallsign::new("KQ4NIT G")?,
            digital_squelch_code: DigitalSquelchCode::new(99)?,
            unidentified_code_bits: ChannelCodeUnidentifiedBits::new(3, true, true)?,
        })
    }

    #[test]
    fn stored_channel_full_record_round_trips_losslessly() -> TestResult {
        let channel = populated_stored_channel()?;
        let bytes = channel.to_bytes();

        assert_eq!(bytes.len(), StoredChannel::BYTE_SIZE);
        assert_eq!(&bytes[0x00..0x04], &145_670_000_u32.to_le_bytes());
        assert_eq!(&bytes[0x04..0x08], &600_000_u32.to_le_bytes());
        assert_eq!(bytes[0x08], 0x58);
        assert_eq!(bytes[0x09], 0x36);
        assert_eq!(bytes[0x0A], 0x1D);
        assert_eq!(bytes[0x0B], 50);
        assert_eq!(bytes[0x0C], 0xCC);
        assert_eq!(bytes[0x0D], 0xAA);
        assert_eq!(bytes[0x0E], 0x9A);
        assert_eq!(&bytes[0x0F..0x17], b"CQCQCQ\0\0");
        assert_eq!(&bytes[0x17..0x1F], b"KQ4NIT B");
        assert_eq!(&bytes[0x1F..0x27], b"KQ4NIT G");
        assert_eq!(bytes[0x27], 0xE3);
        assert_eq!(StoredChannel::from_bytes(&bytes)?, channel);
        assert_eq!(
            channel.transmit_value(),
            ChannelTransmitValue::SplitTransmitFrequency(Frequency::new(600_000)),
        );
        assert_eq!(channel.unidentified_code_bits.ctcss_code_bits_7_to_6(), 3);
        assert!(channel.unidentified_code_bits.dcs_code_bit_7());
        assert!(channel.unidentified_code_bits.digital_squelch_code_bit_7());
        Ok(())
    }

    #[test]
    fn channel_transmit_value_distinguishes_offset_from_split_frequency() {
        let offset = StoredChannel {
            transmit_offset_or_frequency: Frequency::new(600_000),
            ..synthetic_stored_channel(Frequency::new(1))
        };
        assert_eq!(
            offset.transmit_value(),
            ChannelTransmitValue::RepeaterOffset(Frequency::new(600_000)),
        );

        let split = StoredChannel {
            transmit_offset_or_frequency: Frequency::new(146_520_000),
            split: true,
            ..synthetic_stored_channel(Frequency::new(1))
        };
        assert_eq!(
            split.transmit_value(),
            ChannelTransmitValue::SplitTransmitFrequency(Frequency::new(146_520_000)),
        );
    }

    #[test]
    fn stored_channel_mode_nibble_and_nfm_marker_are_exact() -> TestResult {
        for raw in 0..ChannelMode::COUNT {
            let mode = ChannelMode::try_from(raw)?;
            let channel = StoredChannel {
                mode,
                ..synthetic_stored_channel(Frequency::new(1))
            };
            let bytes = channel.to_bytes();

            assert_eq!(bytes[0x09] >> 4, raw);
            assert_eq!(bytes[0x09] & 0x08 != 0, mode == ChannelMode::Nfm);
            assert_eq!(StoredChannel::from_bytes(&bytes)?.mode, mode);
        }
        Ok(())
    }

    #[test]
    fn stored_channel_rejects_contradictory_nfm_markers() {
        let mut fm_with_marker = synthetic_stored_channel(Frequency::new(1)).to_bytes();
        fm_with_marker[0x09] |= 0x08;
        assert!(StoredChannel::from_bytes(&fm_with_marker).is_err());

        let mut nfm_without_marker = StoredChannel {
            mode: ChannelMode::Nfm,
            ..synthetic_stored_channel(Frequency::new(1))
        }
        .to_bytes();
        nfm_without_marker[0x09] &= !0x08;
        assert!(StoredChannel::from_bytes(&nfm_without_marker).is_err());
    }

    #[test]
    fn stored_channel_tone_mode_is_exactly_one_selection() -> TestResult {
        for tone_mode in ToneMode::ALL {
            let channel = StoredChannel {
                tone_mode,
                ..synthetic_stored_channel(Frequency::new(1))
            };
            let bytes = channel.to_bytes();
            assert_eq!(bytes[0x0A] >> 4, u8::from(tone_mode));
            assert_eq!(StoredChannel::from_bytes(&bytes)?.tone_mode, tone_mode);
        }

        for invalid_nibble in [3_u8, 5, 6, 7, 9, 0x0F] {
            let mut bytes = synthetic_stored_channel(Frequency::new(1)).to_bytes();
            bytes[0x0A] = invalid_nibble << 4;
            assert!(StoredChannel::from_bytes(&bytes).is_err());
        }
        Ok(())
    }

    #[test]
    fn stored_channel_step_and_shift_domains_round_trip() -> TestResult {
        for receive_raw in 0..StepSize::COUNT {
            for transmit_raw in 0..StepSize::COUNT {
                let receive_step = StepSize::try_from(receive_raw)?;
                let transmit_step = StepSize::try_from(transmit_raw)?;
                let channel = StoredChannel {
                    receive_step,
                    transmit_step,
                    ..synthetic_stored_channel(Frequency::new(1))
                };
                let parsed = StoredChannel::from_bytes(&channel.to_bytes())?;
                assert_eq!(parsed.receive_step, receive_step);
                assert_eq!(parsed.transmit_step, transmit_step);
            }
        }

        for shift_raw in 0..ShiftDirection::COUNT {
            let shift = ShiftDirection::try_from(shift_raw)?;
            let channel = StoredChannel {
                shift,
                ..synthetic_stored_channel(Frequency::new(1))
            };
            assert_eq!(StoredChannel::from_bytes(&channel.to_bytes())?.shift, shift);
        }
        Ok(())
    }

    #[test]
    fn stored_channel_unidentified_and_digital_squelch_fields_round_trip() -> TestResult {
        for raw in 0..ChannelByte0eBits3To2::COUNT {
            let bits = ChannelByte0eBits3To2::new(raw)?;
            let channel = StoredChannel {
                byte_0e_bits_3_to_2: bits,
                ..synthetic_stored_channel(Frequency::new(1))
            };
            assert_eq!(
                StoredChannel::from_bytes(&channel.to_bytes())?.byte_0e_bits_3_to_2,
                bits,
            );
        }

        for squelch_raw in 0..DigitalSquelchType::COUNT {
            let digital_squelch = DigitalSquelchType::try_from(squelch_raw)?;
            let channel = StoredChannel {
                digital_squelch,
                ..synthetic_stored_channel(Frequency::new(1))
            };
            assert_eq!(
                StoredChannel::from_bytes(&channel.to_bytes())?.digital_squelch,
                digital_squelch,
            );
        }
        Ok(())
    }

    #[test]
    fn programmed_channel_data_rejects_empty_frequency_markers() -> TestResult {
        let flag =
            StoredChannelFlag::programmed(MemoryChannelBand::Vhf, MemoryGroup::new(0)?, false);
        for marker in [0, u32::MAX] {
            let wire = synthetic_stored_channel(Frequency::new(marker)).to_bytes();
            assert!(
                StoredChannelData::from_bytes(&wire, flag).is_err(),
                "programmed receive-frequency marker {marker} must be rejected",
            );
        }
        Ok(())
    }

    #[test]
    fn stored_channel_rejects_every_out_of_domain_wire_field() -> TestResult {
        let invalid_fields = [
            (0x08, 0xF0, "receive step"),
            (0x08, 0x0F, "transmit step"),
            (0x0C, 50, "CTCSS code"),
            (0x0D, 104, "DCS code"),
            (0x0E, 0x0F, "digital squelch"),
            (0x27, 100, "digital squelch code"),
        ];

        for (offset, value, description) in invalid_fields {
            let mut bytes = synthetic_stored_channel(Frequency::new(1)).to_bytes();
            let byte = bytes
                .get_mut(offset)
                .ok_or("test field offset exceeds stored-channel record")?;
            *byte = value;
            assert!(
                StoredChannel::from_bytes(&bytes).is_err(),
                "{description} value 0x{value:02X} must be rejected",
            );
        }
        Ok(())
    }

    #[test]
    fn stored_channel_requires_exact_record_length() {
        for length in [StoredChannel::BYTE_SIZE - 1, StoredChannel::BYTE_SIZE + 1] {
            let bytes = vec![0_u8; length];
            assert!(StoredChannel::from_bytes(&bytes).is_err());
        }
    }

    #[test]
    fn cross_tone_field_preserves_every_wire_nibble() -> TestResult {
        for raw in 0..=CrossToneField::MAX {
            let field = CrossToneField::new(raw)?;
            assert_eq!(field.as_raw(), raw);
            assert_eq!(u8::from(field.tone_type()), raw & 0x03);
            assert_eq!(field.unidentified_bits(), raw >> 2);

            let channel = StoredChannel {
                cross_tone: field,
                ..synthetic_stored_channel(Frequency::new(1))
            };
            assert_eq!(
                StoredChannel::from_bytes(&channel.to_bytes())?.cross_tone,
                field
            );
        }
        assert!(CrossToneField::new(CrossToneField::MAX + 1).is_err());
        Ok(())
    }

    #[test]
    fn cross_tone_and_fine_step_wire_values_round_trip() -> TestResult {
        for raw in 0..CrossToneType::COUNT {
            let value = CrossToneType::try_from(raw)?;
            assert_eq!(u8::from(value), raw);
        }
        assert!(CrossToneType::try_from(CrossToneType::COUNT).is_err());

        for raw in 0..FineStep::COUNT {
            let value = FineStep::try_from(raw)?;
            assert_eq!(u8::from(value), raw);
        }
        assert!(FineStep::try_from(FineStep::COUNT).is_err());
        Ok(())
    }
}
