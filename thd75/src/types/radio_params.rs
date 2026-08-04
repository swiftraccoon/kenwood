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
/// Squelch can be set independently for Band A and Band B.
///
/// Per User Manual Chapter 5: the squelch mutes the speaker when no signals
/// are present. The higher the level, the stronger the signal must be to
/// open squelch. Adjust with `[F]`, `[MONI]` on the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SquelchLevel(u8);

impl SquelchLevel {
    /// Open squelch (level 0).
    pub const OPEN: Self = Self(0);
    /// Maximum valid squelch level (inclusive).
    pub const MAX: u8 = 6;
    /// Number of valid squelch levels (0-6).
    pub const COUNT: u8 = 7;

    /// Creates a new `SquelchLevel` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 6`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
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
    pub const fn as_raw(self) -> u8 {
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
// AfGainLevel (0-200)
// ---------------------------------------------------------------------------

/// Audio frequency gain level (0-200).
///
/// Controls the volume output level. Used by the `AG` CAT command.
/// The wire format is a bare 3-digit zero-padded decimal (`AG 015\r`).
/// Firmware handler validation and hardware reads establish one shared
/// read/write domain of 0 through 200.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AfGainLevel(u8);

impl AfGainLevel {
    /// Muted audio (level 0).
    pub const ZERO: Self = Self(0);
    /// Maximum valid AF gain level (inclusive).
    pub const MAX: u8 = 200;

    /// Creates a new `AfGainLevel` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 200`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                name: "AF gain level",
                value,
                detail: "must be 0-200",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
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
    /// Zero reading (S0, no signal).
    pub const ZERO: Self = Self(0);
    /// Number of valid S-meter reading values (0-5).
    pub const COUNT: u8 = 6;

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
    pub const fn as_raw(self) -> u8 {
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
// TuningMode
// ---------------------------------------------------------------------------

/// VFO/Memory/Call/Weather tuning mode.
///
/// Controls which channel selection mode the band is in.
/// Used by the `VM` CAT command.
///
/// Per User Manual Chapter 5:
///
/// - **VFO mode** (`[VFO]`): manually tune to any frequency using the
///   encoder dial, up/down keys, or direct frequency entry via keypad.
///   The default step size varies by band and model (e.g., TH-D75A:
///   5 kHz on 144 MHz, 20 kHz on 220 MHz, 25 kHz on 430 MHz).
/// - **Memory mode** (`[MR]`): recall one of 1000 stored memory channels
///   (0-999) plus 100 program scan memories and 1 priority channel.
/// - **Call mode** (`[CALL]`): quick-access channel for emergency/group
///   use. Default call channels: TH-D75A 146.520 FM (VHF), 446.000 FM
///   (UHF); TH-D75E 145.500 FM (VHF), 433.500 FM (UHF).
/// - **Weather mode**: NOAA weather channels (TH-D75A only, 10 channels
///   A1-A10 at 161.650-163.275 MHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TuningMode {
    /// VFO mode: frequency entered directly (index 0).
    Vfo = 0,
    /// Memory channel mode: recalls stored channels (index 1).
    Memory = 1,
    /// Call channel mode: quick-access channel (index 2).
    Call = 2,
    /// Weather channel mode: NOAA weather frequencies (index 3).
    Weather = 3,
}

impl TuningMode {
    /// Number of valid VFO/memory mode values (0-3).
    pub const COUNT: u8 = 4;
}

impl fmt::Display for TuningMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vfo => f.write_str("VFO"),
            Self::Memory => f.write_str("Memory"),
            Self::Call => f.write_str("Call"),
            Self::Weather => f.write_str("Weather"),
        }
    }
}

impl TryFrom<u8> for TuningMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Vfo),
            1 => Ok(Self::Memory),
            2 => Ok(Self::Call),
            3 => Ok(Self::Weather),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "tuning mode",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<TuningMode> for u8 {
    fn from(mode: TuningMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// FilterMode
// ---------------------------------------------------------------------------

/// Receiver filter mode selection.
///
/// Selects which demodulator's filter width to read or set.
/// Used by the `SH` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    /// SSB (LSB/USB) filter (index 0).
    Ssb = 0,
    /// CW filter (index 1).
    Cw = 1,
    /// AM filter (index 2).
    Am = 2,
}

impl FilterMode {
    /// Number of valid filter mode values (0-2).
    pub const COUNT: u8 = 3;
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
// BatteryLevel (0-5)
// ---------------------------------------------------------------------------

/// Battery runtime state (0-5).
///
/// Reported by the `BL` CAT command. Read-only on the TH-D75.
/// Menu No. 922 displays the battery level on the radio.
///
/// - 0 = Empty (Red)
/// - 1 = 1/3 (Yellow)
/// - 2 = 2/3 (Green)
/// - 3 = Full (Green)
/// - 4 = Charging (USB power connected)
/// - 5 = Firmware runtime state 5 (meaning not yet qualified)
///
/// Per User Manual Chapter 28: the supplied KNB-75LA is 1820 mAh,
/// 7.4 V Li-ion. Battery life at TX:RX:standby = 6:6:48 ratio with
/// GPS off and battery saver on: H=6 hrs, M=8 hrs, L=12 hrs, EL=15 hrs.
/// GPS on reduces battery life by approximately 10%.
/// The optional KBP-9 case uses 6x AAA alkaline batteries (Low power
/// only, approximately 3.5 hours).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatteryLevel {
    /// Empty: red battery indicator (index 0).
    Empty = 0,
    /// One-third: yellow battery indicator (index 1).
    OneThird = 1,
    /// Two-thirds: green battery indicator (index 2).
    TwoThirds = 2,
    /// Full: green battery indicator (index 3).
    Full = 3,
    /// Charging: USB power connected (index 4).
    Charging = 4,
    /// Runtime state 5. The firmware can emit this value, but its user-facing
    /// meaning has not yet been established.
    Unidentified5 = 5,
}

impl BatteryLevel {
    /// Number of valid battery runtime values (0-5).
    pub const COUNT: u8 = 6;
}

impl fmt::Display for BatteryLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::OneThird => f.write_str("1/3"),
            Self::TwoThirds => f.write_str("2/3"),
            Self::Full => f.write_str("Full"),
            Self::Charging => f.write_str("Charging"),
            Self::Unidentified5 => f.write_str("State 5"),
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
            5 => Ok(Self::Unidentified5),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "battery level",
                value,
                detail: "must be 0-5",
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
/// Menu No. 151. Default: 4.
///
/// Per User Manual Chapter 12: gain 9 transmits even on a quiet voice;
/// gain 0 effectively disables VOX triggering. A headset must be used
/// because the internal speaker and microphone are too close together
/// for VOX to function reliably.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxGain(u8);

impl VoxGain {
    /// Minimum VOX gain (level 0).
    pub const ZERO: Self = Self(0);
    /// Maximum valid VOX gain value (inclusive).
    pub const MAX: u8 = 9;

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
    pub const fn as_raw(self) -> u8 {
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
// VoxDelay (raw index 0-6)
// ---------------------------------------------------------------------------

/// VOX delay selection encoded as a raw index from 0 through 6.
///
/// Controls how long the transmitter stays keyed after voice stops.
/// Used by the `VD` CAT command. VOX must be enabled (`VX 1`) first.
/// Menu No. 152. Default: 500 ms.
///
/// Per User Manual Chapter 12: available values are 250, 500, 750,
/// 1000, 1500, 2000, and 3000 ms. If you press `[PTT]` while VOX is
/// active, the delay time is not applied. If DCS is active, the radio
/// transmits a Turn-Off Code after the delay expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxDelay(u8);

impl VoxDelay {
    /// 250 ms (raw index 0).
    pub const MS_250: Self = Self(0);
    /// 500 ms (raw index 1).
    pub const MS_500: Self = Self(1);
    /// 750 ms (raw index 2).
    pub const MS_750: Self = Self(2);
    /// 1000 ms (raw index 3).
    pub const MS_1000: Self = Self(3);
    /// 1500 ms (raw index 4).
    pub const MS_1500: Self = Self(4);
    /// 2000 ms (raw index 5).
    pub const MS_2000: Self = Self(5);
    /// 3000 ms (raw index 6).
    pub const MS_3000: Self = Self(6);
    /// Maximum valid raw VOX delay index (inclusive).
    pub const MAX: u8 = 6;

    const MILLISECONDS: [u16; 7] = [250, 500, 750, 1000, 1500, 2000, 3000];

    /// Creates a new `VoxDelay` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 6`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                name: "VOX delay",
                value,
                detail: "raw index must be 0-6",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// Returns the delay in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
        let [
            delay_0,
            delay_1,
            delay_2,
            delay_3,
            delay_4,
            delay_5,
            delay_6,
        ] = Self::MILLISECONDS;
        match self.0 {
            0 => delay_0,
            1 => delay_1,
            2 => delay_2,
            3 => delay_3,
            4 => delay_4,
            5 => delay_5,
            6 => delay_6,
            _ => 0,
        }
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
        write!(f, "{}ms", self.as_milliseconds())
    }
}

// ---------------------------------------------------------------------------
// PacketDataRate
// ---------------------------------------------------------------------------

/// Packet-data rate shared by APRS, KISS, and MMDVM operation.
///
/// The CAT and stored APRS encodings both use `0` for 1200 bps and `1` for
/// 9600 bps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketDataRate {
    /// 1200 bps (index 0).
    Bps1200 = 0,
    /// 9600 bps (index 1).
    Bps9600 = 1,
}

impl PacketDataRate {
    /// Number of valid packet-data-rate values (0-1).
    pub const COUNT: u8 = 2;
}

impl fmt::Display for PacketDataRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bps1200 => f.write_str("1200 bps"),
            Self::Bps9600 => f.write_str("9600 bps"),
        }
    }
}

impl TryFrom<u8> for PacketDataRate {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Bps1200),
            1 => Ok(Self::Bps9600),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "packet data rate",
                value,
                detail: "must be 0-1: 1200/9600 bps",
            }),
        }
    }
}

impl From<PacketDataRate> for u8 {
    fn from(data_rate: PacketDataRate) -> Self {
        data_rate as Self
    }
}

// ---------------------------------------------------------------------------
// BeaconMode
// ---------------------------------------------------------------------------

/// APRS beacon transmission mode.
///
/// Controls how the radio sends APRS position beacons.
/// Used by the `PT` CAT command and the stored `aprs.BeaconTxMethod` setting;
/// those two representations intentionally share the same `0..=3` encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconMode {
    /// Manual beacon: transmit only when explicitly requested (wire value 0).
    Manual = 0,
    /// PTT beacon: transmit position on each PTT keyup (wire value 1).
    Ptt = 1,
    /// Auto beacon: transmit at the configured interval (wire value 2).
    Auto = 2,
    /// `SmartBeaconing`: adaptive interval based on speed/heading (wire value 3).
    SmartBeaconing = 3,
}

impl BeaconMode {
    /// Number of valid beacon mode values (0-3).
    pub const COUNT: u8 = 4;
}

impl fmt::Display for BeaconMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            0 => Ok(Self::Manual),
            1 => Ok(Self::Ptt),
            2 => Ok(Self::Auto),
            3 => Ok(Self::SmartBeaconing),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "beacon mode",
                value,
                detail: "must be 0-3",
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
// MyPositionSelection (0-5)
// ---------------------------------------------------------------------------

/// Selected APRS/GPS "My Position" entry (0-5).
///
/// This is the validated value read and written by the `MS` CAT command and
/// stored in the MCP `gps.MyPositionSelect` byte at `0x11C0`. It deliberately
/// preserves the numeric selection instead of assigning unverified names to
/// the six firmware values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MyPositionSelection(u8);

impl MyPositionSelection {
    /// Number of valid selection values (0-5).
    pub const COUNT: u8 = 6;

    /// Creates a validated My Position selection.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 5`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 5 {
            Err(ValidationError::SettingOutOfRange {
                name: "My Position selection",
                value,
                detail: "must be 0-5",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the CAT/MCP numeric value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for MyPositionSelection {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<MyPositionSelection> for u8 {
    fn from(selection: MyPositionSelection) -> Self {
        selection.0
    }
}

impl fmt::Display for MyPositionSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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
    /// Minimum valid D-STAR slot index.
    pub const MIN: u8 = 1;
    /// Maximum valid D-STAR slot index.
    pub const MAX: u8 = 6;
    /// Slot 1.
    pub const SLOT_1: Self = Self(1);
    /// Slot 2.
    pub const SLOT_2: Self = Self(2);
    /// Slot 3.
    pub const SLOT_3: Self = Self(3);

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
    pub const fn as_raw(self) -> u8 {
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
// AntennaInput (BS command)
// ---------------------------------------------------------------------------

/// MW/SW receive antenna selected by the `BS` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AntennaInput {
    /// External antenna connector (`BS 0`).
    Connector,
    /// Internal bar antenna (`BS 1`).
    InternalBar,
}

impl AntennaInput {
    /// Return the exact `BS` wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Connector => 0,
            Self::InternalBar => 1,
        }
    }
}

impl From<AntennaInput> for u8 {
    fn from(input: AntennaInput) -> Self {
        input.as_raw()
    }
}

impl fmt::Display for AntennaInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connector => f.write_str("ANT connector"),
            Self::InternalBar => f.write_str("internal bar antenna"),
        }
    }
}

// ---------------------------------------------------------------------------
// BandMode (DL command)
// ---------------------------------------------------------------------------

/// Front-panel band presentation selected by the `DL` CAT command.
///
/// The wire values name the resulting selection directly: `DL 0` is dual-band
/// display and `DL 1` is single-band display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandMode {
    /// Both bands are displayed (`DL 0`).
    Dual,
    /// Only the active band is displayed (`DL 1`).
    Single,
}

impl BandMode {
    /// Return the exact `DL` wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Dual => 0,
            Self::Single => 1,
        }
    }
}

impl TryFrom<u8> for BandMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dual),
            1 => Ok(Self::Single),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "band mode",
                value,
                detail: "must be 0 (dual) or 1 (single)",
            }),
        }
    }
}

impl From<BandMode> for u8 {
    fn from(mode: BandMode) -> Self {
        mode.as_raw()
    }
}

impl fmt::Display for BandMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dual => f.write_str("Dual Band"),
            Self::Single => f.write_str("Single Band"),
        }
    }
}

// ---------------------------------------------------------------------------
// UsbAudioOutput (IO command)
// ---------------------------------------------------------------------------

/// AF/IF/Detect output mode (Menu No. 102).
///
/// Controls what signal is output via the USB connector to a PC.
/// Used by the `IO` CAT command. Band B single-band mode must be
/// active to select IF or Detect.
///
/// Per User Manual Chapter 12:
///
/// - When IF or Detect is selected, Band A is hidden and its audio
///   output stops. Beeps and voice guidance are also suppressed.
/// - Special PC software is required to process IF or Detect signals.
/// - KISS mode prevents selecting IF or Detect.
/// - DV mode prevents selecting Detect.
/// - For IF 12 kHz output, the demodulation mode can be AM/LSB/USB/CW.
///
/// Source: User Manual Chapter 12 "AF/IF/DETECT OUTPUT MODE".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsbAudioOutput {
    /// Received audio output (index 0).
    Audio = 0,
    /// Intermediate-frequency signal from Band B (index 1).
    IntermediateFrequency = 1,
    /// Detect output: decoded signal of Band B to PC (index 2).
    Detect = 2,
}

impl UsbAudioOutput {
    /// Number of valid detect output mode values (0-2).
    pub const COUNT: u8 = 3;
}

impl fmt::Display for UsbAudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Audio => f.write_str("Audio"),
            Self::IntermediateFrequency => f.write_str("Intermediate Frequency"),
            Self::Detect => f.write_str("Detect"),
        }
    }
}

impl TryFrom<u8> for UsbAudioOutput {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Audio),
            1 => Ok(Self::IntermediateFrequency),
            2 => Ok(Self::Detect),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "detect output mode",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<UsbAudioOutput> for u8 {
    fn from(mode: UsbAudioOutput) -> Self {
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
/// Menu 650 is described in User Manual section 16-13.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DvGatewayMode {
    /// DV Gateway off (index 0).
    Off = 0,
    /// Reflector Terminal Mode enabled (index 1).
    ReflectorTerminal = 1,
}

impl DvGatewayMode {
    /// Number of firmware-defined DV gateway mode values (0-1).
    pub const COUNT: u8 = 2;
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
/// The four established values cover ordinary CAT, APRS, KISS, and
/// MMDVM/Reflector Terminal operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TncMode {
    /// TNC off: no packet mode active, plain CAT operation (index 0).
    ///
    /// After `TN 0,0`, the radio shows no packet-mode indicator: neither
    /// `APRS 12` nor `KISS 12`.
    Off = 0,
    /// APRS mode: packet operation run by the radio firmware (index 1).
    /// The display shows "APRS 12" (or "APRS 96" at 9600 bps).
    Aprs = 1,
    /// KISS mode: PC-based packet via KISS protocol (index 2).
    /// Enter with `TN 2,0` (1200 bps) or `TN 2,1` (9600 bps); the
    /// display shows "KISS 12" / "KISS 96".
    /// See Operating Tips §2.7, User Manual Chapter 15.
    /// The built-in TNC has 4 KB TX and RX buffers and supports only
    /// KISS mode (no Command mode or Converse mode).
    Kiss = 2,
    /// MMDVM/Reflector Terminal mode: D-STAR reflector access (index 3).
    /// Uses MMDVM serial commands via USB or Bluetooth.
    /// See Operating Tips §4.5.
    Mmdvm = 3,
}

impl TncMode {
    /// Number of valid TNC mode values (0-3).
    pub const COUNT: u8 = 4;
}

impl fmt::Display for TncMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Aprs => f.write_str("APRS"),
            Self::Kiss => f.write_str("KISS"),
            Self::Mmdvm => f.write_str("MMDVM"),
        }
    }
}

impl TryFrom<u8> for TncMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Aprs),
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

/// TNC modes that leave the transport under ordinary CAT control.
///
/// KISS and MMDVM are deliberately absent because entering either binary
/// protocol transfers transport ownership to a typed session. Use
/// [`Radio::enter_kiss`](crate::radio::Radio::enter_kiss) or
/// [`Radio::enter_mmdvm`](crate::radio::Radio::enter_mmdvm) for those modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TncControlMode {
    /// Disable the radio's packet engine.
    Off,
    /// Let the radio firmware operate APRS itself.
    Aprs,
}

impl From<TncControlMode> for TncMode {
    fn from(mode: TncControlMode) -> Self {
        match mode {
            TncControlMode::Off => Self::Off,
            TncControlMode::Aprs => Self::Aprs,
        }
    }
}

/// Current TNC mode and packet data rate returned by the `TN` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TncState {
    /// Current TNC operating mode.
    pub mode: TncMode,
    /// Current packet data rate.
    pub data_rate: PacketDataRate,
}

// ---------------------------------------------------------------------------
// FilterWidthIndex (SH command)
// ---------------------------------------------------------------------------

/// Mode-qualified IF receive filter width index for the SH command.
///
/// The receiver mode is retained with the numeric index because the valid
/// domain and physical bandwidth both depend on it. An AM width therefore
/// cannot be confused with an SSB/CW width after construction.
///
/// The valid range depends on the filter mode:
/// - **SSB** (mode 0): 0-4 -> 2.2 / 2.4 / 2.6 / 2.8 / 3.0 kHz high-cut
///   (Menu No. 120, default 2.4 kHz). Low cut is fixed at 200 Hz.
/// - **CW** (mode 1): 0-4 -> 0.3 / 0.5 / 1.0 / 1.5 / 2.0 kHz bandwidth
///   (Menu No. 121, default 1.0 kHz). The filter is centered on the
///   pitch frequency (Menu No. 170).
/// - **AM** (mode 2): 0-3 -> 3.0 / 4.5 / 6.0 / 7.5 kHz high-cut
///   (Menu No. 122, default 6.0 kHz). Low cut is fixed at 200 Hz.
///
/// Per User Manual Chapter 12: these filters reduce interference and
/// noise in SSB, CW, and AM modes to improve reception. Band B only.
///
/// Source: Kenwood TH-D75A/E Operating Tips §5.10 (May 2024).
/// Hardware-verified: `SH mode,width\r` returns echo on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FilterWidthIndex {
    mode: FilterMode,
    index: u8,
}

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
    pub const fn new(mode: FilterMode, index: u8) -> Result<Self, ValidationError> {
        let max = match mode {
            FilterMode::Ssb | FilterMode::Cw => Self::MAX_SSB_CW,
            FilterMode::Am => Self::MAX_AM,
        };
        if index > max {
            Err(ValidationError::SettingOutOfRange {
                name: "filter width index",
                value: index,
                detail: match mode {
                    FilterMode::Ssb | FilterMode::Cw => "must be 0-4 for SSB/CW",
                    FilterMode::Am => "must be 0-3 for AM",
                },
            })
        } else {
            Ok(Self { mode, index })
        }
    }

    /// Receiver mode whose width table this index belongs to.
    #[must_use]
    pub const fn mode(self) -> FilterMode {
        self.mode
    }

    /// Position in the selected mode's filter-width table.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.index
    }
}

impl fmt::Display for FilterWidthIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.index)
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
/// Hardware accepts only values 0 and 1.
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
    /// GPS receiver mode (index 1). **Reboots the radio.**
    GpsReceiver = 1,
}

impl GpsRadioMode {
    /// Number of valid GPS radio mode values (0-1).
    pub const COUNT: u8 = 2;
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
// Memory-read parameters (MemoryReadTarget, MemoryReadOffset, ReadLen)
// ---------------------------------------------------------------------------

/// One past the highest offset expressible by the GM memory-read wire grammar.
///
/// The radio rejects a request unless `offset + length - 1` is strictly less
/// than this value. It is the addressable window implied by the six
/// hexadecimal offset digits of the request grammar.
pub const MEMORY_READ_WIRE_BOUND: u32 = 0x0100_0000;

/// Qualified patched-firmware backend for the repurposed GM memory-read command.
///
/// The two V1.03 patches use the same wire grammar but add the transmitted
/// offset to different CPU-visible bases. Selecting a target is therefore part
/// of capability attestation, not an interpretation callers may change after a
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryReadTarget {
    /// `normal-gm-ddr-read`: offsets address DDR at `0xC0000000`.
    DdrV103,
    /// `normal-gm-nor-read`: offsets address NOR at `0x60000000`.
    ///
    /// Only the low 2 MiB were hardware-qualified. The patched handler's wider
    /// grammar does not authorize reads beyond that proven window.
    LowNorV103,
}

impl MemoryReadTarget {
    /// One past the last offset this exact target is qualified to read.
    #[must_use]
    pub const fn bound(self) -> u32 {
        match self {
            Self::DdrV103 => MEMORY_READ_WIRE_BOUND,
            Self::LowNorV103 => 0x0020_0000,
        }
    }

    /// Short stable name for logs and errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DdrV103 => "DDR V1.03",
            Self::LowNorV103 => "low NOR V1.03",
        }
    }
}

/// An offset into the radio's readable memory window.
///
/// This is an offset, not an absolute address. The base is fixed in the radio
/// and is never transmitted. [`MemoryReadTarget`] qualification determines
/// which base the installed firmware uses.
///
/// Valid range is `0x000000..=0xFFFFFF`, the span expressible in the six
/// hexadecimal digits of the request grammar.
///
/// Memory reads require firmware modified by the `thd75-fw` project. An
/// unmodified radio does not support them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryReadOffset(u32);

impl MemoryReadOffset {
    /// The lowest valid offset.
    pub const ZERO: Self = Self(0);
    /// The highest valid offset.
    pub const MAX: u32 = 0x00FF_FFFF;

    /// Constructs a compile-time offset whose bound has been verified by the
    /// defining module.
    pub(crate) const fn new_const(value: u32) -> Self {
        debug_assert!(
            value <= 0x00FF_FFFF,
            "compile-time memory-read offset exceeds the six-digit wire domain"
        );
        Self(value)
    }

    /// Creates a new `MemoryReadOffset` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::MemoryParamOutOfRange`] if
    /// `value > 0xFFFFFF`.
    pub const fn new(value: u32) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::MemoryParamOutOfRange {
                name: "memory-read offset",
                value,
                detail: "must be 0-0xFFFFFF",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw offset.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for MemoryReadOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:06X}", self.0)
    }
}

/// The number of bytes to read in one memory-read request.
///
/// Valid range is `1..=256`. The radio encodes 256 as the wire value `0x00`,
/// which is why [`ReadLen::as_wire`] is separate from [`ReadLen::as_bytes`].
///
/// The wire byte is what gets stored, so the logical count is produced by
/// widening rather than narrowing. That keeps the single unavoidable narrowing
/// in [`ReadLen::new`], where masking makes it provably lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReadLen(u8);

impl ReadLen {
    /// The largest read a single request can return.
    pub const MAX: u16 = 256;

    /// Creates a new `ReadLen` from a raw byte count.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::MemoryParamOutOfRange`] if `value` is 0 or
    /// greater than 256.
    pub const fn new(value: u16) -> Result<Self, ValidationError> {
        if value == 0 || value > 256 {
            return Err(ValidationError::MemoryParamOutOfRange {
                name: "read length",
                value: value as u32,
                detail: "must be 1-256",
            });
        }
        // `value` is 1..=256 here. Masking to 8 bits maps 1..=255 to
        // themselves and maps 256 to 0, which is exactly the radio's wire
        // encoding, so this narrowing is total and lossless by construction.
        // Clippy proves this too: an `expect(cast_possible_truncation)` here
        // is reported as an unfulfilled expectation, so no suppression is used.
        Ok(Self((value & 0xFF) as u8))
    }

    /// Returns the byte count, with the wire value `0x00` widened back to 256.
    ///
    /// Not a `const fn`: `u16::from` is not const, and using `as` here would
    /// trip `clippy::cast_lossless` for no benefit, since no caller needs this
    /// in a const context.
    #[must_use]
    pub fn as_bytes(self) -> u16 {
        if self.0 == 0 { 256 } else { u16::from(self.0) }
    }

    /// Returns the wire encoding, in which 256 is `0x00`.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self.0
    }
}

impl fmt::Display for ReadLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02X}", self.as_wire())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod memread_param_tests {
    use super::{MEMORY_READ_WIRE_BOUND, MemoryReadOffset, MemoryReadTarget, ReadLen};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn offset_accepts_zero_and_max() -> TestResult {
        assert_eq!(MemoryReadOffset::new(0)?.as_raw(), 0);
        assert_eq!(MemoryReadOffset::new(0x00FF_FFFF)?.as_raw(), 0x00FF_FFFF);
        assert_eq!(MemoryReadOffset::MAX, 0x00FF_FFFF);
        assert_eq!(MemoryReadOffset::ZERO.as_raw(), 0);
        Ok(())
    }

    #[test]
    fn offset_rejects_above_24_bits() {
        let result = MemoryReadOffset::new(0x0100_0000);
        assert!(
            result.is_err(),
            "0x1000000 must be rejected, got {result:?}"
        );
    }

    #[test]
    fn offset_displays_as_six_hex_digits() -> TestResult {
        assert_eq!(format!("{}", MemoryReadOffset::new(0x17_D1BC)?), "17D1BC");
        assert_eq!(format!("{}", MemoryReadOffset::ZERO), "000000");
        Ok(())
    }

    #[test]
    fn read_len_wire_encoding() -> TestResult {
        assert_eq!(ReadLen::new(1)?.as_wire(), 1);
        assert_eq!(ReadLen::new(255)?.as_wire(), 255);
        // 256 is encoded on the wire as 0x00.
        assert_eq!(ReadLen::new(256)?.as_wire(), 0);
        assert_eq!(ReadLen::new(256)?.as_bytes(), 256);
        assert_eq!(ReadLen::MAX, 256);
        Ok(())
    }

    #[test]
    fn read_len_rejects_zero_and_over_256() {
        let zero = ReadLen::new(0);
        assert!(zero.is_err(), "zero length must be rejected, got {zero:?}");
        let over = ReadLen::new(257);
        assert!(over.is_err(), "257 must be rejected, got {over:?}");
    }

    #[test]
    fn bound_is_one_past_max_offset() {
        assert_eq!(MEMORY_READ_WIRE_BOUND, MemoryReadOffset::MAX + 1);
    }

    #[test]
    fn memory_read_targets_have_distinct_qualified_bounds() {
        assert_eq!(MemoryReadTarget::DdrV103.bound(), MEMORY_READ_WIRE_BOUND);
        assert_eq!(MemoryReadTarget::LowNorV103.bound(), 0x0020_0000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn squelch_level_valid() -> TestResult {
        for v in 0..SquelchLevel::COUNT {
            let val = SquelchLevel::new(v)?;
            assert_eq!(val.as_raw(), v, "SquelchLevel round-trip failed at {v}");
        }
        assert!(SquelchLevel::new(SquelchLevel::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn squelch_level_round_trip() -> TestResult {
        let sq = SquelchLevel::new(4)?;
        assert_eq!(u8::from(sq), 4);
        assert_eq!(sq.as_raw(), 4);
        Ok(())
    }

    #[test]
    fn af_gain_valid() -> TestResult {
        assert_eq!(AfGainLevel::new(0)?.as_raw(), 0);
        assert_eq!(AfGainLevel::new(99)?.as_raw(), 99);
        assert_eq!(AfGainLevel::new(200)?.as_raw(), 200);
        assert!(AfGainLevel::new(201).is_err());
        assert_eq!(AfGainLevel::try_from(200)?.as_raw(), 200);
        assert!(AfGainLevel::try_from(201).is_err());
        Ok(())
    }

    #[test]
    fn smeter_s_units() -> TestResult {
        assert_eq!(SMeterReading::new(0)?.s_unit(), "S0");
        assert_eq!(SMeterReading::new(SMeterReading::COUNT - 1)?.s_unit(), "S9");
        assert!(SMeterReading::new(SMeterReading::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn tuning_mode_round_trip() -> TestResult {
        for v in 0..TuningMode::COUNT {
            let mode = TuningMode::try_from(v)?;
            assert_eq!(u8::from(mode), v);
        }
        assert!(TuningMode::try_from(TuningMode::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn filter_mode_round_trip() -> TestResult {
        for v in 0..FilterMode::COUNT {
            let mode = FilterMode::try_from(v)?;
            assert_eq!(u8::from(mode), v);
        }
        assert!(FilterMode::try_from(FilterMode::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn battery_level_round_trip() -> TestResult {
        for v in 0..BatteryLevel::COUNT {
            let bl = BatteryLevel::try_from(v)?;
            assert_eq!(u8::from(bl), v);
        }
        assert!(BatteryLevel::try_from(BatteryLevel::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn battery_level_charging() -> TestResult {
        assert_eq!(BatteryLevel::try_from(4)?, BatteryLevel::Charging);
        assert_eq!(BatteryLevel::try_from(5)?, BatteryLevel::Unidentified5);
        Ok(())
    }

    #[test]
    fn vox_gain_valid() {
        assert!(VoxGain::new(0).is_ok());
        assert!(VoxGain::new(VoxGain::MAX).is_ok());
        assert!(VoxGain::new(VoxGain::MAX + 1).is_err());
    }

    #[test]
    fn vox_delay_millis() -> TestResult {
        let expected = [250, 500, 750, 1000, 1500, 2000, 3000];
        for (raw, millis) in expected.into_iter().enumerate() {
            let delay = VoxDelay::new(u8::try_from(raw)?)?;
            assert_eq!(delay.as_milliseconds(), millis);
        }
        assert!(VoxDelay::new(VoxDelay::MAX + 1).is_err());
        Ok(())
    }

    #[test]
    fn packet_data_rate_round_trip() -> TestResult {
        for v in 0..PacketDataRate::COUNT {
            let val = PacketDataRate::try_from(v)?;
            assert_eq!(u8::from(val), v, "PacketDataRate round-trip failed at {v}");
        }
        assert!(PacketDataRate::try_from(PacketDataRate::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn beacon_mode_round_trip() -> TestResult {
        for v in 0..BeaconMode::COUNT {
            let mode = BeaconMode::try_from(v)?;
            assert_eq!(u8::from(mode), v);
        }
        assert!(BeaconMode::try_from(BeaconMode::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn dstar_slot_valid() {
        assert!(DstarSlot::new(DstarSlot::MIN - 1).is_err());
        assert!(DstarSlot::new(DstarSlot::MIN).is_ok());
        assert!(DstarSlot::new(DstarSlot::MAX).is_ok());
        assert!(DstarSlot::new(DstarSlot::MAX + 1).is_err());
    }

    #[test]
    fn tnc_mode_round_trip() -> TestResult {
        for v in 0..TncMode::COUNT {
            let mode = TncMode::try_from(v)?;
            assert_eq!(u8::from(mode), v);
        }
        assert!(TncMode::try_from(TncMode::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn tnc_mode_kiss() -> TestResult {
        assert_eq!(TncMode::try_from(2)?, TncMode::Kiss);
        Ok(())
    }

    #[test]
    fn filter_width_ssb_cw_range() {
        for v in 0..=4 {
            assert!(FilterWidthIndex::new(FilterMode::Ssb, v).is_ok());
            assert!(FilterWidthIndex::new(FilterMode::Cw, v).is_ok());
        }
        assert!(FilterWidthIndex::new(FilterMode::Ssb, 5).is_err());
        assert!(FilterWidthIndex::new(FilterMode::Cw, 5).is_err());
    }

    #[test]
    fn filter_width_am_range() {
        for v in 0..=3 {
            assert!(FilterWidthIndex::new(FilterMode::Am, v).is_ok());
        }
        assert!(FilterWidthIndex::new(FilterMode::Am, 4).is_err());
    }

    #[test]
    fn filter_width_retains_the_mode_that_defines_its_domain() -> TestResult {
        let width = FilterWidthIndex::new(FilterMode::Cw, 4)?;
        assert_eq!(width.mode(), FilterMode::Cw);
        assert_eq!(width.as_raw(), 4);
        Ok(())
    }

    #[test]
    fn detect_output_mode_round_trip() -> TestResult {
        for v in 0..UsbAudioOutput::COUNT {
            let val = UsbAudioOutput::try_from(v)?;
            assert_eq!(u8::from(val), v, "UsbAudioOutput round-trip failed at {v}");
        }
        assert!(UsbAudioOutput::try_from(UsbAudioOutput::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn dv_gateway_mode_round_trip() -> TestResult {
        for v in 0..DvGatewayMode::COUNT {
            let val = DvGatewayMode::try_from(v)?;
            assert_eq!(u8::from(val), v, "DvGatewayMode round-trip failed at {v}");
        }
        assert!(DvGatewayMode::try_from(DvGatewayMode::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn gps_radio_mode_round_trip() -> TestResult {
        for v in 0..GpsRadioMode::COUNT {
            let val = GpsRadioMode::try_from(v)?;
            assert_eq!(u8::from(val), v, "GpsRadioMode round-trip failed at {v}");
        }
        assert!(GpsRadioMode::try_from(GpsRadioMode::COUNT).is_err());
        Ok(())
    }
}
