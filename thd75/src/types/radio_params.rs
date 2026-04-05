//! Validated parameter types for radio CAT command methods.
//!
//! These newtypes and enums enforce valid ranges at construction time
//! for parameters that the radio methods previously accepted as raw `u8`.

use std::fmt;

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// SquelchLevel (0-6)
// ---------------------------------------------------------------------------

/// Squelch threshold level (0-6).
///
/// 0 = open (no squelch), 6 = maximum squelch. Used by the `SQ` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SquelchLevel(u8);

impl SquelchLevel {
    /// Open squelch (level 0).
    pub const OPEN: Self = Self(0);
    /// Maximum squelch (level 6).
    pub const MAX: Self = Self(6);

    /// Creates a new `SquelchLevel` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 6`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 6 {
            Err(ValidationError::SettingOutOfRange {
                name: "squelch level",
                value,
                detail: "must be 0-6",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for SquelchLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SquelchLevel> for u8 {
    fn from(level: SquelchLevel) -> Self {
        level.0
    }
}

impl fmt::Display for SquelchLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SQ{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// AfGainLevel (0-99)
// ---------------------------------------------------------------------------

/// Audio frequency gain level (0-99).
///
/// Controls the volume output level. Used by the `AG` CAT command.
/// The wire format is a bare 3-digit zero-padded decimal (`AG 015\r`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AfGainLevel(u8);

impl AfGainLevel {
    /// Creates a new `AfGainLevel` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 99`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 99 {
            Err(ValidationError::SettingOutOfRange {
                name: "AF gain level",
                value,
                detail: "must be 0-99",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for AfGainLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AfGainLevel> for u8 {
    fn from(level: AfGainLevel) -> Self {
        level.0
    }
}

impl fmt::Display for AfGainLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// SMeterReading (0-5)
// ---------------------------------------------------------------------------

/// S-meter reading (0-5).
///
/// The radio returns 0-5 via the `SM` command, mapping to signal strengths
/// S0, S1, S3, S5, S7, S9 respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SMeterReading(u8);

impl SMeterReading {
    /// Creates a new `SMeterReading` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 5`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 5 {
            Err(ValidationError::SettingOutOfRange {
                name: "S-meter reading",
                value,
                detail: "must be 0-5",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }

    /// Returns the approximate S-unit string.
    #[must_use]
    pub const fn s_unit(&self) -> &'static str {
        match self.0 {
            0 => "S0",
            1 => "S1",
            2 => "S3",
            3 => "S5",
            4 => "S7",
            5 => "S9",
            _ => "S?",
        }
    }
}

impl TryFrom<u8> for SMeterReading {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SMeterReading> for u8 {
    fn from(reading: SMeterReading) -> Self {
        reading.0
    }
}

impl fmt::Display for SMeterReading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.s_unit())
    }
}

// ---------------------------------------------------------------------------
// VfoMemoryMode
// ---------------------------------------------------------------------------

/// VFO/Memory/Call/Weather operating mode.
///
/// Controls which channel selection mode the band is in.
/// Used by the `VM` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VfoMemoryMode {
    /// VFO mode — frequency entered directly (index 0).
    Vfo = 0,
    /// Memory channel mode — recalls stored channels (index 1).
    Memory = 1,
    /// Call channel mode — quick-access channel (index 2).
    Call = 2,
    /// Weather channel mode — NOAA weather frequencies (index 3).
    Weather = 3,
}

impl fmt::Display for VfoMemoryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vfo => f.write_str("VFO"),
            Self::Memory => f.write_str("Memory"),
            Self::Call => f.write_str("Call"),
            Self::Weather => f.write_str("Weather"),
        }
    }
}

impl TryFrom<u8> for VfoMemoryMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Vfo),
            1 => Ok(Self::Memory),
            2 => Ok(Self::Call),
            3 => Ok(Self::Weather),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "VFO/memory mode",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<VfoMemoryMode> for u8 {
    fn from(mode: VfoMemoryMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// FilterMode
// ---------------------------------------------------------------------------

/// Receiver filter mode selection.
///
/// Selects which demodulator's filter width to read or set.
/// Used by the `SF` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    /// SSB (LSB/USB) filter (index 0).
    Ssb = 0,
    /// CW filter (index 1).
    Cw = 1,
    /// AM filter (index 2).
    Am = 2,
}

impl fmt::Display for FilterMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ssb => f.write_str("SSB"),
            Self::Cw => f.write_str("CW"),
            Self::Am => f.write_str("AM"),
        }
    }
}

impl TryFrom<u8> for FilterMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ssb),
            1 => Ok(Self::Cw),
            2 => Ok(Self::Am),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "filter mode",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<FilterMode> for u8 {
    fn from(mode: FilterMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// BatteryLevel (0-4)
// ---------------------------------------------------------------------------

/// Battery charge level (0-4).
///
/// Reported by the `BL` CAT command. Read-only on the TH-D75.
/// - 0 = Empty (Red)
/// - 1 = 1/3 (Yellow)
/// - 2 = 2/3 (Green)
/// - 3 = Full (Green)
/// - 4 = Charging (USB power connected)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatteryLevel {
    /// Empty — red battery indicator (index 0).
    Empty = 0,
    /// One-third — yellow battery indicator (index 1).
    OneThird = 1,
    /// Two-thirds — green battery indicator (index 2).
    TwoThirds = 2,
    /// Full — green battery indicator (index 3).
    Full = 3,
    /// Charging — USB power connected (index 4).
    Charging = 4,
}

impl fmt::Display for BatteryLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::OneThird => f.write_str("1/3"),
            Self::TwoThirds => f.write_str("2/3"),
            Self::Full => f.write_str("Full"),
            Self::Charging => f.write_str("Charging"),
        }
    }
}

impl TryFrom<u8> for BatteryLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Empty),
            1 => Ok(Self::OneThird),
            2 => Ok(Self::TwoThirds),
            3 => Ok(Self::Full),
            4 => Ok(Self::Charging),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "battery level",
                value,
                detail: "must be 0-4",
            }),
        }
    }
}

impl From<BatteryLevel> for u8 {
    fn from(level: BatteryLevel) -> Self {
        level as Self
    }
}

// ---------------------------------------------------------------------------
// VoxGain (0-9)
// ---------------------------------------------------------------------------

/// VOX gain level (0-9).
///
/// Controls the microphone sensitivity threshold for VOX activation.
/// Used by the `VG` CAT command. VOX must be enabled (`VX 1`) first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxGain(u8);

impl VoxGain {
    /// Creates a new `VoxGain` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 9`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 9 {
            Err(ValidationError::SettingOutOfRange {
                name: "VOX gain",
                value,
                detail: "must be 0-9",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for VoxGain {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<VoxGain> for u8 {
    fn from(gain: VoxGain) -> Self {
        gain.0
    }
}

impl fmt::Display for VoxGain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// VoxDelay (0-30)
// ---------------------------------------------------------------------------

/// VOX delay in 100ms units (0-30, i.e. 0ms to 3000ms).
///
/// Controls how long the transmitter stays keyed after voice stops.
/// Used by the `VD` CAT command. VOX must be enabled (`VX 1`) first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxDelay(u8);

impl VoxDelay {
    /// Creates a new `VoxDelay` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 30`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 30 {
            Err(ValidationError::SettingOutOfRange {
                name: "VOX delay",
                value,
                detail: "must be 0-30",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }

    /// Returns the delay in milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u16 {
        self.0 as u16 * 100
    }
}

impl TryFrom<u8> for VoxDelay {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<VoxDelay> for u8 {
    fn from(delay: VoxDelay) -> Self {
        delay.0
    }
}

impl fmt::Display for VoxDelay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.as_millis())
    }
}

// ---------------------------------------------------------------------------
// TncBaud
// ---------------------------------------------------------------------------

/// TNC data baud rate.
///
/// Controls the APRS/KISS data speed. Used by the `DS` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TncBaud {
    /// 1200 bps AFSK (index 0).
    Bps1200 = 0,
    /// 9600 bps GMSK (index 1).
    Bps9600 = 1,
}

impl fmt::Display for TncBaud {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bps1200 => f.write_str("1200 bps"),
            Self::Bps9600 => f.write_str("9600 bps"),
        }
    }
}

impl TryFrom<u8> for TncBaud {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Bps1200),
            1 => Ok(Self::Bps9600),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "TNC baud rate",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl From<TncBaud> for u8 {
    fn from(baud: TncBaud) -> Self {
        baud as Self
    }
}

// ---------------------------------------------------------------------------
// BeaconMode
// ---------------------------------------------------------------------------

/// APRS beacon transmission mode.
///
/// Controls how the radio sends APRS position beacons.
/// Used by the `BN` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconMode {
    /// Beaconing off (index 0).
    Off = 0,
    /// Manual beacon — press button to transmit (index 1).
    Manual = 1,
    /// PTT beacon — transmit position on each PTT keyup (index 2).
    Ptt = 2,
    /// Auto beacon — transmit at configured interval (index 3).
    Auto = 3,
    /// `SmartBeaconing` — adaptive interval based on speed/heading (index 4).
    SmartBeaconing = 4,
}

impl fmt::Display for BeaconMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::Manual => f.write_str("Manual"),
            Self::Ptt => f.write_str("PTT"),
            Self::Auto => f.write_str("Auto"),
            Self::SmartBeaconing => f.write_str("SmartBeaconing"),
        }
    }
}

impl TryFrom<u8> for BeaconMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Manual),
            2 => Ok(Self::Ptt),
            3 => Ok(Self::Auto),
            4 => Ok(Self::SmartBeaconing),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "beacon mode",
                value,
                detail: "must be 0-4",
            }),
        }
    }
}

impl From<BeaconMode> for u8 {
    fn from(mode: BeaconMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// DstarSlot (1-6)
// ---------------------------------------------------------------------------

/// D-STAR memory slot index (1-6).
///
/// Identifies one of the 6 D-STAR callsign memory slots.
/// Used by the `SD` and `CS` CAT commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarSlot(u8);

impl DstarSlot {
    /// Creates a new `DstarSlot` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value` is not 1-6.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value == 0 || value > 6 {
            Err(ValidationError::SettingOutOfRange {
                name: "D-STAR slot",
                value,
                detail: "must be 1-6",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DstarSlot {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DstarSlot> for u8 {
    fn from(slot: DstarSlot) -> Self {
        slot.0
    }
}

impl fmt::Display for DstarSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Slot {}", self.0)
    }
}

// ---------------------------------------------------------------------------
// CallsignSlot (0-10)
// ---------------------------------------------------------------------------

/// D-STAR active callsign slot index (0-10).
///
/// Selects which callsign from the repeater list is active.
/// Used by the `CS` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallsignSlot(u8);

impl CallsignSlot {
    /// Creates a new `CallsignSlot` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 10`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 10 {
            Err(ValidationError::SettingOutOfRange {
                name: "callsign slot",
                value,
                detail: "must be 0-10",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for CallsignSlot {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CallsignSlot> for u8 {
    fn from(slot: CallsignSlot) -> Self {
        slot.0
    }
}

impl fmt::Display for CallsignSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Slot {}", self.0)
    }
}

// ---------------------------------------------------------------------------
// DetectOutputMode (IO command)
// ---------------------------------------------------------------------------

/// AF/IF/Detect output mode (Menu 102).
///
/// Controls what signal is output via the USB connector to a PC.
/// Used by the `IO` CAT command.
///
/// Source: User Manual §12-2 "AF/IF/DETECT OUTPUT MODE".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetectOutputMode {
    /// AF output — received audio sound (index 0).
    Af = 0,
    /// IF output — received IF signal of Band B to PC (index 1).
    If = 1,
    /// Detect output — decoded signal of Band B to PC (index 2).
    Detect = 2,
}

impl fmt::Display for DetectOutputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Af => f.write_str("AF"),
            Self::If => f.write_str("IF"),
            Self::Detect => f.write_str("Detect"),
        }
    }
}

impl TryFrom<u8> for DetectOutputMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Af),
            1 => Ok(Self::If),
            2 => Ok(Self::Detect),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "detect output mode",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<DetectOutputMode> for u8 {
    fn from(mode: DetectOutputMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// DvGatewayMode
// ---------------------------------------------------------------------------

/// DV Gateway operating mode (Menu 650).
///
/// Controls whether the radio acts as a DV Gateway for D-STAR reflector
/// access via USB or Bluetooth using third-party MMDVM applications.
/// Used by the `GW` CAT command.
///
/// Source: User Manual §16-13, firmware decompilation of `cat_gw_handler`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DvGatewayMode {
    /// DV Gateway off (index 0).
    Off = 0,
    /// Reflector Terminal Mode enabled (index 1).
    ReflectorTerminal = 1,
}

impl fmt::Display for DvGatewayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::ReflectorTerminal => f.write_str("Reflector TERM"),
        }
    }
}

impl TryFrom<u8> for DvGatewayMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::ReflectorTerminal),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "DV gateway mode",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl From<DvGatewayMode> for u8 {
    fn from(mode: DvGatewayMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// TncMode
// ---------------------------------------------------------------------------

/// TNC operating mode.
///
/// Controls the built-in TNC's protocol mode. Used by the `TN` CAT command.
/// The second field of TN is the data speed (0=1200, 1=9600).
///
/// Source: firmware validation (mode < 4), Operating Tips §2.7-2.8 (KISS),
/// §4.5 (Reflector Terminal/MMDVM), firmware string table (NAVITRA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TncMode {
    /// APRS mode — standard packet operation (index 0).
    Aprs = 0,
    /// NAVITRA mode — Japanese APRS variant (index 1).
    Navitra = 1,
    /// KISS mode — PC-based packet via KISS protocol (index 2).
    /// Enter with `TN 2,0`. See Operating Tips §2.7.
    Kiss = 2,
    /// MMDVM/Reflector Terminal mode — D-STAR reflector access (index 3).
    /// Uses MMDVM serial commands via USB or Bluetooth.
    /// See Operating Tips §4.5.
    Mmdvm = 3,
}

impl fmt::Display for TncMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aprs => f.write_str("APRS"),
            Self::Navitra => f.write_str("NAVITRA"),
            Self::Kiss => f.write_str("KISS"),
            Self::Mmdvm => f.write_str("MMDVM"),
        }
    }
}

impl TryFrom<u8> for TncMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Aprs),
            1 => Ok(Self::Navitra),
            2 => Ok(Self::Kiss),
            3 => Ok(Self::Mmdvm),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "TNC mode",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<TncMode> for u8 {
    fn from(mode: TncMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// FilterWidthIndex (SH command)
// ---------------------------------------------------------------------------

/// IF receive filter width index for the SH (filter width) command.
///
/// The valid range depends on the filter mode:
/// - **SSB** (mode 0): 0-4 → 2.2 / 2.4 / 2.6 / 2.8 / 3.0 kHz high-cut
/// - **CW** (mode 1): 0-4 → 0.3 / 0.5 / 1.0 / 1.5 / 2.0 kHz bandwidth
/// - **AM** (mode 2): 0-3 → 3.0 / 4.5 / 6.0 / 7.5 kHz high-cut
///
/// Source: Kenwood TH-D75A/E Operating Tips §5.10 (May 2024).
/// Hardware-verified: `SH mode,width\r` returns echo on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FilterWidthIndex(u8);

impl FilterWidthIndex {
    /// Maximum valid index for SSB and CW modes.
    const MAX_SSB_CW: u8 = 4;
    /// Maximum valid index for AM mode.
    const MAX_AM: u8 = 3;

    /// Creates a new `FilterWidthIndex`, validating against the given mode.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value` exceeds the
    /// mode-specific maximum (4 for SSB/CW, 3 for AM).
    pub const fn new(value: u8, mode: FilterMode) -> Result<Self, ValidationError> {
        let max = match mode {
            FilterMode::Ssb | FilterMode::Cw => Self::MAX_SSB_CW,
            FilterMode::Am => Self::MAX_AM,
        };
        if value > max {
            Err(ValidationError::SettingOutOfRange {
                name: "filter width index",
                value,
                detail: match mode {
                    FilterMode::Ssb | FilterMode::Cw => "must be 0-4 for SSB/CW",
                    FilterMode::Am => "must be 0-3 for AM",
                },
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Creates a `FilterWidthIndex` from a raw value without mode checking.
    ///
    /// Uses the maximum range (0-4) which covers all modes. Use this when
    /// parsing responses where the mode is known but the width may come from
    /// hardware that could return extended values.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 4`.
    pub const fn from_raw(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX_SSB_CW {
            Err(ValidationError::SettingOutOfRange {
                name: "filter width index",
                value,
                detail: "must be 0-4",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for FilterWidthIndex {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_raw(value)
    }
}

impl From<FilterWidthIndex> for u8 {
    fn from(idx: FilterWidthIndex) -> Self {
        idx.0
    }
}

impl fmt::Display for FilterWidthIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// GpsRadioMode (GM command)
// ---------------------------------------------------------------------------

/// GPS/Radio operating mode (GM command).
///
/// Controls whether the radio operates in normal transceiver mode or
/// switches to GPS-receiver-only mode.
///
/// # Firmware verification
///
/// The `cat_gm_handler` at `0xC002EC52` guards with `local_18 < 2`,
/// confirming only values 0 and 1 are valid.
///
/// # Warning
///
/// Setting this to `GpsReceiver` (1) via `GM 1\r` **reboots the radio**
/// into GPS-only mode. The radio becomes unresponsive to CAT commands
/// until manually power-cycled back to normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpsRadioMode {
    /// Normal transceiver mode (index 0).
    Normal = 0,
    /// GPS receiver mode (index 1) — **reboots the radio**.
    GpsReceiver = 1,
}

impl TryFrom<u8> for GpsRadioMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Normal),
            1 => Ok(Self::GpsReceiver),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "GPS radio mode",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl From<GpsRadioMode> for u8 {
    fn from(mode: GpsRadioMode) -> Self {
        mode as Self
    }
}

impl fmt::Display for GpsRadioMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normal => write!(f, "Normal"),
            Self::GpsReceiver => write!(f, "GPS Receiver"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squelch_level_valid() {
        for v in 0..=6 {
            assert!(SquelchLevel::new(v).is_ok());
        }
        assert!(SquelchLevel::new(7).is_err());
    }

    #[test]
    fn squelch_level_round_trip() {
        let sq = SquelchLevel::new(4).unwrap();
        assert_eq!(u8::from(sq), 4);
        assert_eq!(sq.as_u8(), 4);
    }

    #[test]
    fn af_gain_valid() {
        assert!(AfGainLevel::new(0).is_ok());
        assert!(AfGainLevel::new(99).is_ok());
        assert!(AfGainLevel::new(100).is_err());
    }

    #[test]
    fn smeter_s_units() {
        assert_eq!(SMeterReading::new(0).unwrap().s_unit(), "S0");
        assert_eq!(SMeterReading::new(5).unwrap().s_unit(), "S9");
        assert!(SMeterReading::new(6).is_err());
    }

    #[test]
    fn vfo_memory_mode_round_trip() {
        for v in 0..=3 {
            let mode = VfoMemoryMode::try_from(v).unwrap();
            assert_eq!(u8::from(mode), v);
        }
        assert!(VfoMemoryMode::try_from(4).is_err());
    }

    #[test]
    fn filter_mode_round_trip() {
        for v in 0..=2 {
            let mode = FilterMode::try_from(v).unwrap();
            assert_eq!(u8::from(mode), v);
        }
        assert!(FilterMode::try_from(3).is_err());
    }

    #[test]
    fn battery_level_round_trip() {
        for v in 0..=4 {
            let bl = BatteryLevel::try_from(v).unwrap();
            assert_eq!(u8::from(bl), v);
        }
        assert!(BatteryLevel::try_from(5).is_err());
    }

    #[test]
    fn battery_level_charging() {
        assert_eq!(BatteryLevel::try_from(4).unwrap(), BatteryLevel::Charging);
    }

    #[test]
    fn vox_gain_valid() {
        assert!(VoxGain::new(0).is_ok());
        assert!(VoxGain::new(9).is_ok());
        assert!(VoxGain::new(10).is_err());
    }

    #[test]
    fn vox_delay_millis() {
        let d = VoxDelay::new(15).unwrap();
        assert_eq!(d.as_millis(), 1500);
        assert!(VoxDelay::new(31).is_err());
    }

    #[test]
    fn tnc_baud_round_trip() {
        assert_eq!(TncBaud::try_from(0).unwrap(), TncBaud::Bps1200);
        assert_eq!(TncBaud::try_from(1).unwrap(), TncBaud::Bps9600);
        assert!(TncBaud::try_from(2).is_err());
    }

    #[test]
    fn beacon_mode_round_trip() {
        for v in 0..=4 {
            let mode = BeaconMode::try_from(v).unwrap();
            assert_eq!(u8::from(mode), v);
        }
        assert!(BeaconMode::try_from(5).is_err());
    }

    #[test]
    fn dstar_slot_valid() {
        assert!(DstarSlot::new(0).is_err());
        assert!(DstarSlot::new(1).is_ok());
        assert!(DstarSlot::new(6).is_ok());
        assert!(DstarSlot::new(7).is_err());
    }

    #[test]
    fn tnc_mode_round_trip() {
        for v in 0..=3 {
            let mode = TncMode::try_from(v).unwrap();
            assert_eq!(u8::from(mode), v);
        }
        assert!(TncMode::try_from(4).is_err());
    }

    #[test]
    fn tnc_mode_kiss() {
        assert_eq!(TncMode::try_from(2).unwrap(), TncMode::Kiss);
    }

    #[test]
    fn callsign_slot_valid() {
        assert!(CallsignSlot::new(0).is_ok());
        assert!(CallsignSlot::new(10).is_ok());
        assert!(CallsignSlot::new(11).is_err());
    }

    #[test]
    fn filter_width_ssb_cw_range() {
        for v in 0..=4 {
            assert!(FilterWidthIndex::new(v, FilterMode::Ssb).is_ok());
            assert!(FilterWidthIndex::new(v, FilterMode::Cw).is_ok());
        }
        assert!(FilterWidthIndex::new(5, FilterMode::Ssb).is_err());
        assert!(FilterWidthIndex::new(5, FilterMode::Cw).is_err());
    }

    #[test]
    fn filter_width_am_range() {
        for v in 0..=3 {
            assert!(FilterWidthIndex::new(v, FilterMode::Am).is_ok());
        }
        assert!(FilterWidthIndex::new(4, FilterMode::Am).is_err());
    }

    #[test]
    fn filter_width_from_raw() {
        assert!(FilterWidthIndex::from_raw(4).is_ok());
        assert!(FilterWidthIndex::from_raw(5).is_err());
    }

    #[test]
    fn detect_output_mode_round_trip() {
        assert_eq!(DetectOutputMode::try_from(0).unwrap(), DetectOutputMode::Af);
        assert_eq!(DetectOutputMode::try_from(1).unwrap(), DetectOutputMode::If);
        assert_eq!(
            DetectOutputMode::try_from(2).unwrap(),
            DetectOutputMode::Detect
        );
        assert!(DetectOutputMode::try_from(3).is_err());
    }

    #[test]
    fn dv_gateway_mode_round_trip() {
        assert_eq!(DvGatewayMode::try_from(0).unwrap(), DvGatewayMode::Off);
        assert_eq!(
            DvGatewayMode::try_from(1).unwrap(),
            DvGatewayMode::ReflectorTerminal
        );
        assert!(DvGatewayMode::try_from(2).is_err());
    }

    #[test]
    fn gps_radio_mode_round_trip() {
        assert_eq!(GpsRadioMode::try_from(0).unwrap(), GpsRadioMode::Normal);
        assert_eq!(
            GpsRadioMode::try_from(1).unwrap(),
            GpsRadioMode::GpsReceiver
        );
        assert!(GpsRadioMode::try_from(2).is_err());
    }
}
