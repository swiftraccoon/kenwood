//! D-STAR (Digital Smart Technologies for Amateur Radio) settings types.
//!
//! D-STAR is a digital voice and data protocol for amateur radio developed
//! by JARL (Japan Amateur Radio League). The TH-D75 supports DV (Digital
//! Voice) mode with features including reflector linking, callsign routing,
//! gateway access, and DR (D-STAR Repeater) mode for simplified operation.
//!
//! # Callsign registration (per Operating Tips §4.1.1)
//!
//! Before using D-STAR gateway/reflector functions, the operator's callsign
//! must be registered at <https://regist.dstargateway.org>.
//!
//! # My Callsign (per Operating Tips §4.1.2)
//!
//! A valid MY callsign is required for any DV or DR mode transmission.
//! Menu No. 610 allows registration of up to 6 callsigns; the active
//! one is selected for transmission.
//!
//! # DR mode (per Operating Tips §4.2)
//!
//! DR (Digital Repeater) mode simplifies D-STAR operation by combining
//! repeater and destination selection into a single interface. The operator
//! selects an access repeater from the repeater list and a destination
//! (another repeater, callsign, or reflector), and the radio automatically
//! configures RPT1, RPT2, and UR callsign fields.
//!
//! # Reflector Terminal Mode (per Operating Tips §4.4)
//!
//! The TH-D75 supports Reflector Terminal Mode, which connects to D-STAR
//! reflectors without a physical hotspot. On Android, use `BlueDV` Connect
//! via Bluetooth; on Windows, use `BlueDV` via Bluetooth or USB.
//!
//! # Simultaneous reception
//!
//! The TH-D75 can receive D-STAR DV signals on both Band A and Band B
//! simultaneously.
//!
//! # Repeater and Hotspot lists (per Operating Tips §4.3)
//!
//! The radio stores up to 1500 repeater list entries and 30 hotspot list
//! entries. These are managed via the MCP-D75 software or SD card import.
//!
//! These types model D-STAR values whose domains and storage representations
//! are established by the user manual, CAT records, or MCP schema. The module
//! deliberately has no catch-all settings aggregate: several menu selections
//! have independent multi-valued domains that must not be collapsed to bools.

pub use dstar_gateway_core::SlowDataTextMessageError as DstarMessageError;
use dstar_gateway_core::{Callsign, SlowDataTextMessage, Suffix};
pub use dstar_gateway_core::{Module, ReflectorCallsign};
use encoding_rs::SHIFT_JIS;
use thiserror::Error;

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Callsign types
// ---------------------------------------------------------------------------

/// Validated D-STAR callsign (up to 8 printable ASCII bytes).
///
/// This type stores the semantic value without transport padding. CAT `DC`
/// fields are space-padded, while MCP stored-channel records are NUL-padded;
/// the corresponding conversion methods preserve that distinction. Commas
/// and control bytes are excluded because `DC` is comma-separated and
/// carriage-return-delimited.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DstarCallsign(Callsign);

impl DstarCallsign {
    /// Maximum length of a D-STAR callsign.
    pub const MAX_LEN: usize = 8;

    /// Wire-format width (always 8 characters, space-padded).
    pub const WIRE_LEN: usize = 8;

    /// Creates a new D-STAR callsign.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CallsignTooLong`] if the encoded value
    /// exceeds eight bytes, or [`ValidationError::InvalidDstarCallsignByte`]
    /// if it contains non-printable ASCII or a comma.
    pub fn new(callsign: &str) -> Result<Self, ValidationError> {
        validate_dstar_identity(callsign, "callsign", Self::MAX_LEN)?;

        if callsign.bytes().all(|byte| byte == b' ') {
            return Ok(Self(Callsign::from_wire_bytes([b' '; Self::WIRE_LEN])));
        }

        match Callsign::try_from_str(callsign) {
            Ok(callsign) => Ok(Self(callsign)),
            Err(error) => {
                unreachable!("TH-D75 callsign policy accepted a core-invalid callsign: {error}")
            }
        }
    }

    /// Creates the broadcast CQ callsign ("CQCQCQ").
    #[must_use]
    pub const fn cqcqcq() -> Self {
        Self(Callsign::from_wire_bytes(*b"CQCQCQ  "))
    }

    /// Returns the callsign as a trimmed string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self.0.text() {
            Ok(callsign) => callsign,
            Err(error) => {
                unreachable!("validated TH-D75 callsign became invalid: {error}")
            }
        }
    }

    /// Returns the callsign as an 8-byte space-padded ASCII array
    /// for wire encoding.
    #[must_use]
    pub const fn to_wire_bytes(&self) -> [u8; 8] {
        *self.0.as_bytes()
    }

    /// Decodes a D-STAR callsign from an 8-byte space-padded array.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidDstarCallsignByte`] when the wire
    /// field contains a byte outside printable ASCII or a comma.
    pub fn try_from_wire_bytes(bytes: [u8; Self::WIRE_LEN]) -> Result<Self, ValidationError> {
        validate_dstar_identity_bytes(&bytes, "callsign")?;
        Ok(Self(Callsign::from_wire_bytes(bytes)))
    }

    /// Returns the callsign as an 8-byte NUL-padded MCP channel field.
    #[must_use]
    pub fn to_flash_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut bytes = [0; Self::WIRE_LEN];
        bytes
            .iter_mut()
            .zip(self.as_str().as_bytes())
            .for_each(|(destination, &source)| *destination = source);
        bytes
    }

    /// Decodes an 8-byte NUL-padded callsign from an MCP channel record.
    ///
    /// # Errors
    ///
    /// Returns a validation error for non-ASCII content, CAT delimiters, or
    /// non-NUL data after the first terminator.
    pub fn try_from_flash_bytes(bytes: [u8; Self::WIRE_LEN]) -> Result<Self, ValidationError> {
        let content_length = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());

        let padding = bytes
            .get(content_length..)
            .unwrap_or_else(|| unreachable!("terminator index lies within the callsign field"));
        if let Some((relative_offset, &value)) =
            padding.iter().enumerate().find(|(_, byte)| **byte != 0)
        {
            return Err(ValidationError::InvalidDstarCallsignPadding {
                field: "callsign",
                offset: content_length + relative_offset,
                value,
            });
        }

        let content = bytes
            .get(..content_length)
            .unwrap_or_else(|| unreachable!("terminator index lies within the callsign field"));
        Self::from_ascii_bytes(content)
    }

    fn from_ascii_bytes(bytes: &[u8]) -> Result<Self, ValidationError> {
        validate_dstar_identity_bytes(bytes, "callsign")?;
        let callsign: String = bytes.iter().map(|&byte| char::from(byte)).collect();
        Self::new(&callsign)
    }

    /// Returns `true` if this is the broadcast CQ callsign.
    #[must_use]
    pub fn is_cqcqcq(&self) -> bool {
        self.0.as_bytes() == b"CQCQCQ  "
    }

    /// Returns `true` if the callsign is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        let [first, second, third, fourth, fifth, sixth, seventh, eighth] = *self.0.as_bytes();
        first == b' '
            && second == b' '
            && third == b' '
            && fourth == b' '
            && fifth == b' '
            && sixth == b' '
            && seventh == b' '
            && eighth == b' '
    }
}

impl TryFrom<ReflectorCallsign> for DstarCallsign {
    type Error = ValidationError;

    fn try_from(reflector: ReflectorCallsign) -> Result<Self, Self::Error> {
        Self::try_from_wire_bytes(*reflector.callsign().as_bytes())
    }
}

impl Default for DstarCallsign {
    fn default() -> Self {
        Self(Callsign::from_wire_bytes([b' '; Self::WIRE_LEN]))
    }
}

impl std::fmt::Debug for DstarCallsign {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("DstarCallsign")
            .field(&self.as_str())
            .finish()
    }
}

impl From<DstarCallsign> for Callsign {
    fn from(callsign: DstarCallsign) -> Self {
        callsign.0
    }
}

impl From<&DstarCallsign> for Callsign {
    fn from(callsign: &DstarCallsign) -> Self {
        callsign.0
    }
}

/// Validated D-STAR MY callsign suffix (up to 4 printable ASCII bytes).
///
/// The suffix is appended to the MY callsign in the D-STAR frame header
/// as additional identification (e.g. "/P" for portable, "/M" for mobile).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DstarSuffix(Suffix);

impl DstarSuffix {
    /// Maximum length of a D-STAR callsign suffix.
    pub const MAX_LEN: usize = 4;

    /// Wire-format width (always 4 characters, space-padded).
    pub const WIRE_LEN: usize = 4;

    /// Creates a new D-STAR callsign suffix.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CallsignTooLong`] if the encoded value
    /// exceeds four bytes, or [`ValidationError::InvalidDstarCallsignByte`]
    /// if it contains non-printable ASCII or a comma.
    pub fn new(suffix: &str) -> Result<Self, ValidationError> {
        validate_dstar_identity(suffix, "suffix", Self::MAX_LEN)?;
        match Suffix::try_from_str(suffix) {
            Ok(suffix) => Ok(Self(suffix)),
            Err(error) => {
                unreachable!("TH-D75 suffix policy accepted a core-invalid suffix: {error}")
            }
        }
    }

    /// Creates the exact reflector-link suffix for `module`.
    ///
    /// D-STAR gateways interpret the module letter followed by uppercase `L`
    /// as a link request. The remaining two bytes are space padding.
    #[must_use]
    pub const fn reflector_link(module: Module) -> Self {
        Self(Suffix::from_wire_bytes([
            module.as_byte(),
            b'L',
            b' ',
            b' ',
        ]))
    }

    /// Returns the suffix as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self.0.text() {
            Ok(suffix) => suffix,
            Err(error) => unreachable!("validated TH-D75 suffix became invalid: {error}"),
        }
    }

    /// Returns the suffix as a 4-byte space-padded ASCII array.
    #[must_use]
    pub const fn to_wire_bytes(&self) -> [u8; Self::WIRE_LEN] {
        *self.0.as_bytes()
    }

    /// Decodes a four-byte space-padded D-STAR suffix.
    ///
    /// # Errors
    ///
    /// Returns a validation error rather than replacing invalid bytes.
    pub fn try_from_wire_bytes(bytes: [u8; Self::WIRE_LEN]) -> Result<Self, ValidationError> {
        validate_dstar_identity_bytes(&bytes, "suffix")?;
        Ok(Self(Suffix::from_wire_bytes(bytes)))
    }
}

impl Default for DstarSuffix {
    fn default() -> Self {
        Self(Suffix::EMPTY)
    }
}

impl std::fmt::Debug for DstarSuffix {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("DstarSuffix")
            .field(&self.as_str())
            .finish()
    }
}

impl From<DstarSuffix> for Suffix {
    fn from(suffix: DstarSuffix) -> Self {
        suffix.0
    }
}

impl From<&DstarSuffix> for Suffix {
    fn from(suffix: &DstarSuffix) -> Self {
        suffix.0
    }
}

/// One D-STAR callsign slot value with its separately encoded suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DstarCallsignEntry {
    /// Eight-character D-STAR callsign field with transport padding removed.
    pub callsign: DstarCallsign,
    /// Four-character suffix field with transport padding removed.
    pub suffix: DstarSuffix,
}

impl DstarCallsignEntry {
    /// Combines an already-validated callsign and suffix.
    #[must_use]
    pub const fn new(callsign: DstarCallsign, suffix: DstarSuffix) -> Self {
        Self { callsign, suffix }
    }
}

fn validate_dstar_identity(
    value: &str,
    field: &'static str,
    max: usize,
) -> Result<(), ValidationError> {
    if value.len() > max {
        return Err(ValidationError::CallsignTooLong {
            len: value.len(),
            max,
        });
    }

    validate_dstar_identity_bytes(value.as_bytes(), field)
}

fn validate_dstar_identity_bytes(value: &[u8], field: &'static str) -> Result<(), ValidationError> {
    if let Some((offset, &value)) = value
        .iter()
        .enumerate()
        .find(|(_, value)| !(b' '..=b'~').contains(&**value) || **value == b',')
    {
        return Err(ValidationError::InvalidDstarCallsignByte {
            field,
            offset,
            value,
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// GPS Data TX
// ---------------------------------------------------------------------------

/// A sentence selectable for D-STAR GPS Data TX (Menu 631).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DstarGpsDataTxSentence {
    /// Global Positioning System Fix Data.
    Gga,
    /// Geographic Position (latitude and longitude).
    Gll,
    /// DOP and active satellites.
    Gsa,
    /// Satellites in view.
    Gsv,
    /// Recommended Minimum Specific GNSS Data.
    Rmc,
    /// Course over ground and ground speed.
    Vtg,
    /// D-STAR APRS-format sentence.
    Aprs,
}

impl DstarGpsDataTxSentence {
    const fn bit(self) -> u8 {
        match self {
            Self::Gga => 1 << 0,
            Self::Gll => 1 << 1,
            Self::Gsa => 1 << 2,
            Self::Gsv => 1 << 3,
            Self::Rmc => 1 << 4,
            Self::Vtg => 1 << 5,
            Self::Aprs => 1 << 6,
        }
    }

    /// Returns the sentence label used by the radio menu.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Gga => "GGA",
            Self::Gll => "GLL",
            Self::Gsa => "GSA",
            Self::Gsv => "GSV",
            Self::Rmc => "RMC",
            Self::Vtg => "VTG",
            Self::Aprs => "APRS",
        }
    }
}

/// Valid D-STAR GPS Data TX sentence selection (Menu 631).
///
/// The radio permits at most four NMEA sentences. APRS is exclusive and
/// therefore cannot be combined with any NMEA sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarGpsDataTxSentences(u8);

impl DstarGpsDataTxSentences {
    const VALID_BITS: u8 = 0x7F;
    const APRS_BIT: u8 = 1 << 6;

    /// Returns the documented factory selection: GGA and RMC.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(DstarGpsDataTxSentence::Gga.bit() | DstarGpsDataTxSentence::Rmc.bit())
    }

    /// Returns the seven-bit MCP representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether `sentence` is selected.
    #[must_use]
    pub const fn contains(self, sentence: DstarGpsDataTxSentence) -> bool {
        self.0 & sentence.bit() != 0
    }

    /// Adds a sentence while preserving the radio's selection constraints.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if the result selects
    /// more than four NMEA sentences or combines APRS with an NMEA sentence.
    pub fn with(self, sentence: DstarGpsDataTxSentence) -> Result<Self, ValidationError> {
        Self::try_from(self.0 | sentence.bit())
    }

    /// Removes a sentence. An empty stored selection is preserved because the
    /// retained manual does not state that Menu 631 forbids it.
    #[must_use]
    pub const fn without(self, sentence: DstarGpsDataTxSentence) -> Self {
        Self(self.0 & !sentence.bit())
    }
}

impl TryFrom<u8> for DstarGpsDataTxSentences {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        let has_reserved_bits = value & !Self::VALID_BITS != 0;
        let combines_aprs = value & Self::APRS_BIT != 0 && value != Self::APRS_BIT;
        let too_many_nmea = value & Self::APRS_BIT == 0 && value.count_ones() > 4;
        if has_reserved_bits || combines_aprs || too_many_nmea {
            Err(ValidationError::SettingOutOfRange {
                name: "D-STAR GPS Data TX sentence selection",
                value,
                detail: "bits 0-5 allow at most four NMEA sentences; bit 6 APRS is exclusive",
            })
        } else {
            Ok(Self(value))
        }
    }
}

impl From<DstarGpsDataTxSentences> for u8 {
    fn from(sentences: DstarGpsDataTxSentences) -> Self {
        sentences.bits()
    }
}

/// Discrete D-STAR GPS Auto TX interval (Menu 632).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DstarGpsAutoTxInterval {
    /// Automatic transmission disabled.
    Off = 0,
    /// 0.2 minutes (12 seconds).
    Seconds12 = 1,
    /// 0.5 minutes (30 seconds).
    Seconds30 = 2,
    /// One minute.
    OneMinute = 3,
    /// Two minutes.
    TwoMinutes = 4,
    /// Three minutes.
    ThreeMinutes = 5,
    /// Five minutes.
    FiveMinutes = 6,
    /// Ten minutes.
    TenMinutes = 7,
    /// Twenty minutes.
    TwentyMinutes = 8,
    /// Thirty minutes.
    ThirtyMinutes = 9,
    /// Sixty minutes.
    SixtyMinutes = 10,
}

impl DstarGpsAutoTxInterval {
    /// Returns the interval in seconds, or `None` when Auto TX is off.
    #[must_use]
    pub const fn as_seconds(self) -> Option<u16> {
        match self {
            Self::Off => None,
            Self::Seconds12 => Some(12),
            Self::Seconds30 => Some(30),
            Self::OneMinute => Some(60),
            Self::TwoMinutes => Some(120),
            Self::ThreeMinutes => Some(180),
            Self::FiveMinutes => Some(300),
            Self::TenMinutes => Some(600),
            Self::TwentyMinutes => Some(1200),
            Self::ThirtyMinutes => Some(1800),
            Self::SixtyMinutes => Some(3600),
        }
    }
}

impl TryFrom<u8> for DstarGpsAutoTxInterval {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Seconds12),
            2 => Ok(Self::Seconds30),
            3 => Ok(Self::OneMinute),
            4 => Ok(Self::TwoMinutes),
            5 => Ok(Self::ThreeMinutes),
            6 => Ok(Self::FiveMinutes),
            7 => Ok(Self::TenMinutes),
            8 => Ok(Self::TwentyMinutes),
            9 => Ok(Self::ThirtyMinutes),
            10 => Ok(Self::SixtyMinutes),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "D-STAR GPS Auto TX interval",
                value,
                detail: "must be raw index 0-10",
            }),
        }
    }
}

impl From<DstarGpsAutoTxInterval> for u8 {
    fn from(interval: DstarGpsAutoTxInterval) -> Self {
        interval as Self
    }
}

/// D-STAR GPS Data TX settings (Menu 630-632).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarGpsDataTxSettings {
    enabled: bool,
    sentences: DstarGpsDataTxSentences,
    auto_tx: DstarGpsAutoTxInterval,
}

impl DstarGpsDataTxSettings {
    /// Creates settings from three independently stored fields.
    #[must_use]
    pub const fn new(
        enabled: bool,
        sentences: DstarGpsDataTxSentences,
        auto_tx: DstarGpsAutoTxInterval,
    ) -> Self {
        Self {
            enabled,
            sentences,
            auto_tx,
        }
    }

    /// Returns whether GPS information is included in DV frames.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Returns the selected GPS Data TX sentences.
    #[must_use]
    pub const fn sentences(self) -> DstarGpsDataTxSentences {
        self.sentences
    }

    /// Returns the configured automatic-transmission interval.
    #[must_use]
    pub const fn auto_tx(self) -> DstarGpsAutoTxInterval {
        self.auto_tx
    }

    /// Returns the documented TH-D75 factory GPS Data TX settings.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::new(
            false,
            DstarGpsDataTxSentences::factory_default(),
            DstarGpsAutoTxInterval::Off,
        )
    }
}

// ---------------------------------------------------------------------------
// Mode selection
// ---------------------------------------------------------------------------

/// DV/DR mode selection.
///
/// DV mode provides manual repeater configuration; DR mode simplifies
/// operation with automatic repeater selection from the repeater list.
///
/// Per Operating Tips §4.2: DR (Digital Repeater) mode combines repeater
/// selection and destination selection. The radio configures RPT1, RPT2,
/// and UR callsign fields automatically based on the user's choices from
/// the repeater list and destination list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DvDrMode {
    /// DV (Digital Voice) mode -- manual repeater configuration.
    Dv,
    /// DR (D-STAR Repeater) mode -- automatic repeater selection.
    Dr,
}

// ---------------------------------------------------------------------------
// Digital squelch
// ---------------------------------------------------------------------------

/// Validated D-STAR digital squelch code (0-99).
///
/// The TH-D75 uses a numeric code in the range 0-99 for digital code
/// squelch on D-STAR. Only frames with a matching code open the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DigitalSquelchCode(u8);

impl DigitalSquelchCode {
    /// Number of valid digital-squelch codes (`00` through `99`).
    pub const COUNT: u8 = 100;

    /// Creates a new digital squelch code.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DigitalSquelchCodeOutOfRange`] if `code > 99`.
    pub const fn new(code: u8) -> Result<Self, ValidationError> {
        if code <= 99 {
            Ok(Self(code))
        } else {
            Err(ValidationError::DigitalSquelchCodeOutOfRange(code))
        }
    }

    /// Returns the raw code value (0-99).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DigitalSquelchCode {
    type Error = ValidationError;

    fn try_from(code: u8) -> Result<Self, Self::Error> {
        Self::new(code)
    }
}

impl From<DigitalSquelchCode> for u8 {
    fn from(code: DigitalSquelchCode) -> Self {
        code.as_raw()
    }
}

impl std::fmt::Display for DigitalSquelchCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}", self.0)
    }
}

/// Digital squelch settings.
///
/// Digital squelch opens the audio only when the received D-STAR frame
/// header matches specific criteria: a digital code (0-99) or a specific
/// callsign.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DigitalSquelch {
    /// Digital squelch mode.
    pub squelch_type: DigitalSquelchType,
    /// Digital code for code squelch mode (0-99).
    pub code: DigitalSquelchCode,
    /// Callsign for callsign squelch mode.
    pub callsign: DstarCallsign,
}

impl Default for DigitalSquelch {
    fn default() -> Self {
        Self {
            squelch_type: DigitalSquelchType::Off,
            code: DigitalSquelchCode::default(),
            callsign: DstarCallsign::default(),
        }
    }
}

/// Digital squelch type.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DigitalSquelchType {
    /// Digital squelch disabled -- receive all DV signals.
    #[default]
    Off = 0,
    /// Code squelch -- open audio only when the digital code matches.
    CodeSquelch = 1,
    /// Callsign squelch -- open audio only when the source callsign matches.
    CallsignSquelch = 2,
}

impl DigitalSquelchType {
    /// Number of valid digital-squelch type values.
    pub const COUNT: u8 = 3;
}

impl TryFrom<u8> for DigitalSquelchType {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::CodeSquelch),
            2 => Ok(Self::CallsignSquelch),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "digital squelch type",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<DigitalSquelchType> for u8 {
    fn from(squelch_type: DigitalSquelchType) -> Self {
        squelch_type as Self
    }
}

/// D-STAR slow-data transmit message containing up to 20 printable ASCII bytes.
///
/// Storage and validation delegate to
/// [`dstar_gateway_core::SlowDataTextMessage`], the same value consumed by the
/// D-STAR slow-data transmitter. Shorter messages are padded on the wire with
/// ASCII spaces; input is never truncated or replaced with a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarMessage(SlowDataTextMessage);

impl DstarMessage {
    /// Maximum encoded length of a D-STAR message.
    pub const MAX_LEN: usize = dstar_gateway_core::MAX_MESSAGE_LEN;

    /// Creates a D-STAR transmit message from printable ASCII text.
    ///
    /// # Errors
    ///
    /// Returns [`DstarMessageError::TooLong`] if the encoded input exceeds 20
    /// bytes, or [`DstarMessageError::InvalidText`] at the first byte outside
    /// printable ASCII.
    pub fn new(text: &str) -> Result<Self, DstarMessageError> {
        SlowDataTextMessage::try_from_text(text).map(Self)
    }

    /// Returns the validated message without trailing wire padding.
    ///
    /// Leading and interior spaces remain intact. Construction proves that the
    /// wrapped bytes are printable ASCII, so this accessor cannot fail.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.text().unwrap_or_else(|error| {
            unreachable!("validated D-STAR transmit message became invalid: {error}")
        })
    }

    /// Returns the exact 20-byte, space-padded transmitter representation.
    #[must_use]
    pub const fn as_wire_bytes(&self) -> &[u8; Self::MAX_LEN] {
        self.0.as_bytes()
    }
}

impl Default for DstarMessage {
    fn default() -> Self {
        Self(SlowDataTextMessage::from_wire_bytes([b' '; Self::MAX_LEN]))
    }
}

impl TryFrom<&str> for DstarMessage {
    type Error = DstarMessageError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Self::new(text)
    }
}

impl From<DstarMessage> for SlowDataTextMessage {
    fn from(message: DstarMessage) -> Self {
        message.0
    }
}

// ---------------------------------------------------------------------------
// EMR
// ---------------------------------------------------------------------------

/// EMR (Emergency) volume level (Menu No. 615, Level 1-Level 50).
///
/// When EMR mode is activated by the remote station, the radio increases
/// volume to the configured EMR level. Stock TH-D75 V1.03 accepts levels
/// 1 through 50. The TH-D75A user manual marks Level 25 as the factory
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmrVolume(u8);

impl EmrVolume {
    /// Minimum EMR volume level.
    pub const MIN: u8 = 1;
    /// Maximum EMR volume level.
    pub const MAX: u8 = 50;
    /// Factory EMR volume level documented by Kenwood.
    pub const FACTORY_DEFAULT_LEVEL: u8 = 25;

    /// Returns the documented factory EMR volume level.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_LEVEL)
    }

    /// Creates a new EMR volume level.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `level` is in
    /// the inclusive range 1-50.
    pub const fn new(level: u8) -> Result<Self, ValidationError> {
        if level >= Self::MIN && level <= Self::MAX {
            Ok(Self(level))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "D-STAR EMR volume",
                value: level,
                detail: "must be 1-50",
            })
        }
    }

    /// Returns the EMR volume level.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Callsign list entry
// ---------------------------------------------------------------------------

/// A lossless-storage failure for a callsign-list name or memo.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CallsignListTextError {
    /// The text contains NUL, which terminates the fixed-width flash field.
    #[error("NUL at byte {offset} cannot be represented in a callsign-list text field")]
    Nul {
        /// UTF-8 byte offset of the NUL character.
        offset: usize,
    },
    /// A line terminator cannot be represented in one physical TSV row.
    #[error(
        "line terminator {character:?} at byte {offset} cannot appear in a callsign-list field"
    )]
    LineTerminator {
        /// UTF-8 byte offset of the line terminator.
        offset: usize,
        /// Exact line terminator.
        character: char,
    },
    /// The text has no lossless Shift-JIS representation for radio storage.
    #[error("text cannot be represented losslessly in Shift-JIS")]
    UnrepresentableShiftJis,
    /// The encoded text exceeds its fixed-width flash field.
    #[error("Shift-JIS text needs {encoded_len} bytes, but the field stores at most {maximum}")]
    TooLong {
        /// Number of encoded Shift-JIS bytes required.
        encoded_len: usize,
        /// Maximum encoded byte count stored by the radio.
        maximum: usize,
    },
}

/// Validated name in one D-STAR destination-list entry.
///
/// The underlying radio record reserves 16 Shift-JIS bytes for this field.
/// Text is retained exactly; no trimming or replacement characters are used.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallsignListName(String);

impl CallsignListName {
    /// Maximum encoded width of the radio's name field.
    pub const MAX_ENCODED_LEN: usize = 16;

    /// Validate and store a callsign-list name.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListTextError`] if the value cannot be represented
    /// losslessly in the radio's fixed-width Shift-JIS field.
    pub fn new(value: &str) -> Result<Self, CallsignListTextError> {
        validate_callsign_list_text(value, Self::MAX_ENCODED_LEN)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the name exactly as supplied.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Validated memo in one D-STAR destination-list entry.
///
/// The underlying radio record reserves 32 Shift-JIS bytes for this field.
/// Text is retained exactly; no trimming or replacement characters are used.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallsignListMemo(String);

impl CallsignListMemo {
    /// Maximum encoded width of the radio's memo field.
    pub const MAX_ENCODED_LEN: usize = 32;

    /// Validate and store a callsign-list memo.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListTextError`] if the value cannot be represented
    /// losslessly in the radio's fixed-width Shift-JIS field.
    pub fn new(value: &str) -> Result<Self, CallsignListTextError> {
        validate_callsign_list_text(value, Self::MAX_ENCODED_LEN)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the memo exactly as supplied.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Error returned when constructing a D-STAR destination-list entry.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CallsignEntryError {
    /// The name cannot be represented losslessly by the radio.
    #[error("invalid callsign-list name: {0}")]
    InvalidName(CallsignListTextError),
    /// The value is not a valid D-STAR callsign.
    #[error("invalid destination callsign: {0}")]
    InvalidCallsign(ValidationError),
    /// An empty destination cannot occupy a list slot.
    #[error("a direct-call list entry requires a callsign")]
    EmptyCallsign,
    /// The memo cannot be represented losslessly by the radio.
    #[error("invalid callsign-list memo: {0}")]
    InvalidMemo(CallsignListTextError),
}

/// One validated row in the radio's 300-entry D-STAR callsign list.
///
/// Kenwood's TSV and MCP model contain all three fields in this order:
/// name, destination callsign, and memo. Empty names and memos are valid; the
/// destination callsign must be nonempty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallsignEntry {
    name: CallsignListName,
    callsign: DstarCallsign,
    memo: CallsignListMemo,
}

impl CallsignEntry {
    /// Construct one D-STAR destination-list entry.
    ///
    /// Arguments follow the on-disk column order: `Name`, `Callsign`, `Memo`.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignEntryError`] if any field cannot be represented
    /// losslessly or if `callsign` is empty or invalid.
    pub fn new(name: &str, callsign: &str, memo: &str) -> Result<Self, CallsignEntryError> {
        let name = CallsignListName::new(name).map_err(CallsignEntryError::InvalidName)?;
        let callsign = DstarCallsign::new(callsign).map_err(CallsignEntryError::InvalidCallsign)?;
        if callsign.as_str().is_empty() {
            return Err(CallsignEntryError::EmptyCallsign);
        }
        let memo = CallsignListMemo::new(memo).map_err(CallsignEntryError::InvalidMemo)?;
        Ok(Self {
            name,
            callsign,
            memo,
        })
    }

    /// Return the validated display name.
    #[must_use]
    pub const fn name(&self) -> &CallsignListName {
        &self.name
    }

    /// Return the validated destination callsign.
    #[must_use]
    pub const fn callsign(&self) -> &DstarCallsign {
        &self.callsign
    }

    /// Return the validated memo.
    #[must_use]
    pub const fn memo(&self) -> &CallsignListMemo {
        &self.memo
    }
}

fn validate_callsign_list_text(value: &str, maximum: usize) -> Result<(), CallsignListTextError> {
    if let Some((offset, character)) = value
        .char_indices()
        .find(|(_, character)| matches!(character, '\0' | '\r' | '\n'))
    {
        return match character {
            '\0' => Err(CallsignListTextError::Nul { offset }),
            '\r' | '\n' => Err(CallsignListTextError::LineTerminator { offset, character }),
            _ => unreachable!("the search admits only NUL and line terminators"),
        };
    }

    let (encoded, _, had_errors) = SHIFT_JIS.encode(value);
    if had_errors {
        return Err(CallsignListTextError::UnrepresentableShiftJis);
    }
    if encoded.len() > maximum {
        return Err(CallsignListTextError::TooLong {
            encoded_len: encoded.len(),
            maximum,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reflector operations
// ---------------------------------------------------------------------------

/// D-STAR reflector operation command.
///
/// Reflector operations are performed by setting specific URCALL values.
/// The TH-D75 provides dedicated menu items for these operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflectorCommand {
    /// Link to a reflector module.
    Link,
    /// Unlink from the current reflector.
    Unlink,
    /// Echo test (transmit and receive back your own audio).
    Echo,
    /// Request reflector status information.
    Info,
    /// Use the currently linked reflector.
    Use,
}

/// Parsed action from an eight-byte D-STAR URCALL field.
///
/// The URCALL field in a D-STAR header can contain either a destination
/// callsign for routing, or a special command for the gateway. This enum
/// represents all possible interpretations.
///
/// # Special URCALL patterns (per DPlus/DCS/DExtra conventions)
///
/// - `"CQCQCQ  "`: Broadcast CQ (no routing)
/// - `"       E"`: Echo test (7 spaces + `E`)
/// - `"       U"`: Unlink from reflector (7 spaces + `U`)
/// - `"       I"`: Request info (7 spaces + `I`)
/// - `"REF001 A"`: Link to reflector REF001, module A
///   (up to 7 chars reflector name + module letter)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UrCallAction {
    /// Broadcast CQ: no special routing.
    Cq,
    /// Echo test: record and play back the transmission.
    Echo,
    /// Unlink: disconnect from the current reflector.
    Unlink,
    /// Request information from the gateway.
    Info,
    /// Link to a reflector and module.
    Link {
        /// Validated reflector name (e.g. "REF001", "XRF012", "DCS003").
        reflector: ReflectorCallsign,
        /// Module letter (A-Z).
        module: Module,
    },
    /// Route to a destination that is not a recognized special command.
    ///
    /// [`Callsign`] deliberately retains all eight receive bytes, including
    /// malformed or non-UTF-8 fields. Rendering may be lossy, but
    /// classification never replaces, truncates, or pads received data.
    Callsign(Callsign),
}

impl UrCallAction {
    /// Classify an exact-width D-STAR URCALL field.
    ///
    /// Special commands are recognized only when all eight wire bytes match
    /// their protocol representation. Any other field remains available as
    /// a lossless [`Callsign`] destination.
    #[must_use]
    pub fn classify(ur_call: Callsign) -> Self {
        Self::classify_wire_bytes(*ur_call.as_bytes())
    }

    /// Classify eight URCALL bytes without decoding them as text first.
    ///
    /// This is the raw receive-boundary form of [`Self::classify`]. Unknown
    /// and malformed values are returned as [`Self::Callsign`] with their
    /// bytes unchanged.
    #[must_use]
    pub fn classify_wire_bytes(bytes: [u8; 8]) -> Self {
        match bytes {
            exact if exact == *b"CQCQCQ  " => Self::Cq,
            exact if exact == *b"       E" => Self::Echo,
            exact if exact == *b"       U" => Self::Unlink,
            exact if exact == *b"       I" => Self::Info,
            other => Self::classify_link(other)
                .unwrap_or_else(|| Self::Callsign(Callsign::from_wire_bytes(other))),
        }
    }

    fn classify_link(bytes: [u8; 8]) -> Option<Self> {
        let [
            first,
            second,
            third,
            fourth,
            fifth,
            sixth,
            seventh,
            module_byte,
        ] = bytes;
        let reflector_bytes = [first, second, third, fourth, fifth, sixth, seventh];
        let known_prefix = matches!(reflector_bytes.get(..3)?, b"REF" | b"XRF" | b"DCS" | b"XLX");
        if !known_prefix || !is_right_padded_reflector_name(reflector_bytes) {
            return None;
        }

        let module = Module::try_from_byte(module_byte).ok()?;
        let reflector_text = std::str::from_utf8(&reflector_bytes).ok()?;
        let reflector =
            ReflectorCallsign::try_from_str(reflector_text.trim_end_matches(' ')).ok()?;
        Some(Self::Link { reflector, module })
    }
}

fn is_right_padded_reflector_name(bytes: [u8; 7]) -> bool {
    let mut reached_padding = false;
    for byte in bytes {
        if byte == b' ' {
            reached_padding = true;
        } else if reached_padding || !byte.is_ascii_alphanumeric() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Destination / route select
// ---------------------------------------------------------------------------

/// D-STAR destination selection method.
///
/// In DR mode, the radio can select destinations from multiple sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestinationSelect {
    /// Select from the repeater list.
    RepeaterList,
    /// Select from the callsign list.
    CallsignList,
    /// Select from TX/RX history.
    History,
    /// Direct callsign input.
    DirectInput,
}

/// D-STAR route selection for gateway linking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteSelect {
    /// Automatic route selection via the gateway.
    Auto,
    /// Use a specific repeater as the gateway destination.
    Specified,
}

// ---------------------------------------------------------------------------
// QSO log entry (D-STAR specific fields)
// ---------------------------------------------------------------------------

/// D-STAR QSO log entry.
///
/// Extends the generic QSO log with D-STAR-specific fields from the
/// 24-column TSV format stored on the SD card at
/// `/KENWOOD/TH-D75/QSO_LOG/`.
#[derive(Debug, Clone, PartialEq)]
pub struct DstarQsoEntry {
    /// TX or RX direction.
    pub direction: QsoDirection,
    /// Source callsign (MYCALL).
    pub caller: DstarCallsign,
    /// Destination callsign (URCALL).
    pub called: DstarCallsign,
    /// RPT1 callsign (link source repeater).
    pub rpt1: DstarCallsign,
    /// RPT2 callsign (link destination repeater).
    pub rpt2: DstarCallsign,
    /// D-STAR slow-data message content.
    pub message: String,
    /// Break-in flag.
    pub break_in: bool,
    /// EMR (emergency) flag.
    pub emr: bool,
    /// Fast data flag.
    pub fast_data: bool,
    /// Remote station latitude (from D-STAR GPS data).
    pub remote_latitude: Option<f64>,
    /// Remote station longitude (from D-STAR GPS data).
    pub remote_longitude: Option<f64>,
    /// Remote station altitude in meters.
    pub remote_altitude: Option<f64>,
    /// Remote station course in degrees.
    pub remote_course: Option<f64>,
    /// Remote station speed in km/h.
    pub remote_speed: Option<f64>,
}

/// Whether a logged QSO was transmitted or received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QsoDirection {
    /// Transmitted by this radio (`TX` in the SD-card QSO log).
    Tx,
    /// Received by this radio (`RX` in the SD-card QSO log).
    Rx,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn urcall_classification_preserves_non_utf8_bytes() {
        let bytes = [b'W', b'1', 0xff, b'A', b'W', b' ', b' ', b' '];
        let action = UrCallAction::classify_wire_bytes(bytes);
        let UrCallAction::Callsign(callsign) = action else {
            unreachable!("opaque URCALL must remain a destination callsign");
        };
        assert_eq!(callsign.as_bytes(), &bytes);
    }

    #[test]
    fn dstar_callsign_valid() -> TestResult {
        let cs = DstarCallsign::new("N0CALL")?;
        assert_eq!(cs.as_str(), "N0CALL");
        Ok(())
    }

    #[test]
    fn dstar_callsign_max_length() -> TestResult {
        let cs = DstarCallsign::new("JR6YPR A")?;
        assert_eq!(cs.as_str(), "JR6YPR A");
        Ok(())
    }

    #[test]
    fn dstar_callsign_too_long() {
        assert!(DstarCallsign::new("123456789").is_err());
    }

    #[test]
    fn dstar_callsign_trims_trailing_spaces() -> TestResult {
        let cs = DstarCallsign::new("N0CALL  ")?;
        assert_eq!(cs.as_str(), "N0CALL");
        Ok(())
    }

    #[test]
    fn dstar_callsign_wire_bytes_padded() -> TestResult {
        let cs = DstarCallsign::new("N0CALL")?;
        let bytes = cs.to_wire_bytes();
        assert_eq!(&bytes, b"N0CALL  ");
        Ok(())
    }

    #[test]
    fn dstar_callsign_flash_bytes_are_nul_padded() -> TestResult {
        let callsign = DstarCallsign::new("CQCQCQ")?;
        let bytes = callsign.to_flash_bytes();
        assert_eq!(bytes, *b"CQCQCQ\0\0");
        assert_eq!(DstarCallsign::try_from_flash_bytes(bytes)?, callsign);
        assert_eq!(
            DstarCallsign::try_from_flash_bytes([0; DstarCallsign::WIRE_LEN])?,
            DstarCallsign::default()
        );
        Ok(())
    }

    #[test]
    fn dstar_callsign_from_wire_bytes() -> TestResult {
        let bytes = *b"JR6YPR B";
        let cs = DstarCallsign::try_from_wire_bytes(bytes)?;
        assert_eq!(cs.as_str(), "JR6YPR B");
        Ok(())
    }

    #[test]
    fn reflector_callsign_converts_without_changing_wire_bytes() -> TestResult {
        let reflector = ReflectorCallsign::try_from_str("REF030")?;
        let callsign = DstarCallsign::try_from(reflector)?;

        assert_eq!(callsign.to_wire_bytes(), *b"REF030  ");
        Ok(())
    }

    #[test]
    fn reflector_callsign_with_cat_delimiter_cannot_become_dstar_callsign() -> TestResult {
        let reflector = ReflectorCallsign::try_from_str("REF,30")?;

        assert!(DstarCallsign::try_from(reflector).is_err());
        Ok(())
    }

    #[test]
    fn dstar_callsign_cqcqcq() {
        let cs = DstarCallsign::cqcqcq();
        assert!(cs.is_cqcqcq());
        assert_eq!(cs.as_str(), "CQCQCQ");
    }

    #[test]
    fn dstar_suffix_valid() -> TestResult {
        let s = DstarSuffix::new("/P")?;
        assert_eq!(s.as_str(), "/P");
        assert_eq!(s.to_wire_bytes(), *b"/P  ");
        Ok(())
    }

    #[test]
    fn reflector_link_suffix_is_module_then_exact_uppercase_l() {
        assert_eq!(
            DstarSuffix::reflector_link(Module::C).to_wire_bytes(),
            *b"CL  "
        );
    }

    #[test]
    fn dstar_identity_converts_to_core_without_changing_wire_bytes() -> TestResult {
        let callsign = DstarCallsign::new("N0CALL")?;
        let borrowed_callsign: Callsign = (&callsign).into();
        let owned_callsign: Callsign = callsign.into();
        assert_eq!(borrowed_callsign.as_bytes(), b"N0CALL  ");
        assert_eq!(owned_callsign.as_bytes(), b"N0CALL  ");

        let empty_callsign = DstarCallsign::new("")?;
        assert!(
            empty_callsign.is_empty(),
            "empty callsign must remain empty"
        );
        let core_empty_callsign: Callsign = empty_callsign.into();
        assert_eq!(core_empty_callsign.as_bytes(), b"        ");
        assert_eq!(core_empty_callsign.text(), Ok(""));

        let suffix = DstarSuffix::new("/P")?;
        let borrowed_suffix: Suffix = (&suffix).into();
        let owned_suffix: Suffix = suffix.into();
        assert_eq!(borrowed_suffix.as_bytes(), b"/P  ");
        assert_eq!(owned_suffix.as_bytes(), b"/P  ");
        assert_eq!(Suffix::from(DstarSuffix::default()), Suffix::EMPTY);
        Ok(())
    }

    #[test]
    fn dstar_identity_preserves_exact_radio_policy_errors() {
        assert!(
            matches!(
                DstarCallsign::new("123456789"),
                Err(ValidationError::CallsignTooLong { len: 9, max: 8 })
            ),
            "overlength callsign must retain its precise error"
        );
        assert!(
            matches!(
                DstarCallsign::new("N0,CALL"),
                Err(ValidationError::InvalidDstarCallsignByte {
                    field: "callsign",
                    offset: 2,
                    value: b',',
                })
            ),
            "CAT delimiter must retain its byte offset"
        );
        assert!(
            matches!(
                DstarSuffix::new("P\n"),
                Err(ValidationError::InvalidDstarCallsignByte {
                    field: "suffix",
                    offset: 1,
                    value: b'\n',
                })
            ),
            "suffix control byte must retain its byte offset"
        );
        assert!(
            matches!(
                DstarCallsign::try_from_flash_bytes(*b"A\0B\0\0\0\0\0"),
                Err(ValidationError::InvalidDstarCallsignPadding {
                    field: "callsign",
                    offset: 2,
                    value: b'B',
                })
            ),
            "non-NUL data after the flash terminator must retain its offset"
        );
    }

    #[test]
    fn dstar_suffix_too_long() {
        assert!(DstarSuffix::new("12345").is_err());
    }

    #[test]
    fn dstar_identity_rejects_cat_delimiters_controls_and_non_ascii() {
        for value in ["N0,CALL", "N0\rCALL", "N0\nCALL", "NØCALL"] {
            assert!(
                DstarCallsign::new(value).is_err(),
                "invalid D-STAR callsign accepted: {value:?}"
            );
        }
        for value in ["C,L", "C\r", "C\n", "CØ"] {
            assert!(
                DstarSuffix::new(value).is_err(),
                "invalid D-STAR suffix accepted: {value:?}"
            );
        }
    }

    #[test]
    fn dstar_identity_rejects_invalid_wire_bytes() {
        assert!(DstarCallsign::try_from_wire_bytes(*b"N0\rCALL ").is_err());
        assert!(DstarCallsign::try_from_wire_bytes([0xFF; 8]).is_err());
        assert!(DstarSuffix::try_from_wire_bytes(*b"C,L ").is_err());
        assert!(DstarCallsign::try_from_flash_bytes(*b"N0\0CALL ").is_err());
        assert!(DstarCallsign::try_from_flash_bytes([0xFF; 8]).is_err());
    }

    #[test]
    fn dstar_gps_data_tx_sentences_enforce_menu_constraints() -> TestResult {
        let factory = DstarGpsDataTxSentences::factory_default();
        assert_eq!(factory.bits(), 0x11);
        assert!(factory.contains(DstarGpsDataTxSentence::Gga));
        assert!(factory.contains(DstarGpsDataTxSentence::Rmc));

        assert_eq!(DstarGpsDataTxSentences::try_from(0)?.bits(), 0);
        assert_eq!(DstarGpsDataTxSentences::try_from(0x0F)?.bits(), 0x0F);
        assert!(DstarGpsDataTxSentences::try_from(0x1F).is_err());
        assert_eq!(DstarGpsDataTxSentences::try_from(0x40)?.bits(), 0x40);
        assert!(DstarGpsDataTxSentences::try_from(0x41).is_err());
        assert!(DstarGpsDataTxSentences::try_from(0x80).is_err());
        assert!(factory.with(DstarGpsDataTxSentence::Aprs).is_err());
        Ok(())
    }

    #[test]
    fn dstar_gps_auto_tx_uses_documented_discrete_domain() -> TestResult {
        let intervals = [
            (DstarGpsAutoTxInterval::Off, None),
            (DstarGpsAutoTxInterval::Seconds12, Some(12)),
            (DstarGpsAutoTxInterval::Seconds30, Some(30)),
            (DstarGpsAutoTxInterval::OneMinute, Some(60)),
            (DstarGpsAutoTxInterval::TwoMinutes, Some(120)),
            (DstarGpsAutoTxInterval::ThreeMinutes, Some(180)),
            (DstarGpsAutoTxInterval::FiveMinutes, Some(300)),
            (DstarGpsAutoTxInterval::TenMinutes, Some(600)),
            (DstarGpsAutoTxInterval::TwentyMinutes, Some(1200)),
            (DstarGpsAutoTxInterval::ThirtyMinutes, Some(1800)),
            (DstarGpsAutoTxInterval::SixtyMinutes, Some(3600)),
        ];
        for (raw, (expected, seconds)) in (0_u8..=10).zip(intervals) {
            assert_eq!(DstarGpsAutoTxInterval::try_from(raw)?, expected);
            assert_eq!(u8::from(expected), raw);
            assert_eq!(expected.as_seconds(), seconds);
        }
        assert!(DstarGpsAutoTxInterval::try_from(11).is_err());
        Ok(())
    }

    #[test]
    fn dstar_gps_data_tx_factory_default_models_all_three_fields() {
        let data_tx = DstarGpsDataTxSettings::factory_default();
        assert!(!data_tx.enabled());
        assert_eq!(
            data_tx.sentences(),
            DstarGpsDataTxSentences::factory_default()
        );
        assert_eq!(data_tx.auto_tx(), DstarGpsAutoTxInterval::Off);
    }

    #[test]
    fn emr_volume_valid_range() -> Result<(), ValidationError> {
        for i in EmrVolume::MIN..=EmrVolume::MAX {
            assert!(EmrVolume::new(i).is_ok());
        }
        assert_eq!(EmrVolume::new(EmrVolume::MIN)?.as_raw(), 1);
        assert_eq!(EmrVolume::new(EmrVolume::MAX)?.as_raw(), 50);
        Ok(())
    }

    #[test]
    fn emr_volume_rejects_values_outside_stock_domain() {
        assert!(EmrVolume::new(0).is_err());
        assert!(EmrVolume::new(51).is_err());
        assert!(EmrVolume::new(u8::MAX).is_err());
    }

    #[test]
    fn emr_volume_factory_default_is_level_twenty_five() {
        assert_eq!(
            EmrVolume::factory_default().as_raw(),
            EmrVolume::FACTORY_DEFAULT_LEVEL
        );
    }

    #[test]
    fn dstar_message_accepts_printable_ascii() -> TestResult {
        let msg = DstarMessage::new(" A B~")?;
        assert_eq!(msg.as_str(), " A B~");
        assert_eq!(
            msg.as_wire_bytes(),
            b" A B~               ",
            "short text must use exact space padding"
        );
        let core_message: SlowDataTextMessage = msg.into();
        assert_eq!(core_message.as_bytes(), b" A B~               ");
        Ok(())
    }

    #[test]
    fn dstar_message_accepts_exact_twenty_byte_boundary() -> TestResult {
        let msg = DstarMessage::try_from("ABCDEFGHIJKLMNOPQRST")?;
        assert_eq!(msg.as_str(), "ABCDEFGHIJKLMNOPQRST");
        assert_eq!(msg.as_wire_bytes(), b"ABCDEFGHIJKLMNOPQRST");
        Ok(())
    }

    #[test]
    fn dstar_message_default_is_empty_transmit_text() {
        let msg = DstarMessage::default();
        assert_eq!(msg.as_str(), "");
        assert_eq!(msg.as_wire_bytes(), b"                    ");
    }

    #[test]
    fn dstar_message_rejects_overlength_without_truncation() {
        let text = "a".repeat(DstarMessage::MAX_LEN + 1);
        assert_eq!(
            DstarMessage::new(&text),
            Err(DstarMessageError::TooLong {
                length: 21,
                maximum: DstarMessage::MAX_LEN,
            })
        );
    }

    #[test]
    fn dstar_message_rejects_control_bytes_at_exact_offset() {
        assert_eq!(
            DstarMessage::new("A\nB"),
            Err(DstarMessageError::InvalidText(
                dstar_gateway_core::WireTextError {
                    index: 1,
                    byte: b'\n',
                }
            ))
        );
    }

    #[test]
    fn dstar_message_rejects_non_ascii_at_first_utf8_byte() {
        assert_eq!(
            DstarMessage::new("Aé"),
            Err(DstarMessageError::InvalidText(
                dstar_gateway_core::WireTextError {
                    index: 1,
                    byte: 0xC3,
                }
            ))
        );
    }

    #[test]
    fn dstar_message_try_from_uses_the_same_validation() -> TestResult {
        let msg = DstarMessage::try_from("Hello D-STAR")?;
        assert_eq!(msg.as_str(), "Hello D-STAR");
        Ok(())
    }

    #[test]
    fn digital_squelch_default() {
        let sq = DigitalSquelch::default();
        assert_eq!(sq.squelch_type, DigitalSquelchType::Off);
        assert_eq!(sq.code.as_raw(), 0);
    }

    // -----------------------------------------------------------------------
    // UrCallAction tests
    // -----------------------------------------------------------------------

    #[test]
    fn urcall_cq() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"CQCQCQ  "),
            UrCallAction::Cq
        );
    }

    #[test]
    fn urcall_echo() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"       E"),
            UrCallAction::Echo
        );
    }

    #[test]
    fn urcall_unlink() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"       U"),
            UrCallAction::Unlink
        );
    }

    #[test]
    fn urcall_info() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"       I"),
            UrCallAction::Info
        );
    }

    #[test]
    fn urcall_link_ref() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"REF001 A");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid REF command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("REF001")?);
        assert_eq!(module, Module::A);
        Ok(())
    }

    #[test]
    fn urcall_link_xrf() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"XRF012 C");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid XRF command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("XRF012")?);
        assert_eq!(module, Module::C);
        Ok(())
    }

    #[test]
    fn urcall_link_dcs() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"DCS003 B");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid DCS command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("DCS003")?);
        assert_eq!(module, Module::B);
        Ok(())
    }

    #[test]
    fn urcall_link_xlx() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"XLX999 A");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid XLX command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("XLX999")?);
        assert_eq!(module, Module::A);
        Ok(())
    }

    #[test]
    fn urcall_link_accepts_seven_byte_reflector_name() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"REF1234A");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("seven-byte reflector name must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("REF1234")?);
        assert_eq!(module, Module::A);
        Ok(())
    }

    #[test]
    fn urcall_callsign() {
        let action = UrCallAction::classify_wire_bytes(*b"W1AW    ");
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        );
    }

    #[test]
    fn urcall_unknown_single_char() {
        let bytes = *b"       X";
        let action = UrCallAction::classify_wire_bytes(bytes);
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
        );
    }

    #[test]
    fn urcall_near_match_is_not_fabricated_into_cq() {
        let bytes = *b"CQCQCQ X";
        let action = UrCallAction::classify_wire_bytes(bytes);
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
        );
    }

    #[test]
    fn urcall_malformed_reflector_command_remains_lossless() {
        let bytes = [b'R', b'E', b'F', 0, b'0', b'1', b' ', b'A'];
        let action = UrCallAction::classify_wire_bytes(bytes);
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
        );
    }

    #[test]
    fn urcall_link_rejects_internal_padding_and_lowercase_module() {
        for bytes in [*b"REF 01 A", *b"REF001 a"] {
            let action = UrCallAction::classify_wire_bytes(bytes);
            assert_eq!(
                action,
                UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
            );
        }
    }

    #[test]
    fn urcall_classifies_lossless_callsign_value() {
        let callsign = Callsign::from_wire_bytes(*b"       U");
        assert_eq!(UrCallAction::classify(callsign), UrCallAction::Unlink);
    }
}
