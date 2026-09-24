//! Per-band and global settings reachable over CAT.
//!
//! Each bounded domain below was established on firmware 1.02 by writing every
//! value in it, reading it back, and observing the rejection of the next value.
//! Value labels follow the corresponding menu's list in the User Manual; where
//! a label was not confirmed by the radio's own behavior the type says so.

use std::fmt;

use crate::error::ValidationError;

/// Transmit power selected and reported by `PC`.
///
/// Three values are accepted (`3` is answered `N`); the panel's `[LOW]` key
/// cycles High, Medium and Low (User Manual, Selecting an Output Power). The
/// assignment of `0`, `1` and `2` to those labels was not read from the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerLevel {
    /// Highest power (`0`).
    High = 0,
    /// Medium power (`1`).
    Medium = 1,
    /// Low power (`2`).
    Low = 2,
}

impl PowerLevel {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for PowerLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::High),
            1 => Ok(Self::Medium),
            2 => Ok(Self::Low),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: "power level",
                value: u64::from(value),
                max: 2,
            }),
        }
    }
}

impl fmt::Display for PowerLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        })
    }
}

/// Squelch level selected and reported by `SQ`, `0` (open) through `31`.
///
/// The panel's `[SQL]` control changes the same value that `SQ` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SquelchLevel(u8);

impl SquelchLevel {
    /// Open squelch.
    pub const OPEN: Self = Self(0);
    /// Highest level.
    pub const MAX: u8 = 31;

    /// Validate a level.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "squelch level",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The level.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for SquelchLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Signal strength reported by `SM`.
///
/// Readings from firmware 1.02 ran `0` through `9`; `9` was reported on a
/// full-quieting NOAA weather broadcast. The top of the panel scale is not
/// documented, so any decimal reading is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SMeterReading(u8);

impl SMeterReading {
    /// No signal.
    pub const ZERO: Self = Self(0);
    /// Largest reading observed on firmware 1.02.
    pub const OBSERVED_MAX: u8 = 9;

    /// Wrap a reading.
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// The reading.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for SMeterReading {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "S{}", self.0)
    }
}

/// AM receive high-cut filter selected and reported by `SH 0` (User Manual,
/// Menu 120).
///
/// The bench radio read `2`; the list order is the manual's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmHighCut {
    /// 3.0 kHz (`0`).
    Hz3000 = 0,
    /// 4.5 kHz (`1`).
    Hz4500 = 1,
    /// 6.0 kHz (`2`).
    Hz6000 = 2,
    /// 7.5 kHz (`3`).
    Hz7500 = 3,
}

impl AmHighCut {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }

    /// The cutoff in hertz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        match self {
            Self::Hz3000 => 3_000,
            Self::Hz4500 => 4_500,
            Self::Hz6000 => 6_000,
            Self::Hz7500 => 7_500,
        }
    }
}

impl TryFrom<u8> for AmHighCut {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hz3000),
            1 => Ok(Self::Hz4500),
            2 => Ok(Self::Hz6000),
            3 => Ok(Self::Hz7500),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: "AM high cut",
                value: u64::from(value),
                max: 3,
            }),
        }
    }
}

impl fmt::Display for AmHighCut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hz = self.as_hz();
        write!(formatter, "{}.{} kHz", hz / 1000, (hz % 1000) / 100)
    }
}

/// VOX delay selected and reported by `VD` (User Manual, Menu 152).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VoxDelay(u8);

impl VoxDelay {
    /// Highest index.
    pub const MAX: u8 = 6;
    /// Delay of each index in milliseconds.
    pub const MILLISECONDS: [u16; 7] = [250, 500, 750, 1000, 1500, 2000, 3000];

    /// Validate an index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "VOX delay",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// The delay in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
        match self.0 {
            0 => 250,
            1 => 500,
            2 => 750,
            3 => 1000,
            4 => 1500,
            5 => 2000,
            _ => 3000,
        }
    }
}

impl fmt::Display for VoxDelay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ms", self.as_milliseconds())
    }
}

/// VOX gain selected and reported by `VG`, `0` through `9` (User Manual,
/// Menu 151).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VoxGain(u8);

impl VoxGain {
    /// Highest gain.
    pub const MAX: u8 = 9;

    /// Validate a gain.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "VOX gain",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The gain.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for VoxGain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// VOX state reported by `VX`.
///
/// Only `0` has been read from the radio; the crate exposes no `VX` write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoxMode {
    /// VOX off (`0`).
    Off,
    /// Any other reported value, retained exactly as received.
    Unqualified(u8),
}

impl From<u8> for VoxMode {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Off,
            other => Self::Unqualified(other),
        }
    }
}

impl From<VoxMode> for u8 {
    fn from(value: VoxMode) -> Self {
        match value {
            VoxMode::Off => 0,
            VoxMode::Unqualified(raw) => raw,
        }
    }
}

impl fmt::Display for VoxMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => formatter.write_str("off"),
            Self::Unqualified(value) => write!(formatter, "unqualified value {value}"),
        }
    }
}

/// Panel lighting setting selected and reported by `LC`, `0` through `3`.
///
/// The four values were written and read back; the panel behavior each one
/// selects was not observed, and the radio's own setting read `3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BacklightControl(u8);

impl BacklightControl {
    /// Highest value.
    pub const MAX: u8 = 3;

    /// Validate a value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "backlight control",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for BacklightControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Dual or single band display selected and reported by `DL` (User Manual,
/// Selecting Dual Band/Single Band Mode).
///
/// The radio's ordinary setting reads `0`; the panel was not observed while
/// `1` was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandDisplay {
    /// Both bands displayed (`0`).
    Dual = 0,
    /// One band displayed (`1`).
    Single = 1,
}

impl BandDisplay {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for BandDisplay {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dual),
            1 => Ok(Self::Single),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: "band display",
                value: u64::from(value),
                max: 1,
            }),
        }
    }
}

impl fmt::Display for BandDisplay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Dual => "dual band",
            Self::Single => "single band",
        })
    }
}

/// APRS position source selected and reported by `MS`: `0` for the GPS, `1`
/// through `5` for the five stored positions (User Manual, Menu 401).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MyPositionSelection(u8);

impl MyPositionSelection {
    /// The GPS receiver.
    pub const GPS: Self = Self(0);
    /// Highest stored position number.
    pub const MAX: u8 = 5;

    /// Validate a selection.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "my position selection",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for MyPositionSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => formatter.write_str("GPS"),
            number => write!(formatter, "My Position {number}"),
        }
    }
}

/// D-STAR MY callsign slot selected by `DS` and addressed by `DC`, `1`
/// through `6` (User Manual, Menu 610).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DstarSlot(u8);

impl DstarSlot {
    /// First slot.
    pub const MIN: u8 = 1;
    /// Last slot.
    pub const MAX: u8 = 6;

    /// Validate a slot number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] below [`Self::MIN`]
    /// or above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value < Self::MIN || value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "D-STAR callsign slot",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The slot number.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for DstarSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MY{}", self.0)
    }
}

/// One D-STAR MY callsign slot as `DC` reports it: the callsign and the
/// four-character memo, both empty on an unset slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DstarCallsignEntry {
    /// Slot number.
    pub slot: DstarSlot,
    /// Callsign, up to eight characters.
    pub callsign: String,
    /// Memo, up to four characters.
    pub memo: String,
}

impl DstarCallsignEntry {
    /// Maximum callsign length in bytes.
    pub const MAX_CALLSIGN_LEN: usize = 8;
    /// Maximum memo length in bytes.
    pub const MAX_MEMO_LEN: usize = 4;

    /// Validate an entry.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCallsignText`] for an overlong field
    /// or a byte outside graphic ASCII and space.
    pub fn new(slot: DstarSlot, callsign: &str, memo: &str) -> Result<Self, ValidationError> {
        let valid = |text: &str, max: usize| {
            text.len() <= max
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        };
        if !valid(callsign, Self::MAX_CALLSIGN_LEN) {
            return Err(ValidationError::InvalidCallsignText {
                text: callsign.to_owned(),
            });
        }
        if !valid(memo, Self::MAX_MEMO_LEN) {
            return Err(ValidationError::InvalidCallsignText {
                text: memo.to_owned(),
            });
        }
        Ok(Self {
            slot,
            callsign: callsign.to_owned(),
            memo: memo.to_owned(),
        })
    }

    /// An unset slot: empty callsign and memo.
    ///
    /// Writing this entry sends `DC <slot>,,`, which clears the slot; reading
    /// an unset slot returns the same empty callsign and memo.
    #[must_use]
    pub const fn empty(slot: DstarSlot) -> Self {
        Self {
            slot,
            callsign: String::new(),
            memo: String::new(),
        }
    }

    /// Whether the slot holds a callsign.
    ///
    /// True exactly when the callsign is nonempty; the memo is not consulted.
    #[must_use]
    pub const fn is_set(&self) -> bool {
        !self.callsign.is_empty()
    }
}

impl fmt::Display for DstarCallsignEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.callsign.is_empty() {
            write!(formatter, "{} unset", self.slot)
        } else if self.memo.is_empty() {
            write!(formatter, "{} {}", self.slot, self.callsign)
        } else {
            write!(formatter, "{} {} ({})", self.slot, self.callsign, self.memo)
        }
    }
}

/// APRS My Callsign as `CS` reports and accepts it.
///
/// A base callsign of one to six uppercase ASCII letters and digits, with an
/// optional SSID of 1 to 15 written `-N`. SSID 0 is the bare callsign; the
/// radio rejects `-0`, a lowercase base, a base over six characters, and an
/// SSID over 15. An unconfigured slot reads back as the literal `NOCALL`,
/// which is itself a valid value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AprsCallsign(String);

impl AprsCallsign {
    /// Maximum encoded length: a six-character base plus `-15`.
    pub const MAX_LEN: usize = 9;

    /// Validate an APRS My Callsign.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCallsignText`] unless `text` is a base
    /// of one to six uppercase ASCII letters and digits, optionally followed by
    /// `-` and a canonical SSID of 1 to 15.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        if Self::is_canonical(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(ValidationError::InvalidCallsignText {
                text: text.to_owned(),
            })
        }
    }

    fn is_canonical(text: &str) -> bool {
        let (base, ssid) = match text.split_once('-') {
            Some((base, ssid)) => (base, Some(ssid)),
            None => (text, None),
        };
        let base_ok = (1..=6).contains(&base.len())
            && base
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
        let ssid_ok = ssid.is_none_or(|digits| {
            digits
                .parse::<u8>()
                .is_ok_and(|value| (1..=15).contains(&value) && digits == value.to_string())
        });
        base_ok && ssid_ok
    }

    /// The callsign exactly as carried on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AprsCallsign {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Packet data speed selected and reported by `AS` (User Manual, Menu 505).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketDataRate {
    /// 1200 bps (`0`).
    Bps1200 = 0,
    /// 9600 bps (`1`).
    Bps9600 = 1,
}

impl PacketDataRate {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for PacketDataRate {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Bps1200),
            1 => Ok(Self::Bps9600),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: "packet data rate",
                value: u64::from(value),
                max: 1,
            }),
        }
    }
}

impl fmt::Display for PacketDataRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bps1200 => "1200 bps",
            Self::Bps9600 => "9600 bps",
        })
    }
}

/// APRS beacon transmission method selected and reported by `PT` (User
/// Manual, Menu 510).
///
/// With the TNC in APRS mode, `Auto` and `SmartBeaconing` transmit without
/// further operator action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconMethod {
    /// Manual (`0`).
    Manual = 0,
    /// PTT (`1`).
    Ptt = 1,
    /// Auto (`2`).
    Auto = 2,
    /// `SmartBeaconing` (`3`).
    SmartBeaconing = 3,
}

impl BeaconMethod {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for BeaconMethod {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Manual),
            1 => Ok(Self::Ptt),
            2 => Ok(Self::Auto),
            3 => Ok(Self::SmartBeaconing),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: "beacon method",
                value: u64::from(value),
                max: 3,
            }),
        }
    }
}

impl fmt::Display for BeaconMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Manual => "Manual",
            Self::Ptt => "PTT",
            Self::Auto => "Auto",
            Self::SmartBeaconing => "SmartBeaconing",
        })
    }
}

/// One NMEA sentence the GPS can emit, in `GS` wire order (User Manual,
/// Menu 405).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NmeaSentence {
    /// `$GPGGA`, the first `GS` field.
    Gga = 0,
    /// `$GPGLL`.
    Gll = 1,
    /// `$GPGSA`.
    Gsa = 2,
    /// `$GPGSV`.
    Gsv = 3,
    /// `$GPRMC`.
    Rmc = 4,
    /// `$GPVTG`, the last `GS` field.
    Vtg = 5,
}

impl NmeaSentence {
    /// Every sentence in wire order.
    pub const ALL: [Self; 6] = [
        Self::Gga,
        Self::Gll,
        Self::Gsa,
        Self::Gsv,
        Self::Rmc,
        Self::Vtg,
    ];

    /// The sentence name without its `$GP` prefix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gga => "GGA",
            Self::Gll => "GLL",
            Self::Gsa => "GSA",
            Self::Gsv => "GSV",
            Self::Rmc => "RMC",
            Self::Vtg => "VTG",
        }
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

impl fmt::Display for NmeaSentence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The set of NMEA sentences the GPS emits, selected and reported by `GS`
/// as six flags in [`NmeaSentence`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct NmeaSentences(u8);

impl NmeaSentences {
    /// No sentences.
    pub const NONE: Self = Self(0);

    /// Whether `sentence` is selected.
    #[must_use]
    pub const fn contains(self, sentence: NmeaSentence) -> bool {
        self.0 & sentence.bit() != 0
    }

    /// The set with `sentence` selected.
    #[must_use]
    pub const fn with(self, sentence: NmeaSentence) -> Self {
        Self(self.0 | sentence.bit())
    }

    /// The set with `sentence` deselected.
    #[must_use]
    pub const fn without(self, sentence: NmeaSentence) -> Self {
        Self(self.0 & !sentence.bit())
    }

    /// The six flags in wire order.
    #[must_use]
    pub const fn to_flags(self) -> [bool; 6] {
        [
            self.contains(NmeaSentence::Gga),
            self.contains(NmeaSentence::Gll),
            self.contains(NmeaSentence::Gsa),
            self.contains(NmeaSentence::Gsv),
            self.contains(NmeaSentence::Rmc),
            self.contains(NmeaSentence::Vtg),
        ]
    }

    /// Build from the six flags in wire order.
    #[must_use]
    pub const fn from_flags([gga, gll, gsa, gsv, rmc, vtg]: [bool; 6]) -> Self {
        let selections = [
            (gga, NmeaSentence::Gga),
            (gll, NmeaSentence::Gll),
            (gsa, NmeaSentence::Gsa),
            (gsv, NmeaSentence::Gsv),
            (rmc, NmeaSentence::Rmc),
            (vtg, NmeaSentence::Vtg),
        ];
        let mut set = Self::NONE;
        let mut remaining: &[(bool, NmeaSentence)] = &selections;
        while let [(selected, sentence), rest @ ..] = remaining {
            if *selected {
                set = set.with(*sentence);
            }
            remaining = rest;
        }
        set
    }

    /// The selected sentences in wire order.
    pub fn iter(self) -> impl Iterator<Item = NmeaSentence> {
        NmeaSentence::ALL
            .into_iter()
            .filter(move |sentence| self.contains(*sentence))
    }
}

impl FromIterator<NmeaSentence> for NmeaSentences {
    fn from_iter<I: IntoIterator<Item = NmeaSentence>>(sentences: I) -> Self {
        sentences.into_iter().fold(Self::NONE, Self::with)
    }
}

impl fmt::Display for NmeaSentences {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let enabled: Vec<&str> = self.iter().map(NmeaSentence::as_str).collect();
        if enabled.is_empty() {
            formatter.write_str("none")
        } else {
            formatter.write_str(&enabled.join(","))
        }
    }
}

/// GPS receiver settings selected and reported by `GP`: the built-in
/// receiver (Menu 400) and its PC output (Menu 403).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpsSettings {
    /// Whether the built-in GPS receiver is on.
    pub gps_enabled: bool,
    /// Whether GPS data is output to the PC.
    pub pc_output: bool,
}

impl fmt::Display for GpsSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GPS {}, PC output {}",
            if self.gps_enabled { "on" } else { "off" },
            if self.pc_output { "on" } else { "off" }
        )
    }
}

/// TNC mode reported by `TN`.
///
/// Only `0` has been read from the radio; the crate exposes no `TN` write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TncMode {
    /// TNC off (`0`).
    Off,
    /// Any other reported value, retained exactly as received.
    Unqualified(u8),
}

impl From<u8> for TncMode {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Off,
            other => Self::Unqualified(other),
        }
    }
}

impl From<TncMode> for u8 {
    fn from(value: TncMode) -> Self {
        match value {
            TncMode::Off => 0,
            TncMode::Unqualified(raw) => raw,
        }
    }
}

impl fmt::Display for TncMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => formatter.write_str("off"),
            Self::Unqualified(value) => write!(formatter, "unqualified value {value}"),
        }
    }
}

/// The radio's clock as `RT` reports it: twelve digits `YYMMDDHHMMSS` in
/// the radio's local time, with the year counted from 2000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RealTimeClock {
    /// Full year, 2000 through 2099.
    pub year: u16,
    /// Month, 1 through 12.
    pub month: u8,
    /// Day of month, 1 through 31.
    pub day: u8,
    /// Hour, 0 through 23.
    pub hour: u8,
    /// Minute, 0 through 59.
    pub minute: u8,
    /// Second, 0 through 59.
    pub second: u8,
}

impl RealTimeClock {
    /// Number of wire digits.
    pub const WIRE_DIGITS: usize = 12;

    /// Validate a calendar time.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] naming the first field
    /// outside its range. Days are checked against 31 only.
    pub const fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, ValidationError> {
        if year < 2000 || year > 2099 {
            return Err(ValidationError::SettingOutOfRange {
                setting: "clock year",
                value: year as u64,
                max: 2099,
            });
        }
        if month < 1 || month > 12 {
            return Err(ValidationError::SettingOutOfRange {
                setting: "clock month",
                value: month as u64,
                max: 12,
            });
        }
        if day < 1 || day > 31 {
            return Err(ValidationError::SettingOutOfRange {
                setting: "clock day",
                value: day as u64,
                max: 31,
            });
        }
        if hour > 23 {
            return Err(ValidationError::SettingOutOfRange {
                setting: "clock hour",
                value: hour as u64,
                max: 23,
            });
        }
        if minute > 59 {
            return Err(ValidationError::SettingOutOfRange {
                setting: "clock minute",
                value: minute as u64,
                max: 59,
            });
        }
        if second > 59 {
            return Err(ValidationError::SettingOutOfRange {
                setting: "clock second",
                value: second as u64,
                max: 59,
            });
        }
        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// Parse the twelve-digit wire field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidWireDigits`] unless the text is
    /// exactly twelve ASCII digits, then the error of [`Self::new`].
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        if text.len() != Self::WIRE_DIGITS || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ValidationError::InvalidWireDigits {
                text: text.to_owned(),
                digits: Self::WIRE_DIGITS,
            });
        }
        let mut pairs = text.as_bytes().chunks_exact(2).map(|pair| {
            pair.iter()
                .fold(0_u8, |value, byte| value * 10 + (byte - b'0'))
        });
        let mut next = || pairs.next().unwrap_or_default();
        let year = 2000 + u16::from(next());
        let (month, day, hour, minute, second) = (next(), next(), next(), next(), next());
        Self::new(year, month, day, hour, minute, second)
    }

    /// The twelve-digit wire field.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        format!(
            "{:02}{:02}{:02}{:02}{:02}{:02}",
            self.year - 2000,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        )
    }
}

impl fmt::Display for RealTimeClock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// The serial number and model code reported by `AE`.
///
/// Both fields are graphic ASCII: an eight-character serial number and a
/// three-character code whose meaning the manuals do not document (`K01` on
/// the North American unit).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SerialInformation {
    serial_number: String,
    model_code: String,
}

impl SerialInformation {
    /// Serial number length in bytes.
    pub const SERIAL_LEN: usize = 8;
    /// Model code length in bytes.
    pub const MODEL_CODE_LEN: usize = 3;

    /// Validate both fields.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidSerialText`] for a field of the
    /// wrong length or with a byte outside graphic ASCII.
    pub fn new(serial_number: &str, model_code: &str) -> Result<Self, ValidationError> {
        let valid = |text: &str, len: usize| {
            text.len() == len && text.bytes().all(|byte| byte.is_ascii_graphic())
        };
        if !valid(serial_number, Self::SERIAL_LEN) {
            return Err(ValidationError::InvalidSerialText {
                text: serial_number.to_owned(),
            });
        }
        if !valid(model_code, Self::MODEL_CODE_LEN) {
            return Err(ValidationError::InvalidSerialText {
                text: model_code.to_owned(),
            });
        }
        Ok(Self {
            serial_number: serial_number.to_owned(),
            model_code: model_code.to_owned(),
        })
    }

    /// The serial number.
    #[must_use]
    pub fn serial_number(&self) -> &str {
        &self.serial_number
    }

    /// The model code.
    #[must_use]
    pub fn model_code(&self) -> &str {
        &self.model_code
    }
}

impl fmt::Display for SerialInformation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.serial_number, self.model_code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn bounded_settings_reject_the_first_value_past_their_domain() -> TestResult {
        assert_eq!(PowerLevel::try_from(2)?, PowerLevel::Low);
        assert!(PowerLevel::try_from(3).is_err());
        assert_eq!(SquelchLevel::new(31)?.as_raw(), 31);
        assert!(SquelchLevel::new(32).is_err());
        assert_eq!(AmHighCut::try_from(3)?.to_string(), "7.5 kHz");
        assert!(AmHighCut::try_from(4).is_err());
        assert_eq!(VoxDelay::new(6)?.as_milliseconds(), 3000);
        assert_eq!(VoxDelay::new(1)?.to_string(), "500 ms");
        assert!(VoxDelay::new(7).is_err());
        assert_eq!(VoxGain::new(9)?.as_raw(), 9);
        assert!(VoxGain::new(10).is_err());
        assert_eq!(BacklightControl::new(3)?.as_raw(), 3);
        assert!(BacklightControl::new(4).is_err());
        assert_eq!(BandDisplay::try_from(1)?, BandDisplay::Single);
        assert!(BandDisplay::try_from(2).is_err());
        assert_eq!(MyPositionSelection::new(5)?.to_string(), "My Position 5");
        assert_eq!(MyPositionSelection::GPS.to_string(), "GPS");
        assert!(MyPositionSelection::new(6).is_err());
        assert_eq!(DstarSlot::new(6)?.to_string(), "MY6");
        assert!(DstarSlot::new(0).is_err());
        assert!(DstarSlot::new(7).is_err());
        assert_eq!(PacketDataRate::try_from(1)?, PacketDataRate::Bps9600);
        assert!(PacketDataRate::try_from(2).is_err());
        assert_eq!(BeaconMethod::try_from(3)?, BeaconMethod::SmartBeaconing);
        assert!(BeaconMethod::try_from(4).is_err());
        assert_eq!(SMeterReading::new(9).to_string(), "S9");
        assert_eq!(VoxMode::from(2), VoxMode::Unqualified(2));
        assert_eq!(u8::from(VoxMode::Unqualified(2)), 2);
        assert_eq!(TncMode::from(1), TncMode::Unqualified(1));
        assert_eq!(u8::from(TncMode::Off), 0);
        Ok(())
    }

    #[test]
    fn callsign_entries_bound_both_fields() -> TestResult {
        let entry = DstarCallsignEntry::new(DstarSlot::new(1)?, "", "")?;
        assert_eq!(entry.to_string(), "MY1 unset");
        let entry = DstarCallsignEntry::new(DstarSlot::new(2)?, "W1AW", "D750")?;
        assert_eq!(entry.to_string(), "MY2 W1AW (D750)");
        assert!(DstarCallsignEntry::new(DstarSlot::new(1)?, "W1AWW1AWW", "").is_err());
        assert!(DstarCallsignEntry::new(DstarSlot::new(1)?, "W1AW", "D750A").is_err());
        assert!(DstarCallsignEntry::new(DstarSlot::new(1)?, "W1\rAW", "").is_err());
        let unset = DstarCallsignEntry::empty(DstarSlot::new(3)?);
        assert_eq!(unset, DstarCallsignEntry::new(DstarSlot::new(3)?, "", "")?);
        assert!(!unset.is_set());
        assert!(DstarCallsignEntry::new(DstarSlot::new(3)?, "W1AW", "")?.is_set());
        assert!(!DstarCallsignEntry::new(DstarSlot::new(3)?, "", "MEMO")?.is_set());
        Ok(())
    }

    #[test]
    fn aprs_callsign_accepts_the_radio_domain_and_rejects_the_rest() -> TestResult {
        for text in ["NOCALL", "KQ4NIT", "A", "AB1CD", "KQ4NIT-1", "KQ4NIT-15"] {
            let callsign = AprsCallsign::new(text)?;
            assert_eq!(callsign.as_str(), text);
            assert_eq!(callsign.to_string(), text);
        }
        assert_eq!(
            AprsCallsign::new("KQ4NIT-15")?.as_str().len(),
            AprsCallsign::MAX_LEN
        );
        for text in [
            "",
            "kq4nit",
            "ABCDEFG",
            "KQ4NIT-0",
            "KQ4NIT-16",
            "KQ4NIT-",
            "KQ4NIT-05",
            "KQ4-NIT",
        ] {
            assert!(AprsCallsign::new(text).is_err(), "{text} must be rejected");
        }
        Ok(())
    }

    #[test]
    fn vox_delay_table_matches_the_index_lookup() -> TestResult {
        for (index, milliseconds) in VoxDelay::MILLISECONDS.into_iter().enumerate() {
            let delay = VoxDelay::new(u8::try_from(index)?)?;
            assert_eq!(delay.as_milliseconds(), milliseconds);
        }
        Ok(())
    }

    #[test]
    fn sentences_and_gps_settings_display_their_flags() {
        let sentences = NmeaSentences::from_flags([true, false, false, false, true, false]);
        assert_eq!(sentences.to_string(), "GGA,RMC");
        assert_eq!(NmeaSentences::default().to_string(), "none");
        assert_eq!(
            sentences.to_flags(),
            [true, false, false, false, true, false]
        );
        assert!(sentences.contains(NmeaSentence::Rmc));
        assert!(!sentences.contains(NmeaSentence::Vtg));
        assert_eq!(
            sentences
                .with(NmeaSentence::Vtg)
                .without(NmeaSentence::Gga)
                .to_string(),
            "RMC,VTG"
        );
        let collected: NmeaSentences = [NmeaSentence::Gsv, NmeaSentence::Gga].into_iter().collect();
        assert_eq!(
            collected.iter().collect::<Vec<_>>(),
            [NmeaSentence::Gga, NmeaSentence::Gsv]
        );
        assert_eq!(NmeaSentence::Vtg.to_string(), "VTG");
        let gps = GpsSettings {
            gps_enabled: true,
            pc_output: false,
        };
        assert_eq!(gps.to_string(), "GPS on, PC output off");
    }

    #[test]
    fn clock_round_trips_the_twelve_digit_field() -> TestResult {
        let clock = RealTimeClock::from_wire_str("260921003607")?;
        assert_eq!(clock.to_string(), "2026-09-21 00:36:07");
        assert_eq!(clock.to_wire_string(), "260921003607");
        for text in ["26092100360", "2609210036070", "26092100360x", ""] {
            assert!(
                matches!(
                    RealTimeClock::from_wire_str(text),
                    Err(ValidationError::InvalidWireDigits { .. })
                ),
                "{text:?}"
            );
        }
        for (text, setting) in [
            ("261321003607", "clock month"),
            ("260900003607", "clock day"),
            ("260921243607", "clock hour"),
            ("260921006007", "clock minute"),
            ("260921003660", "clock second"),
        ] {
            let result = RealTimeClock::from_wire_str(text);
            assert!(
                matches!(result, Err(ValidationError::SettingOutOfRange { setting: actual, .. }) if actual == setting),
                "{text:?}: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn serial_information_requires_exact_lengths() -> TestResult {
        let info = SerialInformation::new("C6210439", "K01")?;
        assert_eq!(info.serial_number(), "C6210439");
        assert_eq!(info.model_code(), "K01");
        assert_eq!(info.to_string(), "C6210439 (K01)");
        assert!(SerialInformation::new("C621043", "K01").is_err());
        assert!(SerialInformation::new("C6210439", "K1").is_err());
        assert!(SerialInformation::new("C621 439", "K01").is_err());
        Ok(())
    }
}
