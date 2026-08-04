//! Typed access to the system settings region of the memory image.
//!
//! The system settings occupy bytes `0x0000`-`0x1FFF` (32 pages, 8,192
//! bytes). This region stores the radio's global configuration
//! including band state, menu settings, and configuration strings.
//!
//! # Offset provenance
//!
//! Every menu-setting offset in this module is taken from the MCP-D75
//! field registry ([`MCP_D75_MENU_FIELDS`](super::MCP_D75_MENU_FIELDS),
//! generated from Kenwood's official MCP-D75 serializers) and
//! cross-checked against a memory dump read from a physical TH-D75
//! (`tests/memory_golden.rs`). A unit test in this file pins each
//! offset constant to its registry field so the two layers cannot
//! drift apart.
//!
//! | Region | Offsets | Source |
//! |--------|---------|--------|
//! | Band state (power level, attenuator, dual band) | `0x0359`, `0x035C`, `0x0396` | Hardware verified (registry has no field below `0x1000`) |
//! | Radio menu block | `0x1000`-`0x10DF` | MCP-D75 registry |
//! | APRS lock bits | `0x120A` | MCP-D75 registry (`aprs.*`) |
//! | DV EMR volume | `0x1A03` | MCP-D75 registry (`dv.*`) |
//!
//! Two registry cells are shared bit bytes: the key-lock configuration
//! pair at `0x1084` and the APRS lock triple at `0x120A`. Their setters
//! perform masked read-modify-write so unrelated bits survive.

use crate::error::ValidationError;
use crate::protocol::programming;
use crate::types::cw::CwPitch;
use crate::types::dstar::EmrVolume;
use crate::types::dtmf::DtmfPause;
use crate::types::scan::ScanResumeMethod;
use crate::types::settings::{
    AltitudeRainUnit, AmHighCut, AutoMuteReturnDelay, AutoPowerOff, BacklightControl,
    BacklightTimer, BatterySaverInterval, BeatShift, CwFilterWidth, DisplayUnits, DtmfToneDuration,
    FrontPanelPfFunction, Language, LinkedVolumeLevel, PcOutputInterface, PowerOnMessage,
    RepeaterCallKey, ScanRestartDelay, SpeedDistanceUnit, SsbHighCut, StoredFrontPanelPfAssignment,
    TemperatureUnit, TransmitTimeout, VoiceAnnounceMode, VoiceGuideSpeed,
};
use crate::types::{BandMode, CoordinateFormat, GridSquareFormat, PowerLevel, VoxDelay, VoxGain};

/// An invalid or unavailable value in the settings region of an MCP image.
///
/// Unlike live CAT parsing, settings access reads bytes that may have come
/// from a damaged or incompatible image. Callers must be able to distinguish
/// that state from a legitimate minimum or default selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SettingsValueError {
    /// The complete memory image did not contain a required setting byte.
    MissingByte {
        /// Registry setting name.
        setting: &'static str,
        /// Absolute byte offset in the MCP image.
        offset: usize,
    },
    /// A stored byte was outside the setting's declared domain.
    InvalidValue {
        /// Registry setting name.
        setting: &'static str,
        /// Absolute byte offset in the MCP image.
        offset: usize,
        /// Invalid stored byte.
        value: u8,
        /// Human-readable accepted domain.
        detail: &'static str,
    },
}

impl std::fmt::Display for SettingsValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingByte { setting, offset } => {
                write!(f, "{setting} byte is missing at MCP offset 0x{offset:04X}")
            }
            Self::InvalidValue {
                setting,
                offset,
                value,
                detail,
            } => write!(
                f,
                "{setting} has invalid value {value} at MCP offset 0x{offset:04X} ({detail})"
            ),
        }
    }
}

impl std::error::Error for SettingsValueError {}

// ---------------------------------------------------------------------------
// System settings region (0x0000 - 0x1FFF)
// ---------------------------------------------------------------------------

/// Byte offset of the system settings region.
const SETTINGS_OFFSET: usize = 0x0000;

/// Size of the system settings region in bytes.
const SETTINGS_SIZE: usize = (programming::SETTINGS_END as usize + 1
    - programming::SETTINGS_START as usize)
    * programming::PAGE_SIZE;

// ---------------------------------------------------------------------------
// Hardware-verified band-state offsets (outside registry scope)
//
// Each of these offsets was confirmed on a real TH-D75 by toggling the
// setting individually and identifying the changed byte in the MCP
// image. The MCP-D75 registry has no field below 0x1000, so these do
// not conflict with any serializer cell (a unit test below asserts
// that).
// ---------------------------------------------------------------------------

/// Hardware-verified offset for Band A power level (1 byte, 0=Hi, 1=Mid, 2=Lo, 3=EL).
const POWER_LEVEL_A_OFFSET: usize = 0x0359;

/// Hardware-verified offset for Band A attenuator on/off (1 byte, 0=off, 1=on).
const ATTENUATOR_A_OFFSET: usize = 0x035C;

/// Hardware-verified offset for dual-band display (1 byte, 0=single, 1=dual).
const DUAL_BAND_OFFSET: usize = 0x0396;

// ---------------------------------------------------------------------------
// Registry-mapped menu offsets (`radio.*` unless noted)
// ---------------------------------------------------------------------------

// --- TX/RX ---
/// `radio.BeatShift` (1 byte, 0-7 = Type 1-8).
const BEAT_SHIFT_OFFSET: usize = 0x1000;
/// `radio.TxInhibit` (1 byte, 0=off, 1=on).
const TX_INHIBIT_OFFSET: usize = 0x1001;
/// `radio.TimeOutTimer` (1 byte, 0-10 indexing 0.5-5.0 then 10.0 minutes).
const TIMEOUT_TIMER_OFFSET: usize = 0x1003;
/// `radio.MicSensitivity` (1 byte, 0=High, 1=Medium, 2=Low).
const MIC_SENSITIVITY_OFFSET: usize = 0x1006;
/// `radio.SsbHighCut` (1 byte, 0-4 = 2.2/2.4/2.6/2.8/3.0 kHz).
const SSB_HIGH_CUT_OFFSET: usize = 0x1008;
/// `radio.CwWidth` (1 byte, 0-4 = 0.3/0.5/1.0/1.5/2.0 kHz).
const CW_WIDTH_OFFSET: usize = 0x1009;
/// `radio.AmHighCut` (1 byte, 0-3 = 3.0/4.5/6.0/7.5 kHz).
const AM_HIGH_CUT_OFFSET: usize = 0x100A;

// --- Scan ---
/// `radio.ScanResumeAnalog` (1 byte, 0=Time, 1=Carrier, 2=Seek).
const SCAN_RESUME_OFFSET: usize = 0x100C;
/// `radio.ScanResumeDigital` (1 byte, 0=Time, 1=Carrier, 2=Seek).
const DIGITAL_SCAN_RESUME_OFFSET: usize = 0x100D;
/// `radio.TimeRestart` (1 byte, 1-10 seconds).
const SCAN_RESTART_TIME_OFFSET: usize = 0x100E;
/// `radio.CarrierRestart` (1 byte, 1-10 seconds).
const SCAN_RESTART_CARRIER_OFFSET: usize = 0x100F;

// --- Repeater ---
/// `radio.AutoOffset` (1 byte, 0=off, 1=on).
const REPEATER_AUTO_OFFSET_OFFSET: usize = 0x1018;
/// `radio.CallKey` (1 byte, 0=CALL, 1=1750 Hz).
const REPEATER_CALL_KEY_OFFSET: usize = 0x1019;

// --- VOX ---
/// `radio.Vox` (1 byte, 0=off, 1=on).
const VOX_ENABLED_OFFSET: usize = 0x101B;
/// `radio.VoxGain` (1 byte, 0-9).
const VOX_GAIN_OFFSET: usize = 0x101C;
/// `radio.VoxDelay` (1 byte, 0-6 indexing 250/500/750/1000/1500/2000/3000 ms).
const VOX_DELAY_OFFSET: usize = 0x101D;
/// `radio.VoxTxOnBusy` (1 byte, 0=off, 1=on).
const VOX_TX_ON_BUSY_OFFSET: usize = 0x101E;

// --- DTMF ---
/// `radio.DtmfSpeed` (1 byte, 0-2 = 50/100/150 ms).
const DTMF_SPEED_OFFSET: usize = 0x101F;
/// `radio.DtmfPauseTime` (1 byte, 0-6 = 100/250/500/750/1000/1500/2000 ms).
const DTMF_PAUSE_TIME_OFFSET: usize = 0x1020;
/// `radio.DtmfTxHold` (1 byte, 0=off, 1=on).
const DTMF_TX_HOLD_OFFSET: usize = 0x1021;

// --- CW receive ---
/// `radio.CwPitchFreq` (1 byte, 0-6).
const CW_PITCH_OFFSET: usize = 0x1024;

// --- Audio ---
/// `radio.AutoMuteRetTime` (1 byte, 1-10).
const AUTO_MUTE_RETURN_TIME_OFFSET: usize = 0x1041;

// --- Display ---
/// `radio.BacklightControl` (1 byte, 0=Manual, 1=On, 2=Auto, 3=Auto (DC-IN)).
const BACKLIGHT_CONTROL_OFFSET: usize = 0x1060;
/// `radio.BacklightTimer` (1 byte, 3-60 seconds).
const BACKLIGHT_TIMER_OFFSET: usize = 0x1061;

// --- Beep / voice guidance ---
/// `radio.Beep` (1 byte, 0=off, 1=on).
const KEY_BEEP_OFFSET: usize = 0x1071;
/// `radio.BeepVolume` (1 byte, 0=VOL Link, 1-7=Level 1-7).
const BEEP_VOLUME_OFFSET: usize = 0x1072;
/// `radio.VoiceAnnounce` (1 byte, 0=Off, 1=Manual, 2=Auto1, 3=Auto2).
const ANNOUNCE_OFFSET: usize = 0x1073;
/// `radio.VoiceAnnounceVolume` (1 byte, 0=VOL Link, 1-7=Level 1-7).
const VOICE_VOLUME_OFFSET: usize = 0x1074;
/// `radio.VoiceGuidanceSpeed` (1 byte, 0-3 = Speed 1-4).
const VOICE_SPEED_OFFSET: usize = 0x1097;

// --- Battery / power ---
/// `radio.BatterySaver` (1 byte, 0=Off, 1-9 select the saver interval 0.2-5.0 s).
const BATTERY_SAVER_OFFSET: usize = 0x1076;
/// `radio.AutoPowerOff` (1 byte, 0=Off, 1=15 min, 2=30 min, 3=60 min).
const AUTO_POWER_OFF_OFFSET: usize = 0x1077;

// --- Bluetooth ---
/// `radio.BluetoothOnOff` (1 byte, 0=off, 1=on). Hardware verified.
const BLUETOOTH_OFFSET: usize = 0x1078;
/// `radio.BluetoothAutoConnect` (1 byte, 0=off, 1=on).
const BT_AUTO_CONNECT_OFFSET: usize = 0x1079;

// --- PF keys ---
/// `radio.Pf1PfKey` (1 byte; official writable domain 0-30 with gaps).
const PF_KEY1_OFFSET: usize = 0x107A;
/// `radio.Pf2PfKey` (1 byte; official writable domain 0-30 with gaps).
const PF_KEY2_OFFSET: usize = 0x107B;

// --- Locks ---
/// Shared bit byte: `radio.KeyLockTypeKeyLock` (bit `0x01`) and
/// `radio.KeyLockTypeFrequencyLock` (bit `0x02`).
const KEY_LOCK_OFFSET: usize = 0x1084;
/// Bit owned by `radio.KeyLockTypeKeyLock` within `0x1084`.
const KEY_LOCK_KEY_MASK: u8 = 0x01;
/// Bit owned by `radio.KeyLockTypeFrequencyLock` within `0x1084`.
const KEY_LOCK_FREQUENCY_MASK: u8 = 0x02;
/// `radio.VolumeLockOnOff` (1 byte, 0=off, 1=on).
const VOLUME_LOCK_OFFSET: usize = 0x1087;
/// Shared bit byte for the APRS key-lock checkbox set: `aprs.Frequency`
/// (bit `0x01`), `aprs.Ptt` (bit `0x02`), `aprs.AprsKey` (bit `0x04`).
const APRS_LOCK_OFFSET: usize = 0x120A;
/// Bit owned by `aprs.Frequency` within `0x120A`.
const APRS_LOCK_FREQUENCY_MASK: u8 = 0x01;
/// Bit owned by `aprs.Ptt` within `0x120A`.
const APRS_LOCK_PTT_MASK: u8 = 0x02;
/// Bit owned by `aprs.AprsKey` within `0x120A`.
const APRS_LOCK_KEY_MASK: u8 = 0x04;

// --- Units ---
/// `radio.SpeedDistance` (1 byte, 0=mi/h+mile, 1=km/h+km, 2=knots+nm).
const SPEED_DISTANCE_UNIT_OFFSET: usize = 0x1088;
/// `radio.AltitudeRain` (1 byte, 0=feet/inch, 1=m/mm).
const ALTITUDE_RAIN_UNIT_OFFSET: usize = 0x1089;
/// `radio.Temperature` (1 byte, 0=°F, 1=°C).
const TEMPERATURE_UNIT_OFFSET: usize = 0x108A;
/// `radio.LatitudeLongitude` (1 byte, 0=DD MM.MM, 1=DD MM SS.S).
const COORDINATE_FORMAT_OFFSET: usize = 0x108B;
/// `radio.GridSquareFormat` (1 byte, 0=Maidenhead, 1=SAR CONV, 2=SAR CELL).
const GRID_SQUARE_FORMAT_OFFSET: usize = 0x108C;

// --- Interfaces ---
/// `radio.PcOutputInterfaceGps` (1 byte, 0=USB, 1=Bluetooth).
const GPS_BT_INTERFACE_OFFSET: usize = 0x108E;
/// `radio.PcOutputInterfaceAprs` (1 byte, 0=USB, 1=Bluetooth).
const APRS_USB_MODE_OFFSET: usize = 0x108F;

// --- System ---
/// `radio.Language` (1 byte, 0=English, 1=Japanese).
const LANGUAGE_OFFSET: usize = 0x1092;

// --- DV (D-STAR) ---
/// `dv.EmrVolumeLevelTxRx` (1 byte, 1-50).
const EMR_VOLUME_LEVEL_OFFSET: usize = 0x1A03;

// --- Strings ---
/// `radio.PowerOnMessage` (16 bytes, NUL-padded).
const POWER_ON_MESSAGE_OFFSET: usize = 0x10C0;
/// Size of the power-on message field.
const POWER_ON_MESSAGE_SIZE: usize = PowerOnMessage::WIRE_LEN;

// ---------------------------------------------------------------------------
// SettingsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the system settings region.
///
/// Provides raw byte access and typed field accessors for the settings
/// region at bytes `0x0000`-`0x1FFF`. Menu-setting offsets come from
/// the MCP-D75 field registry; the band-state trio below `0x1000` is
/// hardware-verified (see the module docs for provenance).
#[derive(Debug)]
pub struct SettingsAccess<'a> {
    image: &'a [u8],
}

impl<'a> SettingsAccess<'a> {
    /// Create a new settings accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the raw system settings bytes (0x0000-0x1FFF).
    ///
    /// Returns `None` if the image is too small.
    #[must_use]
    pub fn as_raw(&self) -> Option<&[u8]> {
        let end = SETTINGS_OFFSET + SETTINGS_SIZE;
        self.image.get(SETTINGS_OFFSET..end)
    }

    /// Get the power-on message (`radio.PowerOnMessage`, up to 16 bytes).
    ///
    /// MCP offset `0x10C0`. The semantic text is printable ASCII
    /// (`0x20`-`0x7E`) and the remainder of the fixed field is NUL-padded.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the image is incomplete, a semantic
    /// byte is outside printable ASCII, or bytes after the first NUL are
    /// nonzero.
    pub fn power_on_message(&self) -> Result<PowerOnMessage, SettingsValueError> {
        let end = POWER_ON_MESSAGE_OFFSET + POWER_ON_MESSAGE_SIZE;
        let bytes = self.image.get(POWER_ON_MESSAGE_OFFSET..end).ok_or(
            SettingsValueError::MissingByte {
                setting: "radio.PowerOnMessage",
                offset: end - 1,
            },
        )?;
        let wire = <[u8; PowerOnMessage::WIRE_LEN]>::try_from(bytes).map_err(|_| {
            SettingsValueError::MissingByte {
                setting: "radio.PowerOnMessage",
                offset: end - 1,
            }
        })?;

        PowerOnMessage::try_from_wire(wire).map_err(|error| match error {
            ValidationError::InvalidPowerOnMessageByte { offset, value } => {
                SettingsValueError::InvalidValue {
                    setting: "radio.PowerOnMessage",
                    offset: POWER_ON_MESSAGE_OFFSET + offset,
                    value,
                    detail: "text bytes must be printable ASCII 0x20-0x7E",
                }
            }
            ValidationError::PowerOnMessageDataAfterNul { offset, value, .. } => {
                SettingsValueError::InvalidValue {
                    setting: "radio.PowerOnMessage",
                    offset: POWER_ON_MESSAGE_OFFSET + offset,
                    value,
                    detail: "bytes after the first NUL must be NUL padding",
                }
            }
            ValidationError::PowerOnMessageTooLong { .. } => SettingsValueError::InvalidValue {
                setting: "radio.PowerOnMessage",
                offset: POWER_ON_MESSAGE_OFFSET,
                value: 0,
                detail: "text exceeds the fixed 16-byte field",
            },
            _ => SettingsValueError::InvalidValue {
                setting: "radio.PowerOnMessage",
                offset: POWER_ON_MESSAGE_OFFSET,
                value: 0,
                detail: "text is outside the power-on message domain",
            },
        })
    }

    /// Read an arbitrary byte range from the settings region.
    ///
    /// The offset is relative to the start of the image (MCP byte
    /// address). Returns `None` if the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.image.get(offset..end)
    }

    fn setting_byte(&self, setting: &'static str, offset: usize) -> Result<u8, SettingsValueError> {
        self.image
            .get(offset)
            .copied()
            .ok_or(SettingsValueError::MissingByte { setting, offset })
    }

    fn setting_value<T>(
        &self,
        setting: &'static str,
        offset: usize,
        detail: &'static str,
    ) -> Result<T, SettingsValueError>
    where
        T: TryFrom<u8>,
    {
        let raw = self.setting_byte(setting, offset)?;
        T::try_from(raw).map_err(|_| SettingsValueError::InvalidValue {
            setting,
            offset,
            value: raw,
            detail,
        })
    }

    fn setting_bool(
        &self,
        setting: &'static str,
        offset: usize,
    ) -> Result<bool, SettingsValueError> {
        let raw = self.setting_byte(setting, offset)?;
        match raw {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SettingsValueError::InvalidValue {
                setting,
                offset,
                value: raw,
                detail: "must be 0 or 1",
            }),
        }
    }

    fn setting_bit(
        &self,
        setting: &'static str,
        offset: usize,
        mask: u8,
    ) -> Result<bool, SettingsValueError> {
        Ok(self.setting_byte(setting, offset)? & mask != 0)
    }

    // -----------------------------------------------------------------------
    // TX/RX
    // -----------------------------------------------------------------------

    /// Read the beat-shift type (`radio.BeatShift`).
    ///
    /// MCP offset `0x1000`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn beat_shift(&self) -> Result<BeatShift, SettingsValueError> {
        self.setting_value("radio.BeatShift", BEAT_SHIFT_OFFSET, "must be 0-7")
    }

    /// Read TX inhibit (`radio.TxInhibit`).
    ///
    /// MCP offset `0x1001`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn tx_inhibit(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.TxInhibit", TX_INHIBIT_OFFSET)
    }

    /// Read the TX timeout selection (`radio.TimeOutTimer`).
    ///
    /// MCP offset `0x1003`. Indexes the table 0.5, 1.0, 1.5, 2.0, 2.5,
    /// 3.0, 3.5, 4.0, 4.5, 5.0, 10.0 minutes, NOT a minute count.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn timeout_timer(&self) -> Result<TransmitTimeout, SettingsValueError> {
        self.setting_value("radio.TimeOutTimer", TIMEOUT_TIMER_OFFSET, "must be 0-10")
    }

    /// Read microphone sensitivity (`radio.MicSensitivity`).
    ///
    /// MCP offset `0x1006`. The encoding is inverted versus intuition:
    /// **0=High, 1=Medium, 2=Low**.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn mic_sensitivity(
        &self,
    ) -> Result<crate::types::settings::MicSensitivity, SettingsValueError> {
        self.setting_value(
            "radio.MicSensitivity",
            MIC_SENSITIVITY_OFFSET,
            "must be 0-2",
        )
    }

    /// Read the SSB high-cut filter (`radio.SsbHighCut`).
    ///
    /// MCP offset `0x1008`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn ssb_high_cut(&self) -> Result<SsbHighCut, SettingsValueError> {
        self.setting_value("radio.SsbHighCut", SSB_HIGH_CUT_OFFSET, "must be 0-4")
    }

    /// Read the CW filter width (`radio.CwWidth`).
    ///
    /// MCP offset `0x1009`. The D75 menu calls this "CW Width" (it was
    /// previously misnamed "CW high cut" here).
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn cw_width(&self) -> Result<CwFilterWidth, SettingsValueError> {
        self.setting_value("radio.CwWidth", CW_WIDTH_OFFSET, "must be 0-4")
    }

    /// Read the AM high-cut filter (`radio.AmHighCut`).
    ///
    /// MCP offset `0x100A`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn am_high_cut(&self) -> Result<AmHighCut, SettingsValueError> {
        self.setting_value("radio.AmHighCut", AM_HIGH_CUT_OFFSET, "must be 0-3")
    }

    // -----------------------------------------------------------------------
    // Scan
    // -----------------------------------------------------------------------

    /// Read the analog scan-resume method (`radio.ScanResumeAnalog`).
    ///
    /// MCP offset `0x100C`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn scan_resume(&self) -> Result<ScanResumeMethod, SettingsValueError> {
        self.setting_value("radio.ScanResumeAnalog", SCAN_RESUME_OFFSET, "must be 0-2")
    }

    /// Read the digital scan-resume method (`radio.ScanResumeDigital`).
    ///
    /// MCP offset `0x100D`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn digital_scan_resume(&self) -> Result<ScanResumeMethod, SettingsValueError> {
        self.setting_value(
            "radio.ScanResumeDigital",
            DIGITAL_SCAN_RESUME_OFFSET,
            "must be 0-2",
        )
    }

    /// Read the time-operated scan restart time (`radio.TimeRestart`).
    ///
    /// MCP offset `0x100E`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn scan_restart_time(&self) -> Result<ScanRestartDelay, SettingsValueError> {
        self.setting_value(
            "radio.TimeRestart",
            SCAN_RESTART_TIME_OFFSET,
            "must be 1-10 seconds",
        )
    }

    /// Read the carrier-operated scan restart time
    /// (`radio.CarrierRestart`).
    ///
    /// MCP offset `0x100F`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn scan_restart_carrier(&self) -> Result<ScanRestartDelay, SettingsValueError> {
        self.setting_value(
            "radio.CarrierRestart",
            SCAN_RESTART_CARRIER_OFFSET,
            "must be 1-10 seconds",
        )
    }

    // -----------------------------------------------------------------------
    // Repeater
    // -----------------------------------------------------------------------

    /// Read repeater auto offset (`radio.AutoOffset`).
    ///
    /// MCP offset `0x1018`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn repeater_auto_offset(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.AutoOffset", REPEATER_AUTO_OFFSET_OFFSET)
    }

    /// Read the CALL key function (`radio.CallKey`).
    ///
    /// MCP offset `0x1019`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn repeater_call_key(&self) -> Result<RepeaterCallKey, SettingsValueError> {
        self.setting_value("radio.CallKey", REPEATER_CALL_KEY_OFFSET, "must be 0-1")
    }

    // -----------------------------------------------------------------------
    // VOX
    // -----------------------------------------------------------------------

    /// Read VOX enabled (`radio.Vox`).
    ///
    /// MCP offset `0x101B`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn vox_enabled(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.Vox", VOX_ENABLED_OFFSET)
    }

    /// Read VOX gain (`radio.VoxGain`, 0-9).
    ///
    /// MCP offset `0x101C`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] rather than silently clamping an invalid
    /// stored byte or inventing a value for an incomplete image.
    pub fn vox_gain(&self) -> Result<VoxGain, SettingsValueError> {
        let raw = self.setting_byte("radio.VoxGain", VOX_GAIN_OFFSET)?;
        VoxGain::new(raw).map_err(|_| SettingsValueError::InvalidValue {
            setting: "radio.VoxGain",
            offset: VOX_GAIN_OFFSET,
            value: raw,
            detail: "must be 0-9",
        })
    }

    /// Read the VOX delay selection (`radio.VoxDelay`, raw index 0-6).
    ///
    /// MCP offset `0x101D`. Indexes the table 250, 500, 750, 1000,
    /// 1500, 2000, 3000 ms, NOT a 100 ms unit count.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] rather than silently clamping an invalid
    /// stored byte or inventing a value for an incomplete image.
    pub fn vox_delay(&self) -> Result<VoxDelay, SettingsValueError> {
        let raw = self.setting_byte("radio.VoxDelay", VOX_DELAY_OFFSET)?;
        VoxDelay::new(raw).map_err(|_| SettingsValueError::InvalidValue {
            setting: "radio.VoxDelay",
            offset: VOX_DELAY_OFFSET,
            value: raw,
            detail: "raw index must be 0-6",
        })
    }

    /// Read VOX TX-on-busy (`radio.VoxTxOnBusy`).
    ///
    /// MCP offset `0x101E`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn vox_tx_on_busy(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.VoxTxOnBusy", VOX_TX_ON_BUSY_OFFSET)
    }

    // -----------------------------------------------------------------------
    // DTMF
    // -----------------------------------------------------------------------

    /// Read DTMF tone duration (`radio.DtmfSpeed`).
    ///
    /// MCP offset `0x101F`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn dtmf_speed(&self) -> Result<DtmfToneDuration, SettingsValueError> {
        self.setting_value("radio.DtmfSpeed", DTMF_SPEED_OFFSET, "must be 0-2")
    }

    /// Read DTMF pause time (`radio.DtmfPauseTime`).
    ///
    /// MCP offset `0x1020`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn dtmf_pause_time(&self) -> Result<DtmfPause, SettingsValueError> {
        let raw = self.setting_byte("radio.DtmfPauseTime", DTMF_PAUSE_TIME_OFFSET)?;
        match raw {
            0 => Ok(DtmfPause::Ms100),
            1 => Ok(DtmfPause::Ms250),
            2 => Ok(DtmfPause::Ms500),
            3 => Ok(DtmfPause::Ms750),
            4 => Ok(DtmfPause::Ms1000),
            5 => Ok(DtmfPause::Ms1500),
            6 => Ok(DtmfPause::Ms2000),
            _ => Err(SettingsValueError::InvalidValue {
                setting: "radio.DtmfPauseTime",
                offset: DTMF_PAUSE_TIME_OFFSET,
                value: raw,
                detail: "must be 0-6",
            }),
        }
    }

    /// Read DTMF TX hold (`radio.DtmfTxHold`).
    ///
    /// MCP offset `0x1021`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn dtmf_tx_hold(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.DtmfTxHold", DTMF_TX_HOLD_OFFSET)
    }

    // -----------------------------------------------------------------------
    // CW receive
    // -----------------------------------------------------------------------

    /// Read the CW pitch (`radio.CwPitchFreq`).
    ///
    /// MCP offset `0x1024`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn cw_pitch(&self) -> Result<CwPitch, SettingsValueError> {
        let raw = self.setting_byte("radio.CwPitchFreq", CW_PITCH_OFFSET)?;
        let hz = 400 + u16::from(raw) * 100;
        CwPitch::new(hz).map_err(|_| SettingsValueError::InvalidValue {
            setting: "radio.CwPitchFreq",
            offset: CW_PITCH_OFFSET,
            value: raw,
            detail: "must be 0-6 (400-1000 Hz)",
        })
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    /// Read the auto-mute return time (`radio.AutoMuteRetTime`).
    ///
    /// MCP offset `0x1041`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn auto_mute_return_time(&self) -> Result<AutoMuteReturnDelay, SettingsValueError> {
        self.setting_value(
            "radio.AutoMuteRetTime",
            AUTO_MUTE_RETURN_TIME_OFFSET,
            "must be 1-10 seconds",
        )
    }

    /// Read the EMR volume level (`dv.EmrVolumeLevelTxRx`).
    ///
    /// MCP offset `0x1A03`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn emr_volume_level(&self) -> Result<EmrVolume, SettingsValueError> {
        let raw = self.setting_byte("dv.EmrVolumeLevelTxRx", EMR_VOLUME_LEVEL_OFFSET)?;
        EmrVolume::new(raw).map_err(|_| SettingsValueError::InvalidValue {
            setting: "dv.EmrVolumeLevelTxRx",
            offset: EMR_VOLUME_LEVEL_OFFSET,
            value: raw,
            detail: "must be 1-50",
        })
    }

    // -----------------------------------------------------------------------
    // Display
    // -----------------------------------------------------------------------

    /// Read backlight control (`radio.BacklightControl`).
    ///
    /// MCP offset `0x1060`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn backlight_control(&self) -> Result<BacklightControl, SettingsValueError> {
        self.setting_value(
            "radio.BacklightControl",
            BACKLIGHT_CONTROL_OFFSET,
            "must be 0-3",
        )
    }

    /// Read the backlight timer (`radio.BacklightTimer`).
    ///
    /// MCP offset `0x1061`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn backlight_timer(&self) -> Result<BacklightTimer, SettingsValueError> {
        self.setting_value(
            "radio.BacklightTimer",
            BACKLIGHT_TIMER_OFFSET,
            "must be 3-60 seconds",
        )
    }

    // -----------------------------------------------------------------------
    // Beep / voice guidance
    // -----------------------------------------------------------------------

    /// Read key beep (`radio.Beep`).
    ///
    /// MCP offset `0x1071`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn key_beep(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.Beep", KEY_BEEP_OFFSET)
    }

    /// Read beep volume (`radio.BeepVolume`).
    ///
    /// MCP offset `0x1072`. **0 is a legal value meaning "VOL Link"**
    /// (beep follows the main volume); 1-7 are fixed levels.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn beep_volume(&self) -> Result<LinkedVolumeLevel, SettingsValueError> {
        self.setting_value(
            "radio.BeepVolume",
            BEEP_VOLUME_OFFSET,
            "must be 0-7, where 0 means VOL Link",
        )
    }

    /// Read the voice announce mode (`radio.VoiceAnnounce`).
    ///
    /// MCP offset `0x1073`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn announce(&self) -> Result<VoiceAnnounceMode, SettingsValueError> {
        self.setting_value("radio.VoiceAnnounce", ANNOUNCE_OFFSET, "must be 0-3")
    }

    /// Read the voice announce volume (`radio.VoiceAnnounceVolume`).
    ///
    /// MCP offset `0x1074`. **0 is a legal value meaning "VOL Link"**;
    /// 1-7 are fixed levels.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn voice_volume(&self) -> Result<LinkedVolumeLevel, SettingsValueError> {
        self.setting_value(
            "radio.VoiceAnnounceVolume",
            VOICE_VOLUME_OFFSET,
            "must be 0-7, where 0 means VOL Link",
        )
    }

    /// Read the voice guidance speed (`radio.VoiceGuidanceSpeed`).
    ///
    /// MCP offset `0x1097`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn voice_speed(&self) -> Result<VoiceGuideSpeed, SettingsValueError> {
        self.setting_value(
            "radio.VoiceGuidanceSpeed",
            VOICE_SPEED_OFFSET,
            "must be 0-3",
        )
    }

    // -----------------------------------------------------------------------
    // Battery / power
    // -----------------------------------------------------------------------

    /// Read the battery saver interval (`radio.BatterySaver`).
    ///
    /// MCP offset `0x1076`. This is a 10-value selector, not an on/off
    /// switch.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn battery_saver(&self) -> Result<BatterySaverInterval, SettingsValueError> {
        self.setting_value("radio.BatterySaver", BATTERY_SAVER_OFFSET, "must be 0-9")
    }

    /// Read auto power off (`radio.AutoPowerOff`).
    ///
    /// MCP offset `0x1077`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn auto_power_off(&self) -> Result<AutoPowerOff, SettingsValueError> {
        self.setting_value("radio.AutoPowerOff", AUTO_POWER_OFF_OFFSET, "must be 0-3")
    }

    // -----------------------------------------------------------------------
    // Bluetooth
    // -----------------------------------------------------------------------

    /// Read Bluetooth on/off (`radio.BluetoothOnOff`).
    ///
    /// MCP offset `0x1078`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn bluetooth(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.BluetoothOnOff", BLUETOOTH_OFFSET)
    }

    /// Read Bluetooth auto-connect (`radio.BluetoothAutoConnect`).
    ///
    /// MCP offset `0x1079`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn bt_auto_connect(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.BluetoothAutoConnect", BT_AUTO_CONNECT_OFFSET)
    }

    // -----------------------------------------------------------------------
    // PF keys
    // -----------------------------------------------------------------------

    /// Read the exact stored PF1 key assignment (`radio.Pf1PfKey`).
    ///
    /// MCP offset `0x107A`. The official writable domain is 0-30 with
    /// gaps, but hardware probes may leave a known off-menu function code
    /// such as 31 (Screen Capture). Reads preserve such values exactly;
    /// [`SettingsWriter::set_pf_key1`] accepts only official assignments.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing.
    pub fn pf_key1(&self) -> Result<StoredFrontPanelPfAssignment, SettingsValueError> {
        self.setting_byte("radio.Pf1PfKey", PF_KEY1_OFFSET)
            .map(StoredFrontPanelPfAssignment::from)
    }

    /// Read the exact stored PF2 key assignment (`radio.Pf2PfKey`).
    ///
    /// MCP offset `0x107B`. Reads preserve values outside the official
    /// writable domain so diagnostics cannot silently misreport them.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing.
    pub fn pf_key2(&self) -> Result<StoredFrontPanelPfAssignment, SettingsValueError> {
        self.setting_byte("radio.Pf2PfKey", PF_KEY2_OFFSET)
            .map(StoredFrontPanelPfAssignment::from)
    }

    // -----------------------------------------------------------------------
    // Locks
    // -----------------------------------------------------------------------

    /// Read the key-lock configuration bit
    /// (`radio.KeyLockTypeKeyLock`).
    ///
    /// MCP offset `0x1084`, bit `0x01`. This is the "Key Lock"
    /// checkbox of the lock-type menu, not the runtime lock state (the
    /// latter is CAT `LC`/`DL`).
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn key_lock(&self) -> Result<bool, SettingsValueError> {
        self.setting_bit(
            "radio.KeyLockTypeKeyLock",
            KEY_LOCK_OFFSET,
            KEY_LOCK_KEY_MASK,
        )
    }

    /// Read the frequency-lock configuration bit
    /// (`radio.KeyLockTypeFrequencyLock`; false if unreadable).
    ///
    /// MCP offset `0x1084`, bit `0x02`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn frequency_lock(&self) -> Result<bool, SettingsValueError> {
        self.setting_bit(
            "radio.KeyLockTypeFrequencyLock",
            KEY_LOCK_OFFSET,
            KEY_LOCK_FREQUENCY_MASK,
        )
    }

    /// Read volume lock (`radio.VolumeLockOnOff`).
    ///
    /// MCP offset `0x1087`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn volume_lock(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("radio.VolumeLockOnOff", VOLUME_LOCK_OFFSET)
    }

    /// Read the APRS-lock frequency bit (`aprs.Frequency`).
    ///
    /// MCP offset `0x120A`, bit `0x01`. Part of the APRS key-lock
    /// checkbox set.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn aprs_lock_frequency(&self) -> Result<bool, SettingsValueError> {
        self.setting_bit("aprs.Frequency", APRS_LOCK_OFFSET, APRS_LOCK_FREQUENCY_MASK)
    }

    /// Read the APRS-lock PTT bit (`aprs.Ptt`; false if unreadable).
    ///
    /// MCP offset `0x120A`, bit `0x02`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn aprs_lock_ptt(&self) -> Result<bool, SettingsValueError> {
        self.setting_bit("aprs.Ptt", APRS_LOCK_OFFSET, APRS_LOCK_PTT_MASK)
    }

    /// Read the APRS-lock APRS-key bit (`aprs.AprsKey`; false if
    /// unreadable).
    ///
    /// MCP offset `0x120A`, bit `0x04`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn aprs_lock_key(&self) -> Result<bool, SettingsValueError> {
        self.setting_bit("aprs.AprsKey", APRS_LOCK_OFFSET, APRS_LOCK_KEY_MASK)
    }

    // -----------------------------------------------------------------------
    // Units / language
    // -----------------------------------------------------------------------

    /// Read display unit settings (`radio.SpeedDistance`,
    /// `radio.AltitudeRain`, `radio.Temperature`, `radio.LatitudeLongitude`,
    /// and `radio.GridSquareFormat`).
    ///
    /// MCP offsets `0x1088` through `0x108C`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if any byte is missing or invalid.
    pub fn display_units(&self) -> Result<DisplayUnits, SettingsValueError> {
        Ok(DisplayUnits {
            speed_distance: self.setting_value(
                "radio.SpeedDistance",
                SPEED_DISTANCE_UNIT_OFFSET,
                "must be 0-2",
            )?,
            altitude_rain: self.setting_value(
                "radio.AltitudeRain",
                ALTITUDE_RAIN_UNIT_OFFSET,
                "must be 0-1",
            )?,
            temperature: self.setting_value(
                "radio.Temperature",
                TEMPERATURE_UNIT_OFFSET,
                "must be 0-1",
            )?,
            coordinates: self.setting_value(
                "radio.LatitudeLongitude",
                COORDINATE_FORMAT_OFFSET,
                "must be 0-1",
            )?,
            grid_square: self.setting_value(
                "radio.GridSquareFormat",
                GRID_SQUARE_FORMAT_OFFSET,
                "must be 0-2",
            )?,
        })
    }

    /// Read language (`radio.Language`).
    ///
    /// MCP offset `0x1092`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn language(&self) -> Result<Language, SettingsValueError> {
        self.setting_value("radio.Language", LANGUAGE_OFFSET, "must be 0-1")
    }

    // -----------------------------------------------------------------------
    // Interfaces
    // -----------------------------------------------------------------------

    /// Read the GPS PC-output interface (`radio.PcOutputInterfaceGps`).
    ///
    /// MCP offset `0x108E`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn gps_pc_output_interface(&self) -> Result<PcOutputInterface, SettingsValueError> {
        self.setting_value(
            "radio.PcOutputInterfaceGps",
            GPS_BT_INTERFACE_OFFSET,
            "must be 0-1",
        )
    }

    /// Read the APRS PC-output interface
    /// (`radio.PcOutputInterfaceAprs`).
    ///
    /// MCP offset `0x108F`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn aprs_pc_output_interface(&self) -> Result<PcOutputInterface, SettingsValueError> {
        self.setting_value(
            "radio.PcOutputInterfaceAprs",
            APRS_USB_MODE_OFFSET,
            "must be 0-1",
        )
    }

    // -----------------------------------------------------------------------
    // Band state (hardware-verified, outside registry scope)
    // -----------------------------------------------------------------------

    /// Read Band A power level.
    ///
    /// MCP offset `0x0359`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or invalid.
    pub fn power_level_a(&self) -> Result<PowerLevel, SettingsValueError> {
        self.setting_value("Band A power level", POWER_LEVEL_A_OFFSET, "must be 0-3")
    }

    /// Read Band A attenuator setting (0=off, 1=on).
    ///
    /// MCP offset `0x035C`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn attenuator_a(&self) -> Result<bool, SettingsValueError> {
        self.setting_bool("Band A attenuator", ATTENUATOR_A_OFFSET)
    }

    /// Read dual-band display setting (0=single, 1=dual).
    ///
    /// MCP offset `0x0396`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the byte is missing or is not 0 or 1.
    pub fn band_mode(&self) -> Result<BandMode, SettingsValueError> {
        self.setting_bool("Dual-band display", DUAL_BAND_OFFSET)
            .map(|dual| {
                if dual {
                    BandMode::Dual
                } else {
                    BandMode::Single
                }
            })
    }
}

// ---------------------------------------------------------------------------
// SettingsWriter (mutable access)
// ---------------------------------------------------------------------------

/// Mutable access to the system settings region of the memory image.
///
/// Every write method targets a registry-verified offset (or one of
/// the three hardware-verified band-state cells) and accepts a value whose
/// type already proves the registry domain. An incomplete image produces an
/// explicit error instead of silently dropping the write.
/// Setters for shared bit bytes (`0x1084`, `0x120A`) perform masked
/// read-modify-write and never touch bits owned by other fields.
#[derive(Debug)]
pub struct SettingsWriter<'a> {
    image: &'a mut [u8],
}

impl<'a> SettingsWriter<'a> {
    /// Create a new mutable settings accessor.
    pub(crate) const fn new(image: &'a mut [u8]) -> Self {
        Self { image }
    }

    /// Write `value` to the byte at `offset`.
    fn put(
        &mut self,
        setting: &'static str,
        offset: usize,
        value: u8,
    ) -> Result<(), SettingsValueError> {
        let byte = self
            .image
            .get_mut(offset)
            .ok_or(SettingsValueError::MissingByte { setting, offset })?;
        *byte = value;
        Ok(())
    }

    /// Set or clear `mask` within the shared bit byte at `offset`,
    /// preserving all other bits (masked read-modify-write).
    fn put_bit(
        &mut self,
        setting: &'static str,
        offset: usize,
        mask: u8,
        enabled: bool,
    ) -> Result<(), SettingsValueError> {
        let byte = self
            .image
            .get_mut(offset)
            .ok_or(SettingsValueError::MissingByte { setting, offset })?;
        if enabled {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
        Ok(())
    }

    /// Set the power-on message (`radio.PowerOnMessage`).
    ///
    /// MCP offset `0x10C0`. The validated printable ASCII text is followed by
    /// NUL padding through the end of the 16-byte field.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the complete target field is missing.
    pub fn set_power_on_message(
        &mut self,
        value: &PowerOnMessage,
    ) -> Result<(), SettingsValueError> {
        let end = POWER_ON_MESSAGE_OFFSET + POWER_ON_MESSAGE_SIZE;
        let field = self.image.get_mut(POWER_ON_MESSAGE_OFFSET..end).ok_or(
            SettingsValueError::MissingByte {
                setting: "radio.PowerOnMessage",
                offset: end - 1,
            },
        )?;
        field.copy_from_slice(&value.to_wire_bytes());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // TX/RX
    // -----------------------------------------------------------------------

    /// Set the beat-shift type (`radio.BeatShift`).
    ///
    /// MCP offset `0x1000`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_beat_shift(&mut self, value: BeatShift) -> Result<(), SettingsValueError> {
        self.put("radio.BeatShift", BEAT_SHIFT_OFFSET, u8::from(value))
    }

    /// Set TX inhibit on/off (`radio.TxInhibit`).
    ///
    /// MCP offset `0x1001`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_tx_inhibit(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put("radio.TxInhibit", TX_INHIBIT_OFFSET, u8::from(enabled))
    }

    /// Set the TX timeout timer (`radio.TimeOutTimer`).
    ///
    /// MCP offset `0x1003`. The value indexes the 0.5-10.0 minute
    /// table (see [`SettingsAccess::timeout_timer`]).
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_timeout_timer(&mut self, value: TransmitTimeout) -> Result<(), SettingsValueError> {
        self.put("radio.TimeOutTimer", TIMEOUT_TIMER_OFFSET, u8::from(value))
    }

    /// Set microphone sensitivity (`radio.MicSensitivity`).
    ///
    /// MCP offset `0x1006`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_mic_sensitivity(
        &mut self,
        value: crate::types::settings::MicSensitivity,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.MicSensitivity",
            MIC_SENSITIVITY_OFFSET,
            u8::from(value),
        )
    }

    /// Set the SSB high-cut filter (`radio.SsbHighCut`).
    ///
    /// MCP offset `0x1008`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_ssb_high_cut(&mut self, value: SsbHighCut) -> Result<(), SettingsValueError> {
        self.put("radio.SsbHighCut", SSB_HIGH_CUT_OFFSET, u8::from(value))
    }

    /// Set the CW filter width (`radio.CwWidth`).
    ///
    /// MCP offset `0x1009`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_cw_width(&mut self, value: CwFilterWidth) -> Result<(), SettingsValueError> {
        self.put("radio.CwWidth", CW_WIDTH_OFFSET, u8::from(value))
    }

    /// Set the AM high-cut filter (`radio.AmHighCut`).
    ///
    /// MCP offset `0x100A`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_am_high_cut(&mut self, value: AmHighCut) -> Result<(), SettingsValueError> {
        self.put("radio.AmHighCut", AM_HIGH_CUT_OFFSET, u8::from(value))
    }

    // -----------------------------------------------------------------------
    // Scan
    // -----------------------------------------------------------------------

    /// Set the analog scan-resume method (`radio.ScanResumeAnalog`).
    ///
    /// MCP offset `0x100C`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_scan_resume(&mut self, value: ScanResumeMethod) -> Result<(), SettingsValueError> {
        self.put("radio.ScanResumeAnalog", SCAN_RESUME_OFFSET, value.as_raw())
    }

    /// Set the digital scan-resume method (`radio.ScanResumeDigital`).
    ///
    /// MCP offset `0x100D`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_digital_scan_resume(
        &mut self,
        value: ScanResumeMethod,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.ScanResumeDigital",
            DIGITAL_SCAN_RESUME_OFFSET,
            value.as_raw(),
        )
    }

    /// Set the time-operated scan restart time (`radio.TimeRestart`).
    ///
    /// MCP offset `0x100E`. 0 is not a legal value for this field.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_scan_restart_time(
        &mut self,
        value: ScanRestartDelay,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.TimeRestart",
            SCAN_RESTART_TIME_OFFSET,
            u8::from(value),
        )
    }

    /// Set the carrier-operated scan restart time
    /// (`radio.CarrierRestart`).
    ///
    /// MCP offset `0x100F`. 0 is not a legal value for this field.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_scan_restart_carrier(
        &mut self,
        value: ScanRestartDelay,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.CarrierRestart",
            SCAN_RESTART_CARRIER_OFFSET,
            u8::from(value),
        )
    }

    // -----------------------------------------------------------------------
    // Repeater
    // -----------------------------------------------------------------------

    /// Set repeater auto offset on/off (`radio.AutoOffset`).
    ///
    /// MCP offset `0x1018`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_repeater_auto_offset(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put(
            "radio.AutoOffset",
            REPEATER_AUTO_OFFSET_OFFSET,
            u8::from(enabled),
        )
    }

    /// Set the CALL key function (`radio.CallKey`).
    ///
    /// MCP offset `0x1019`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_repeater_call_key(
        &mut self,
        value: RepeaterCallKey,
    ) -> Result<(), SettingsValueError> {
        self.put("radio.CallKey", REPEATER_CALL_KEY_OFFSET, u8::from(value))
    }

    // -----------------------------------------------------------------------
    // VOX
    // -----------------------------------------------------------------------

    /// Set VOX enabled on/off (`radio.Vox`).
    ///
    /// MCP offset `0x101B`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_vox_enabled(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put("radio.Vox", VOX_ENABLED_OFFSET, u8::from(enabled))
    }

    /// Set VOX gain level (`radio.VoxGain`, 0-9).
    ///
    /// MCP offset `0x101C`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_vox_gain(&mut self, gain: VoxGain) -> Result<(), SettingsValueError> {
        self.put("radio.VoxGain", VOX_GAIN_OFFSET, gain.as_raw())
    }

    /// Set the VOX delay selection (`radio.VoxDelay`, raw index 0-6).
    ///
    /// MCP offset `0x101D`. The value indexes the 250-3000 ms table
    /// (see [`SettingsAccess::vox_delay`]); it is NOT a 100 ms unit
    /// count.
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_vox_delay(&mut self, delay: VoxDelay) -> Result<(), SettingsValueError> {
        self.put("radio.VoxDelay", VOX_DELAY_OFFSET, delay.as_raw())
    }

    /// Set VOX TX-on-busy on/off (`radio.VoxTxOnBusy`).
    ///
    /// MCP offset `0x101E`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_vox_tx_on_busy(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put(
            "radio.VoxTxOnBusy",
            VOX_TX_ON_BUSY_OFFSET,
            u8::from(enabled),
        )
    }

    // -----------------------------------------------------------------------
    // DTMF
    // -----------------------------------------------------------------------

    /// Set DTMF tone duration (`radio.DtmfSpeed`).
    ///
    /// MCP offset `0x101F`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_dtmf_speed(&mut self, value: DtmfToneDuration) -> Result<(), SettingsValueError> {
        self.put("radio.DtmfSpeed", DTMF_SPEED_OFFSET, u8::from(value))
    }

    /// Set DTMF pause time (`radio.DtmfPauseTime`).
    ///
    /// MCP offset `0x1020`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_dtmf_pause_time(&mut self, value: DtmfPause) -> Result<(), SettingsValueError> {
        let raw = match value {
            DtmfPause::Ms100 => 0,
            DtmfPause::Ms250 => 1,
            DtmfPause::Ms500 => 2,
            DtmfPause::Ms750 => 3,
            DtmfPause::Ms1000 => 4,
            DtmfPause::Ms1500 => 5,
            DtmfPause::Ms2000 => 6,
        };
        self.put("radio.DtmfPauseTime", DTMF_PAUSE_TIME_OFFSET, raw)
    }

    /// Set DTMF TX hold on/off (`radio.DtmfTxHold`).
    ///
    /// MCP offset `0x1021`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_dtmf_tx_hold(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put("radio.DtmfTxHold", DTMF_TX_HOLD_OFFSET, u8::from(enabled))
    }

    // -----------------------------------------------------------------------
    // CW receive
    // -----------------------------------------------------------------------

    /// Set the CW pitch (`radio.CwPitchFreq`).
    ///
    /// MCP offset `0x1024`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_cw_pitch(&mut self, value: CwPitch) -> Result<(), SettingsValueError> {
        let raw = u8::try_from((value.as_hz() - CwPitch::MIN_HZ) / CwPitch::STEP_HZ)
            .unwrap_or_else(|_| unreachable!("validated CW pitch index fits in one byte"));
        self.put("radio.CwPitchFreq", CW_PITCH_OFFSET, raw)
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    /// Set the auto-mute return time (`radio.AutoMuteRetTime`).
    ///
    /// MCP offset `0x1041`. 0 is not a legal value for this field.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_auto_mute_return_time(
        &mut self,
        value: AutoMuteReturnDelay,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.AutoMuteRetTime",
            AUTO_MUTE_RETURN_TIME_OFFSET,
            u8::from(value),
        )
    }

    /// Set the EMR volume level (`dv.EmrVolumeLevelTxRx`).
    ///
    /// MCP offset `0x1A03`. 0 is not a legal value for this field.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_emr_volume_level(&mut self, value: EmrVolume) -> Result<(), SettingsValueError> {
        self.put(
            "dv.EmrVolumeLevelTxRx",
            EMR_VOLUME_LEVEL_OFFSET,
            value.as_raw(),
        )
    }

    // -----------------------------------------------------------------------
    // Display
    // -----------------------------------------------------------------------

    /// Set backlight control (`radio.BacklightControl`).
    ///
    /// MCP offset `0x1060`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_backlight_control(
        &mut self,
        value: BacklightControl,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.BacklightControl",
            BACKLIGHT_CONTROL_OFFSET,
            u8::from(value),
        )
    }

    /// Set the backlight timer (`radio.BacklightTimer`).
    ///
    /// MCP offset `0x1061`. Values below 3 are not legal for this
    /// field.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_backlight_timer(&mut self, value: BacklightTimer) -> Result<(), SettingsValueError> {
        self.put(
            "radio.BacklightTimer",
            BACKLIGHT_TIMER_OFFSET,
            u8::from(value),
        )
    }

    // -----------------------------------------------------------------------
    // Beep / voice guidance
    // -----------------------------------------------------------------------

    /// Set key beep on/off (`radio.Beep`).
    ///
    /// MCP offset `0x1071`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_key_beep(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put("radio.Beep", KEY_BEEP_OFFSET, u8::from(enabled))
    }

    /// Set beep volume (`radio.BeepVolume`).
    ///
    /// MCP offset `0x1072`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_beep_volume(&mut self, volume: LinkedVolumeLevel) -> Result<(), SettingsValueError> {
        self.put("radio.BeepVolume", BEEP_VOLUME_OFFSET, u8::from(volume))
    }

    /// Set the voice announce mode (`radio.VoiceAnnounce`).
    ///
    /// MCP offset `0x1073`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_announce(&mut self, value: VoiceAnnounceMode) -> Result<(), SettingsValueError> {
        self.put("radio.VoiceAnnounce", ANNOUNCE_OFFSET, u8::from(value))
    }

    /// Set the voice announce volume (`radio.VoiceAnnounceVolume`).
    ///
    /// MCP offset `0x1074`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_voice_volume(&mut self, value: LinkedVolumeLevel) -> Result<(), SettingsValueError> {
        self.put(
            "radio.VoiceAnnounceVolume",
            VOICE_VOLUME_OFFSET,
            u8::from(value),
        )
    }

    /// Set the voice guidance speed (`radio.VoiceGuidanceSpeed`).
    ///
    /// MCP offset `0x1097`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_voice_speed(&mut self, value: VoiceGuideSpeed) -> Result<(), SettingsValueError> {
        self.put(
            "radio.VoiceGuidanceSpeed",
            VOICE_SPEED_OFFSET,
            u8::from(value),
        )
    }

    // -----------------------------------------------------------------------
    // Battery / power
    // -----------------------------------------------------------------------

    /// Set the battery saver interval (`radio.BatterySaver`).
    ///
    /// MCP offset `0x1076`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_battery_saver(
        &mut self,
        value: BatterySaverInterval,
    ) -> Result<(), SettingsValueError> {
        self.put("radio.BatterySaver", BATTERY_SAVER_OFFSET, u8::from(value))
    }

    /// Set auto power off (`radio.AutoPowerOff`).
    ///
    /// MCP offset `0x1077`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_auto_power_off(&mut self, value: AutoPowerOff) -> Result<(), SettingsValueError> {
        self.put("radio.AutoPowerOff", AUTO_POWER_OFF_OFFSET, u8::from(value))
    }

    // -----------------------------------------------------------------------
    // Bluetooth
    // -----------------------------------------------------------------------

    /// Set Bluetooth on/off (`radio.BluetoothOnOff`).
    ///
    /// MCP offset `0x1078`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_bluetooth(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put("radio.BluetoothOnOff", BLUETOOTH_OFFSET, u8::from(enabled))
    }

    /// Set Bluetooth auto-connect on/off
    /// (`radio.BluetoothAutoConnect`).
    ///
    /// MCP offset `0x1079`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_bt_auto_connect(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put(
            "radio.BluetoothAutoConnect",
            BT_AUTO_CONNECT_OFFSET,
            u8::from(enabled),
        )
    }

    // -----------------------------------------------------------------------
    // PF keys
    // -----------------------------------------------------------------------

    /// Set the PF1 key assignment (`radio.Pf1PfKey`).
    ///
    /// MCP offset `0x107A`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_pf_key1(&mut self, value: FrontPanelPfFunction) -> Result<(), SettingsValueError> {
        self.put("radio.Pf1PfKey", PF_KEY1_OFFSET, u8::from(value))
    }

    /// Set the PF2 key assignment (`radio.Pf2PfKey`).
    ///
    /// MCP offset `0x107B`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_pf_key2(&mut self, value: FrontPanelPfFunction) -> Result<(), SettingsValueError> {
        self.put("radio.Pf2PfKey", PF_KEY2_OFFSET, u8::from(value))
    }

    // -----------------------------------------------------------------------
    // Locks
    // -----------------------------------------------------------------------

    /// Set the key-lock configuration bit
    /// (`radio.KeyLockTypeKeyLock`).
    ///
    /// MCP offset `0x1084`, bit `0x01`. Masked read-modify-write: the
    /// frequency-lock bit and the six unowned bits of the byte are
    /// preserved.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn set_key_lock(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put_bit(
            "radio.KeyLockTypeKeyLock",
            KEY_LOCK_OFFSET,
            KEY_LOCK_KEY_MASK,
            enabled,
        )
    }

    /// Set the frequency-lock configuration bit
    /// (`radio.KeyLockTypeFrequencyLock`).
    ///
    /// MCP offset `0x1084`, bit `0x02`. Masked read-modify-write.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn set_frequency_lock(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put_bit(
            "radio.KeyLockTypeFrequencyLock",
            KEY_LOCK_OFFSET,
            KEY_LOCK_FREQUENCY_MASK,
            enabled,
        )
    }

    /// Set volume lock on/off (`radio.VolumeLockOnOff`).
    ///
    /// MCP offset `0x1087`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_volume_lock(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put(
            "radio.VolumeLockOnOff",
            VOLUME_LOCK_OFFSET,
            u8::from(enabled),
        )
    }

    /// Set the APRS-lock frequency bit (`aprs.Frequency`).
    ///
    /// MCP offset `0x120A`, bit `0x01`. Masked read-modify-write: the
    /// other APRS-lock bits and the five unowned bits are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn set_aprs_lock_frequency(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put_bit(
            "aprs.Frequency",
            APRS_LOCK_OFFSET,
            APRS_LOCK_FREQUENCY_MASK,
            enabled,
        )
    }

    /// Set the APRS-lock PTT bit (`aprs.Ptt`).
    ///
    /// MCP offset `0x120A`, bit `0x02`. Masked read-modify-write.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn set_aprs_lock_ptt(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put_bit("aprs.Ptt", APRS_LOCK_OFFSET, APRS_LOCK_PTT_MASK, enabled)
    }

    /// Set the APRS-lock APRS-key bit (`aprs.AprsKey`).
    ///
    /// MCP offset `0x120A`, bit `0x04`. Masked read-modify-write.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the shared byte is missing.
    pub fn set_aprs_lock_key(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put_bit(
            "aprs.AprsKey",
            APRS_LOCK_OFFSET,
            APRS_LOCK_KEY_MASK,
            enabled,
        )
    }

    // -----------------------------------------------------------------------
    // Units / language
    // -----------------------------------------------------------------------

    /// Set speed/distance display unit (`radio.SpeedDistance`).
    ///
    /// MCP offset `0x1088`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_speed_distance_unit(
        &mut self,
        value: SpeedDistanceUnit,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.SpeedDistance",
            SPEED_DISTANCE_UNIT_OFFSET,
            u8::from(value),
        )
    }

    /// Set altitude/rain display unit (`radio.AltitudeRain`).
    ///
    /// MCP offset `0x1089`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_altitude_rain_unit(
        &mut self,
        value: AltitudeRainUnit,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.AltitudeRain",
            ALTITUDE_RAIN_UNIT_OFFSET,
            u8::from(value),
        )
    }

    /// Set temperature display unit (`radio.Temperature`).
    ///
    /// MCP offset `0x108A`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_temperature_unit(
        &mut self,
        value: TemperatureUnit,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.Temperature",
            TEMPERATURE_UNIT_OFFSET,
            u8::from(value),
        )
    }

    /// Set latitude/longitude display notation (`radio.LatitudeLongitude`).
    ///
    /// MCP offset `0x108B`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_coordinate_format(
        &mut self,
        value: CoordinateFormat,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.LatitudeLongitude",
            COORDINATE_FORMAT_OFFSET,
            u8::from(value),
        )
    }

    /// Set grid-square display format (`radio.GridSquareFormat`).
    ///
    /// MCP offset `0x108C`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_grid_square_format(
        &mut self,
        value: GridSquareFormat,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.GridSquareFormat",
            GRID_SQUARE_FORMAT_OFFSET,
            u8::from(value),
        )
    }

    /// Set language (`radio.Language`).
    ///
    /// MCP offset `0x1092`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_language(&mut self, value: Language) -> Result<(), SettingsValueError> {
        self.put("radio.Language", LANGUAGE_OFFSET, u8::from(value))
    }

    // -----------------------------------------------------------------------
    // Interfaces
    // -----------------------------------------------------------------------

    /// Set the GPS PC-output interface (`radio.PcOutputInterfaceGps`).
    ///
    /// MCP offset `0x108E`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_gps_pc_output_interface(
        &mut self,
        value: PcOutputInterface,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.PcOutputInterfaceGps",
            GPS_BT_INTERFACE_OFFSET,
            u8::from(value),
        )
    }

    /// Set the APRS PC-output interface (`radio.PcOutputInterfaceAprs`).
    ///
    /// MCP offset `0x108F`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_aprs_pc_output_interface(
        &mut self,
        value: PcOutputInterface,
    ) -> Result<(), SettingsValueError> {
        self.put(
            "radio.PcOutputInterfaceAprs",
            APRS_USB_MODE_OFFSET,
            u8::from(value),
        )
    }

    // -----------------------------------------------------------------------
    // Band state (hardware-verified, outside registry scope)
    // -----------------------------------------------------------------------

    /// Set Band A power level.
    ///
    /// MCP offset `0x0359`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_power_level_a(&mut self, level: PowerLevel) -> Result<(), SettingsValueError> {
        self.put("Band A power level", POWER_LEVEL_A_OFFSET, u8::from(level))
    }

    /// Set Band A attenuator on/off.
    ///
    /// MCP offset `0x035C`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_attenuator_a(&mut self, enabled: bool) -> Result<(), SettingsValueError> {
        self.put("Band A attenuator", ATTENUATOR_A_OFFSET, u8::from(enabled))
    }

    /// Set single-band or dual-band display.
    ///
    /// MCP offset `0x0396`. Hardware verified.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsValueError`] if the target byte is missing.
    pub fn set_band_mode(&mut self, mode: BandMode) -> Result<(), SettingsValueError> {
        self.put(
            "Dual-band display",
            DUAL_BAND_OFFSET,
            u8::from(matches!(mode, BandMode::Dual)),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::menu_fields::{MCP_D75_MENU_FIELDS, menu_field};
    use crate::memory::schema::FieldCodec;
    use crate::protocol::programming::TOTAL_SIZE;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Write a single byte into a mutable slice, returning an error if the offset is out of range.
    /// Used by tests instead of direct `slice[idx] = val` (banned by workspace `indexing_slicing`).
    fn set_byte(
        image: &mut [u8],
        offset: usize,
        value: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let img_len = image.len();
        *image
            .get_mut(offset)
            .ok_or_else(|| format!("set_byte: offset {offset} out of range (len={img_len})"))? =
            value;
        Ok(())
    }

    /// Copy `data` into `image` starting at `offset`, returning an error if the range is out of bounds.
    /// Used by tests instead of direct `slice[a..b].copy_from_slice(...)` (banned by `indexing_slicing`).
    fn write_slice(
        image: &mut [u8],
        offset: usize,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let end = offset + data.len();
        let img_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("write_slice: range {offset}..{end} out of bounds (len={img_len})")
            })?
            .copy_from_slice(data);
        Ok(())
    }

    /// Read a single byte from `image` at `offset`, returning an error if out of range.
    /// Used by tests asserting raw bytes after writes.
    fn get_byte(image: &[u8], offset: usize) -> Result<u8, Box<dyn std::error::Error>> {
        image.get(offset).copied().ok_or_else(|| {
            format!(
                "get_byte: offset {offset} out of range (len={})",
                image.len()
            )
            .into()
        })
    }

    fn make_settings_image() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut image = vec![0x00_u8; TOTAL_SIZE];
        write_slice(
            &mut image,
            POWER_ON_MESSAGE_OFFSET,
            b"Hello D75!\0\0\0\0\0\0",
        )?;
        Ok(image)
    }

    #[test]
    fn settings_power_on_message() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let settings = mi.settings();
        assert_eq!(settings.power_on_message()?.as_str(), "Hello D75!");
        Ok(())
    }

    #[test]
    fn settings_raw_not_none() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let settings = mi.settings();
        let raw = settings
            .as_raw()
            .ok_or("settings raw slice unexpectedly None")?;
        assert_eq!(raw.len(), SETTINGS_SIZE);
        Ok(())
    }

    #[test]
    fn settings_read_bytes() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let settings = mi.settings();
        let bytes = settings
            .read_bytes(POWER_ON_MESSAGE_OFFSET, 10)
            .ok_or("read_bytes returned None for 10-byte power-on message")?;
        assert_eq!(
            bytes.get(..10).ok_or("read_bytes returned <10 bytes")?,
            b"Hello D75!"
        );
        assert!(
            settings.read_bytes(usize::MAX, 2).is_none(),
            "overflowing diagnostic ranges must fail instead of wrapping"
        );
        Ok(())
    }

    #[test]
    fn power_on_message_preserves_spaces_and_rejects_bad_padding() -> TestResult {
        let mut image = make_settings_image()?;
        write_slice(
            &mut image,
            POWER_ON_MESSAGE_OFFSET,
            b"  D75  \0\0\0\0\0\0\0\0\0",
        )?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().power_on_message()?.as_str(), "  D75  ");

        let mut image = make_settings_image()?;
        write_slice(
            &mut image,
            POWER_ON_MESSAGE_OFFSET,
            b"D75\0X\0\0\0\0\0\0\0\0\0\0\0",
        )?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().power_on_message(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.PowerOnMessage",
                offset,
                value: b'X',
                detail: "bytes after the first NUL must be NUL padding",
            }) if offset == POWER_ON_MESSAGE_OFFSET + 4
        ));
        Ok(())
    }

    #[test]
    fn power_on_message_read_rejects_non_printable_bytes_at_the_exact_offset() -> TestResult {
        for (relative_offset, value) in [(0, 0x1F), (1, 0x7F), (2, 0xFF)] {
            let mut image = make_settings_image()?;
            set_byte(&mut image, POWER_ON_MESSAGE_OFFSET + relative_offset, value)?;
            let mi = crate::memory::MemoryImage::from_raw(image)?;
            assert!(matches!(
                mi.settings().power_on_message(),
                Err(SettingsValueError::InvalidValue {
                    setting: "radio.PowerOnMessage",
                    offset,
                    value: actual_value,
                    detail: "text bytes must be printable ASCII 0x20-0x7E",
                }) if offset == POWER_ON_MESSAGE_OFFSET + relative_offset
                    && actual_value == value
            ));
        }
        Ok(())
    }

    #[test]
    fn strict_boolean_access_rejects_non_boolean_bytes() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, KEY_BEEP_OFFSET, 2)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().key_beep(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.Beep",
                offset: KEY_BEEP_OFFSET,
                value: 2,
                detail: "must be 0 or 1",
            })
        ));
        Ok(())
    }

    #[test]
    fn incomplete_images_return_explicit_read_and_write_errors() {
        let mut short = [0_u8; 1];
        assert!(matches!(
            SettingsAccess::new(&short).key_beep(),
            Err(SettingsValueError::MissingByte {
                setting: "radio.Beep",
                offset: KEY_BEEP_OFFSET,
            })
        ));
        assert!(matches!(
            SettingsWriter::new(&mut short).set_key_beep(true),
            Err(SettingsValueError::MissingByte {
                setting: "radio.Beep",
                offset: KEY_BEEP_OFFSET,
            })
        ));
        assert_eq!(short, [0]);
    }

    #[test]
    fn incomplete_power_on_message_write_is_atomic() -> TestResult {
        let end = POWER_ON_MESSAGE_OFFSET + POWER_ON_MESSAGE_SIZE;
        let mut short = vec![0xA5; end - 1];
        let before = short.clone();
        let message = PowerOnMessage::new("D75")?;
        assert!(matches!(
            SettingsWriter::new(&mut short).set_power_on_message(&message),
            Err(SettingsValueError::MissingByte {
                setting: "radio.PowerOnMessage",
                offset,
            }) if offset == end - 1
        ));
        assert_eq!(short, before, "failed fixed-field writes must be atomic");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Read accessor tests
    // -----------------------------------------------------------------------

    #[test]
    fn settings_key_beep() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, KEY_BEEP_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().key_beep()?);
        Ok(())
    }

    #[test]
    fn settings_key_beep_off() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.settings().key_beep()?);
        Ok(())
    }

    #[test]
    fn settings_vox() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, VOX_ENABLED_OFFSET, 1)?;
        set_byte(&mut image, VOX_GAIN_OFFSET, 7)?;
        set_byte(&mut image, VOX_DELAY_OFFSET, 5)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let settings = mi.settings();
        assert!(settings.vox_enabled()?);
        assert_eq!(settings.vox_gain()?, VoxGain::new(7)?);
        assert_eq!(settings.vox_delay()?, VoxDelay::new(5)?);
        Ok(())
    }

    #[test]
    fn settings_vox_gain_rejects_invalid_stored_value() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, VOX_GAIN_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().vox_gain(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.VoxGain",
                offset: VOX_GAIN_OFFSET,
                value: 0xFF,
                detail: "must be 0-9",
            })
        ));
        Ok(())
    }

    #[test]
    fn settings_vox_delay_rejects_invalid_stored_value() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, VOX_DELAY_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().vox_delay(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.VoxDelay",
                offset: VOX_DELAY_OFFSET,
                value: 0xFF,
                detail: "raw index must be 0-6",
            })
        ));
        Ok(())
    }

    #[test]
    fn settings_dual_band() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, DUAL_BAND_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().band_mode()?, BandMode::Dual);
        Ok(())
    }

    #[test]
    fn settings_dual_band_off() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().band_mode()?, BandMode::Single);
        Ok(())
    }

    #[test]
    fn settings_attenuator_a() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, ATTENUATOR_A_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().attenuator_a()?);
        Ok(())
    }

    #[test]
    fn settings_power_level_a() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, POWER_LEVEL_A_OFFSET, 2)?; // Lo
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().power_level_a()?, PowerLevel::Low);
        Ok(())
    }

    #[test]
    fn settings_power_level_a_rejects_invalid_stored_value() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, POWER_LEVEL_A_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().power_level_a(),
            Err(SettingsValueError::InvalidValue {
                setting: "Band A power level",
                offset: POWER_LEVEL_A_OFFSET,
                value: 0xFF,
                detail: "must be 0-3",
            })
        ));
        Ok(())
    }

    #[test]
    fn settings_bluetooth() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BLUETOOTH_OFFSET, 1)?;
        set_byte(&mut image, BT_AUTO_CONNECT_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().bluetooth()?);
        assert!(mi.settings().bt_auto_connect()?);
        Ok(())
    }

    #[test]
    fn settings_beep_volume_zero_is_vol_link() -> TestResult {
        // 0 is a legal stored value (VOL Link), not an error or a
        // clamped minimum.
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().beep_volume()?, LinkedVolumeLevel::VOLUME_LINK);
        Ok(())
    }

    #[test]
    fn settings_beep_volume_rejects_invalid_stored_value() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BEEP_VOLUME_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().beep_volume(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.BeepVolume",
                offset: BEEP_VOLUME_OFFSET,
                value: 0xFF,
                detail: "must be 0-7, where 0 means VOL Link",
            })
        ));
        Ok(())
    }

    #[test]
    fn settings_backlight_control() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BACKLIGHT_CONTROL_OFFSET, 2)?; // Auto
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().backlight_control()?, BacklightControl::Auto);
        Ok(())
    }

    #[test]
    fn settings_auto_power_off() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, AUTO_POWER_OFF_OFFSET, 2)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        // Raw 2 is 30 minutes on the D75 (0=Off, 1=15, 2=30, 3=60).
        assert_eq!(mi.settings().auto_power_off()?, AutoPowerOff::Min30);
        Ok(())
    }

    #[test]
    fn settings_auto_power_off_one_is_fifteen_minutes() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, AUTO_POWER_OFF_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().auto_power_off()?, AutoPowerOff::Min15);
        Ok(())
    }

    #[test]
    fn settings_auto_power_off_rejects_invalid_stored_value() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, AUTO_POWER_OFF_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().auto_power_off(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.AutoPowerOff",
                offset: AUTO_POWER_OFF_OFFSET,
                value: 0xFF,
                detail: "must be 0-3",
            })
        ));
        Ok(())
    }

    #[test]
    fn settings_battery_saver_is_an_interval_index() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BATTERY_SAVER_OFFSET, 9)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(
            mi.settings().battery_saver()?,
            BatterySaverInterval::Seconds5
        );
        Ok(())
    }

    #[test]
    fn settings_beat_shift() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BEAT_SHIFT_OFFSET, 7)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().beat_shift()?, BeatShift::Type8);
        Ok(())
    }

    #[test]
    fn settings_beat_shift_rejects_invalid_stored_value() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BEAT_SHIFT_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(matches!(
            mi.settings().beat_shift(),
            Err(SettingsValueError::InvalidValue {
                setting: "radio.BeatShift",
                offset: BEAT_SHIFT_OFFSET,
                value: 0xFF,
                detail: "must be 0-7",
            })
        ));
        Ok(())
    }

    #[test]
    fn settings_key_lock_bits() -> TestResult {
        let mut image = make_settings_image()?;
        // Both configuration bits set, plus unowned upper bits that
        // must not leak into either reader.
        set_byte(&mut image, KEY_LOCK_OFFSET, 0xFC | 0x01)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().key_lock()?);
        assert!(!mi.settings().frequency_lock()?);
        Ok(())
    }

    #[test]
    fn settings_aprs_lock_bits() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, APRS_LOCK_OFFSET, 0x05)?; // frequency + APRS key
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().aprs_lock_frequency()?);
        assert!(!mi.settings().aprs_lock_ptt()?);
        assert!(mi.settings().aprs_lock_key()?);
        Ok(())
    }

    #[test]
    fn settings_display_units() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, SPEED_DISTANCE_UNIT_OFFSET, 1)?; // km/h
        set_byte(&mut image, ALTITUDE_RAIN_UNIT_OFFSET, 1)?; // m/mm
        set_byte(&mut image, TEMPERATURE_UNIT_OFFSET, 1)?; // Celsius
        set_byte(&mut image, COORDINATE_FORMAT_OFFSET, 1)?; // DMS
        set_byte(&mut image, GRID_SQUARE_FORMAT_OFFSET, 2)?; // SAR CELL
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let units = mi.settings().display_units()?;
        assert_eq!(units.speed_distance, SpeedDistanceUnit::KilometersPerHour);
        assert_eq!(units.altitude_rain, AltitudeRainUnit::MetersMm);
        assert_eq!(units.temperature, TemperatureUnit::Celsius);
        assert_eq!(units.coordinates, CoordinateFormat::Dms);
        assert_eq!(units.grid_square, GridSquareFormat::SarCell);
        Ok(())
    }

    #[test]
    fn settings_language() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, LANGUAGE_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().language()?, Language::Japanese);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Write accessor tests (SettingsWriter)
    // -----------------------------------------------------------------------

    #[test]
    fn write_key_beep() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.settings().key_beep()?);
        mi.settings_mut().set_key_beep(true)?;
        assert!(mi.settings().key_beep()?);
        mi.settings_mut().set_key_beep(false)?;
        assert!(!mi.settings().key_beep()?);
        Ok(())
    }

    #[test]
    fn write_power_on_message_replaces_the_complete_fixed_field() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        let message = PowerOnMessage::new(" READY ")?;
        mi.settings_mut().set_power_on_message(&message)?;
        assert_eq!(mi.settings().power_on_message()?.as_str(), " READY ");
        let settings = mi.settings();
        let raw = settings
            .read_bytes(POWER_ON_MESSAGE_OFFSET, POWER_ON_MESSAGE_SIZE)
            .ok_or("power-on message field is missing")?;
        assert_eq!(
            raw, b" READY \0\0\0\0\0\0\0\0\0",
            "setter must NUL-pad the complete 16-byte field"
        );
        Ok(())
    }

    #[test]
    fn write_vox_gain_requires_validated_value() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(VoxGain::new(0xFF).is_err());
        let gain = VoxGain::new(9)?;
        mi.settings_mut().set_vox_gain(gain)?;
        assert_eq!(mi.settings().vox_gain()?, gain);
        Ok(())
    }

    #[test]
    fn write_vox_delay_requires_validated_value() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(VoxDelay::new(30).is_err());
        mi.settings_mut().set_vox_delay(VoxDelay::MS_3000)?;
        assert_eq!(get_byte(mi.as_raw(), VOX_DELAY_OFFSET)?, 6);
        assert_eq!(mi.settings().vox_delay()?, VoxDelay::MS_3000);
        Ok(())
    }

    #[test]
    fn write_backlight_timer_requires_validated_value() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(BacklightTimer::new(0).is_err());
        assert!(BacklightTimer::new(61).is_err());
        mi.settings_mut()
            .set_backlight_timer(BacklightTimer::new(3)?)?;
        assert_eq!(get_byte(mi.as_raw(), BACKLIGHT_TIMER_OFFSET)?, 3);
        mi.settings_mut()
            .set_backlight_timer(BacklightTimer::new(60)?)?;
        assert_eq!(get_byte(mi.as_raw(), BACKLIGHT_TIMER_OFFSET)?, 60);
        Ok(())
    }

    #[test]
    fn write_scan_restart_times_require_validated_values() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(ScanRestartDelay::new(0).is_err());
        let one_second = ScanRestartDelay::new(1)?;
        mi.settings_mut().set_scan_restart_time(one_second)?;
        mi.settings_mut().set_scan_restart_carrier(one_second)?;
        assert_eq!(get_byte(mi.as_raw(), SCAN_RESTART_TIME_OFFSET)?, 1);
        assert_eq!(get_byte(mi.as_raw(), SCAN_RESTART_CARRIER_OFFSET)?, 1);
        Ok(())
    }

    #[test]
    fn write_emr_volume_level_requires_validated_value() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(EmrVolume::new(0).is_err());
        assert!(EmrVolume::new(51).is_err());
        mi.settings_mut().set_emr_volume_level(EmrVolume::new(1)?)?;
        assert_eq!(get_byte(mi.as_raw(), EMR_VOLUME_LEVEL_OFFSET)?, 1);
        mi.settings_mut()
            .set_emr_volume_level(EmrVolume::new(50)?)?;
        assert_eq!(get_byte(mi.as_raw(), EMR_VOLUME_LEVEL_OFFSET)?, 50);
        Ok(())
    }

    #[test]
    fn write_beat_shift() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_beat_shift(BeatShift::Type5)?;
        assert_eq!(mi.settings().beat_shift()?, BeatShift::Type5);
        assert_eq!(get_byte(mi.as_raw(), BEAT_SHIFT_OFFSET)?, 4);
        Ok(())
    }

    #[test]
    fn write_auto_power_off_enum_encoding() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_auto_power_off(AutoPowerOff::Min15)?;
        assert_eq!(get_byte(mi.as_raw(), AUTO_POWER_OFF_OFFSET)?, 1);
        mi.settings_mut().set_auto_power_off(AutoPowerOff::Min60)?;
        assert_eq!(get_byte(mi.as_raw(), AUTO_POWER_OFF_OFFSET)?, 3);
        Ok(())
    }

    #[test]
    fn write_pf_keys_accept_only_typed_official_assignments() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut()
            .set_pf_key1(FrontPanelPfFunction::MemoryInput)?;
        assert_eq!(
            mi.settings().pf_key1()?,
            StoredFrontPanelPfAssignment::Official(FrontPanelPfFunction::MemoryInput)
        );
        mi.settings_mut().set_pf_key2(FrontPanelPfFunction::Gps)?;
        assert_eq!(
            mi.settings().pf_key2()?,
            StoredFrontPanelPfAssignment::Official(FrontPanelPfFunction::Gps)
        );

        for invalid in [5u8, 23, 25, 26, 31, 0xFF] {
            assert!(
                FrontPanelPfFunction::try_from(invalid).is_err(),
                "PF raw {invalid} must not construct an official assignment"
            );
        }
        Ok(())
    }

    #[test]
    fn read_pf_keys_preserves_noncanonical_raw_values() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, PF_KEY1_OFFSET, 31)?;
        set_byte(&mut image, PF_KEY2_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;

        assert_eq!(mi.settings().pf_key1()?.as_raw(), 31);
        assert_eq!(mi.settings().pf_key2()?.as_raw(), 0xFF);
        assert!(matches!(
            mi.settings().pf_key1()?,
            StoredFrontPanelPfAssignment::OffMenu(_)
        ));
        assert!(matches!(
            mi.settings().pf_key2()?,
            StoredFrontPanelPfAssignment::OffMenu(_)
        ));
        Ok(())
    }

    #[test]
    fn key_lock_setters_preserve_unowned_bits() -> TestResult {
        let mut image = make_settings_image()?;
        // Seed the shared byte with all six unowned bits set.
        set_byte(&mut image, KEY_LOCK_OFFSET, 0xFC)?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;

        mi.settings_mut().set_key_lock(true)?;
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFD);
        mi.settings_mut().set_frequency_lock(true)?;
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFF);
        mi.settings_mut().set_key_lock(false)?;
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFE);
        mi.settings_mut().set_frequency_lock(false)?;
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFC);
        Ok(())
    }

    #[test]
    fn aprs_lock_setters_preserve_unowned_bits() -> TestResult {
        let mut image = make_settings_image()?;
        // Seed the shared byte with all five unowned bits set.
        set_byte(&mut image, APRS_LOCK_OFFSET, 0xF8)?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;

        mi.settings_mut().set_aprs_lock_frequency(true)?;
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xF9);
        mi.settings_mut().set_aprs_lock_ptt(true)?;
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xFB);
        mi.settings_mut().set_aprs_lock_key(true)?;
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xFF);
        mi.settings_mut().set_aprs_lock_ptt(false)?;
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xFD);
        mi.settings_mut().set_aprs_lock_frequency(false)?;
        mi.settings_mut().set_aprs_lock_key(false)?;
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xF8);
        Ok(())
    }

    #[test]
    fn write_roundtrip_hardware_verified_cells() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;

        // Set everything to non-default values.
        mi.settings_mut().set_key_beep(true)?;
        mi.settings_mut().set_vox_enabled(true)?;
        mi.settings_mut().set_vox_gain(VoxGain::new(9)?)?;
        mi.settings_mut().set_band_mode(BandMode::Dual)?;
        mi.settings_mut().set_attenuator_a(true)?;
        mi.settings_mut().set_power_level_a(PowerLevel::ExtraLow)?;
        mi.settings_mut().set_bluetooth(true)?;

        // Verify reads match.
        let s = mi.settings();
        assert!(s.key_beep()?);
        assert!(s.vox_enabled()?);
        assert_eq!(s.vox_gain()?, VoxGain::new(9)?);
        assert_eq!(s.band_mode()?, BandMode::Dual);
        assert!(s.attenuator_a()?);
        assert_eq!(s.power_level_a()?, PowerLevel::ExtraLow);
        assert!(s.bluetooth()?);

        // Verify raw bytes at the verified offsets.
        let raw = mi.as_raw();
        assert_eq!(get_byte(raw, KEY_BEEP_OFFSET)?, 1);
        assert_eq!(get_byte(raw, VOX_ENABLED_OFFSET)?, 1);
        assert_eq!(get_byte(raw, VOX_GAIN_OFFSET)?, 9);
        assert_eq!(get_byte(raw, DUAL_BAND_OFFSET)?, 1);
        assert_eq!(get_byte(raw, ATTENUATOR_A_OFFSET)?, 1);
        assert_eq!(get_byte(raw, POWER_LEVEL_A_OFFSET)?, 3); // ExtraLow = 3
        assert_eq!(get_byte(raw, BLUETOOTH_OFFSET)?, 1);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Setter offset audit
    // -----------------------------------------------------------------------

    /// A setter invocation for the offset-audit table.
    type ApplySetter = fn(&mut SettingsWriter<'_>) -> Result<(), SettingsValueError>;

    fn test_value<T>(
        value: Result<T, ValidationError>,
        setting: &'static str,
        offset: usize,
        raw: u8,
        detail: &'static str,
    ) -> Result<T, SettingsValueError> {
        value.map_err(|_| SettingsValueError::InvalidValue {
            setting,
            offset,
            value: raw,
            detail,
        })
    }

    /// Every single-byte settings setter, paired with the flash offset it must
    /// write, transcribed INDEPENDENTLY from the MCP-D75 registry map
    /// (hex literals, not the `*_OFFSET` constants): a transposed
    /// digit in a constant would corrupt an unrelated radio setting on
    /// MCP write-back, and the offset audit below is the test that
    /// catches it. The third column carries the owned bit mask for
    /// setters that share a byte with other registry fields.
    static SETTER_OFFSET_TABLE: &[(&str, usize, Option<u8>, ApplySetter)] = &[
        ("set_beat_shift", 0x1000, None, |s| {
            s.set_beat_shift(BeatShift::Type2)
        }),
        ("set_tx_inhibit", 0x1001, None, |s| s.set_tx_inhibit(true)),
        ("set_timeout_timer", 0x1003, None, |s| {
            s.set_timeout_timer(TransmitTimeout::Seconds60)
        }),
        ("set_mic_sensitivity", 0x1006, None, |s| {
            s.set_mic_sensitivity(crate::types::settings::MicSensitivity::Medium)
        }),
        ("set_ssb_high_cut", 0x1008, None, |s| {
            s.set_ssb_high_cut(SsbHighCut::Khz2_4)
        }),
        ("set_cw_width", 0x1009, None, |s| {
            s.set_cw_width(CwFilterWidth::Khz0_5)
        }),
        ("set_am_high_cut", 0x100A, None, |s| {
            s.set_am_high_cut(AmHighCut::Khz4_5)
        }),
        ("set_scan_resume", 0x100C, None, |s| {
            s.set_scan_resume(ScanResumeMethod::CarrierOperated)
        }),
        ("set_digital_scan_resume", 0x100D, None, |s| {
            s.set_digital_scan_resume(ScanResumeMethod::CarrierOperated)
        }),
        ("set_scan_restart_time", 0x100E, None, |s| {
            let delay = test_value(
                ScanRestartDelay::new(2),
                "radio.TimeRestart",
                SCAN_RESTART_TIME_OFFSET,
                2,
                "must be 1-10 seconds",
            )?;
            s.set_scan_restart_time(delay)
        }),
        ("set_scan_restart_carrier", 0x100F, None, |s| {
            let delay = test_value(
                ScanRestartDelay::new(2),
                "radio.CarrierRestart",
                SCAN_RESTART_CARRIER_OFFSET,
                2,
                "must be 1-10 seconds",
            )?;
            s.set_scan_restart_carrier(delay)
        }),
        ("set_repeater_auto_offset", 0x1018, None, |s| {
            s.set_repeater_auto_offset(true)
        }),
        ("set_repeater_call_key", 0x1019, None, |s| {
            s.set_repeater_call_key(RepeaterCallKey::Tone1750Hz)
        }),
        ("set_vox_enabled", 0x101B, None, |s| s.set_vox_enabled(true)),
        ("set_vox_gain", 0x101C, None, |s| {
            let gain = VoxGain::new(1).map_err(|_| SettingsValueError::InvalidValue {
                setting: "radio.VoxGain",
                offset: VOX_GAIN_OFFSET,
                value: 1,
                detail: "must be 0-9",
            })?;
            s.set_vox_gain(gain)
        }),
        ("set_vox_delay", 0x101D, None, |s| {
            s.set_vox_delay(VoxDelay::MS_500)
        }),
        ("set_vox_tx_on_busy", 0x101E, None, |s| {
            s.set_vox_tx_on_busy(true)
        }),
        ("set_dtmf_speed", 0x101F, None, |s| {
            s.set_dtmf_speed(DtmfToneDuration::Ms100)
        }),
        ("set_dtmf_pause_time", 0x1020, None, |s| {
            s.set_dtmf_pause_time(DtmfPause::Ms250)
        }),
        ("set_dtmf_tx_hold", 0x1021, None, |s| {
            s.set_dtmf_tx_hold(true)
        }),
        ("set_cw_pitch", 0x1024, None, |s| {
            let pitch = CwPitch::new(500).map_err(|_| SettingsValueError::InvalidValue {
                setting: "radio.CwPitchFreq",
                offset: CW_PITCH_OFFSET,
                value: 1,
                detail: "must be 0-6 (400-1000 Hz)",
            })?;
            s.set_cw_pitch(pitch)
        }),
        ("set_auto_mute_return_time", 0x1041, None, |s| {
            let delay = test_value(
                AutoMuteReturnDelay::new(2),
                "radio.AutoMuteRetTime",
                AUTO_MUTE_RETURN_TIME_OFFSET,
                2,
                "must be 1-10 seconds",
            )?;
            s.set_auto_mute_return_time(delay)
        }),
        ("set_backlight_control", 0x1060, None, |s| {
            s.set_backlight_control(BacklightControl::On)
        }),
        ("set_backlight_timer", 0x1061, None, |s| {
            let timer = test_value(
                BacklightTimer::new(10),
                "radio.BacklightTimer",
                BACKLIGHT_TIMER_OFFSET,
                10,
                "must be 3-60 seconds",
            )?;
            s.set_backlight_timer(timer)
        }),
        ("set_key_beep", 0x1071, None, |s| s.set_key_beep(true)),
        ("set_beep_volume", 0x1072, None, |s| {
            let volume = test_value(
                LinkedVolumeLevel::fixed(1),
                "radio.BeepVolume",
                BEEP_VOLUME_OFFSET,
                1,
                "must be 0-7, where 0 means VOL Link",
            )?;
            s.set_beep_volume(volume)
        }),
        ("set_announce", 0x1073, None, |s| {
            s.set_announce(VoiceAnnounceMode::Manual)
        }),
        ("set_voice_volume", 0x1074, None, |s| {
            let volume = test_value(
                LinkedVolumeLevel::fixed(1),
                "radio.VoiceAnnounceVolume",
                VOICE_VOLUME_OFFSET,
                1,
                "must be 0-7, where 0 means VOL Link",
            )?;
            s.set_voice_volume(volume)
        }),
        ("set_battery_saver", 0x1076, None, |s| {
            s.set_battery_saver(BatterySaverInterval::Seconds0_2)
        }),
        ("set_auto_power_off", 0x1077, None, |s| {
            s.set_auto_power_off(AutoPowerOff::Min15)
        }),
        ("set_bluetooth", 0x1078, None, |s| s.set_bluetooth(true)),
        ("set_bt_auto_connect", 0x1079, None, |s| {
            s.set_bt_auto_connect(true)
        }),
        ("set_pf_key1", 0x107A, None, |s| {
            s.set_pf_key1(FrontPanelPfFunction::VoiceMessage1)
        }),
        ("set_pf_key2", 0x107B, None, |s| {
            s.set_pf_key2(FrontPanelPfFunction::VoiceMessage1)
        }),
        ("set_key_lock", 0x1084, Some(0x01), |s| s.set_key_lock(true)),
        ("set_frequency_lock", 0x1084, Some(0x02), |s| {
            s.set_frequency_lock(true)
        }),
        ("set_volume_lock", 0x1087, None, |s| s.set_volume_lock(true)),
        ("set_speed_distance_unit", 0x1088, None, |s| {
            s.set_speed_distance_unit(SpeedDistanceUnit::KilometersPerHour)
        }),
        ("set_altitude_rain_unit", 0x1089, None, |s| {
            s.set_altitude_rain_unit(AltitudeRainUnit::MetersMm)
        }),
        ("set_temperature_unit", 0x108A, None, |s| {
            s.set_temperature_unit(TemperatureUnit::Celsius)
        }),
        ("set_gps_pc_output_interface", 0x108E, None, |s| {
            s.set_gps_pc_output_interface(PcOutputInterface::Bluetooth)
        }),
        ("set_aprs_pc_output_interface", 0x108F, None, |s| {
            s.set_aprs_pc_output_interface(PcOutputInterface::Bluetooth)
        }),
        ("set_language", 0x1092, None, |s| {
            s.set_language(Language::Japanese)
        }),
        ("set_voice_speed", 0x1097, None, |s| {
            s.set_voice_speed(VoiceGuideSpeed::Speed2)
        }),
        ("set_aprs_lock_frequency", 0x120A, Some(0x01), |s| {
            s.set_aprs_lock_frequency(true)
        }),
        ("set_aprs_lock_ptt", 0x120A, Some(0x02), |s| {
            s.set_aprs_lock_ptt(true)
        }),
        ("set_aprs_lock_key", 0x120A, Some(0x04), |s| {
            s.set_aprs_lock_key(true)
        }),
        ("set_emr_volume_level", 0x1A03, None, |s| {
            let volume = test_value(
                EmrVolume::new(2),
                "dv.EmrVolumeLevelTxRx",
                EMR_VOLUME_LEVEL_OFFSET,
                2,
                "must be 1-50",
            )?;
            s.set_emr_volume_level(volume)
        }),
        ("set_power_level_a", 0x0359, None, |s| {
            s.set_power_level_a(PowerLevel::ExtraLow)
        }),
        ("set_attenuator_a", 0x035C, None, |s| {
            s.set_attenuator_a(true)
        }),
        ("set_band_mode", 0x0396, None, |s| {
            s.set_band_mode(BandMode::Dual)
        }),
    ];

    /// Applies every entry of [`SETTER_OFFSET_TABLE`] to a fresh image
    /// and asserts exactly one byte changed, at the documented offset
    /// (no neighbor stomping, no wrong-offset writes). For masked bit
    /// setters, additionally asserts the change stayed within the
    /// owned bits.
    #[test]
    fn every_single_byte_setter_writes_its_documented_offset() -> TestResult {
        for (name, offset, mask, apply) in SETTER_OFFSET_TABLE {
            let image = make_settings_image()?;
            let mut mi = crate::memory::MemoryImage::from_raw(image)?;
            let before = mi.as_raw().to_vec();
            apply(&mut mi.settings_mut())?;
            let after = mi.as_raw();
            let diffs: Vec<usize> = before
                .iter()
                .zip(after.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                diffs,
                vec![*offset],
                "{name}: expected exactly one byte changed at 0x{offset:04X}, got {diffs:04X?}"
            );
            if let Some(mask) = mask {
                let before_byte = get_byte(&before, *offset)?;
                let after_byte = get_byte(after, *offset)?;
                assert_eq!(
                    before_byte & !mask,
                    after_byte & !mask,
                    "{name}: bits outside owned mask 0x{mask:02X} changed at 0x{offset:04X}"
                );
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cross-layer registry consistency
    // -----------------------------------------------------------------------

    /// Registry codec shape expected for one settings-layer binding.
    /// Byte domains are the exact domains the typed accessors enforce, so a
    /// registry regeneration that changes a domain fails this test
    /// until the accessor is re-audited.
    enum ExpectedCodec {
        /// `FieldCodec::Bool` (one byte holding 0/1).
        Bool,
        /// `FieldCodec::Byte` with this inclusive domain.
        Byte {
            /// Smallest legal raw value.
            min: u8,
            /// Largest legal raw value.
            max: u8,
        },
        /// `FieldCodec::BitBool` with exactly this owned mask.
        BitBool {
            /// The single bit the accessor reads and writes.
            mask: u8,
        },
        /// `FieldCodec::FixedString` of this length.
        FixedString {
            /// Reserved byte count.
            len: usize,
        },
    }

    /// Every surviving offset constant, bound to the registry field it
    /// must address. This is the drift guard: `menu_fields.rs` is
    /// generated from the official MCP-D75 serializers, so if a
    /// regeneration moves or re-domains a field, this test points at
    /// the exact accessor that needs re-auditing.
    static REGISTRY_BINDINGS: &[(usize, &str, ExpectedCodec)] = &[
        (
            BEAT_SHIFT_OFFSET,
            "radio.BeatShift",
            ExpectedCodec::Byte { min: 0, max: 7 },
        ),
        (TX_INHIBIT_OFFSET, "radio.TxInhibit", ExpectedCodec::Bool),
        (
            TIMEOUT_TIMER_OFFSET,
            "radio.TimeOutTimer",
            ExpectedCodec::Byte { min: 0, max: 10 },
        ),
        (
            MIC_SENSITIVITY_OFFSET,
            "radio.MicSensitivity",
            ExpectedCodec::Byte { min: 0, max: 2 },
        ),
        (
            SSB_HIGH_CUT_OFFSET,
            "radio.SsbHighCut",
            ExpectedCodec::Byte { min: 0, max: 4 },
        ),
        (
            CW_WIDTH_OFFSET,
            "radio.CwWidth",
            ExpectedCodec::Byte { min: 0, max: 4 },
        ),
        (
            AM_HIGH_CUT_OFFSET,
            "radio.AmHighCut",
            ExpectedCodec::Byte { min: 0, max: 3 },
        ),
        (
            SCAN_RESUME_OFFSET,
            "radio.ScanResumeAnalog",
            ExpectedCodec::Byte { min: 0, max: 2 },
        ),
        (
            DIGITAL_SCAN_RESUME_OFFSET,
            "radio.ScanResumeDigital",
            ExpectedCodec::Byte { min: 0, max: 2 },
        ),
        (
            SCAN_RESTART_TIME_OFFSET,
            "radio.TimeRestart",
            ExpectedCodec::Byte { min: 1, max: 10 },
        ),
        (
            SCAN_RESTART_CARRIER_OFFSET,
            "radio.CarrierRestart",
            ExpectedCodec::Byte { min: 1, max: 10 },
        ),
        (
            REPEATER_AUTO_OFFSET_OFFSET,
            "radio.AutoOffset",
            ExpectedCodec::Bool,
        ),
        (
            REPEATER_CALL_KEY_OFFSET,
            "radio.CallKey",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (VOX_ENABLED_OFFSET, "radio.Vox", ExpectedCodec::Bool),
        (
            VOX_GAIN_OFFSET,
            "radio.VoxGain",
            ExpectedCodec::Byte { min: 0, max: 9 },
        ),
        (
            VOX_DELAY_OFFSET,
            "radio.VoxDelay",
            ExpectedCodec::Byte { min: 0, max: 6 },
        ),
        (
            VOX_TX_ON_BUSY_OFFSET,
            "radio.VoxTxOnBusy",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            DTMF_SPEED_OFFSET,
            "radio.DtmfSpeed",
            ExpectedCodec::Byte { min: 0, max: 2 },
        ),
        (
            DTMF_PAUSE_TIME_OFFSET,
            "radio.DtmfPauseTime",
            ExpectedCodec::Byte { min: 0, max: 6 },
        ),
        (DTMF_TX_HOLD_OFFSET, "radio.DtmfTxHold", ExpectedCodec::Bool),
        (
            CW_PITCH_OFFSET,
            "radio.CwPitchFreq",
            ExpectedCodec::Byte { min: 0, max: 6 },
        ),
        (
            AUTO_MUTE_RETURN_TIME_OFFSET,
            "radio.AutoMuteRetTime",
            ExpectedCodec::Byte { min: 1, max: 10 },
        ),
        (
            BACKLIGHT_CONTROL_OFFSET,
            "radio.BacklightControl",
            ExpectedCodec::Byte { min: 0, max: 3 },
        ),
        (
            BACKLIGHT_TIMER_OFFSET,
            "radio.BacklightTimer",
            ExpectedCodec::Byte { min: 3, max: 60 },
        ),
        (KEY_BEEP_OFFSET, "radio.Beep", ExpectedCodec::Bool),
        (
            BEEP_VOLUME_OFFSET,
            "radio.BeepVolume",
            ExpectedCodec::Byte { min: 0, max: 7 },
        ),
        (
            ANNOUNCE_OFFSET,
            "radio.VoiceAnnounce",
            ExpectedCodec::Byte { min: 0, max: 3 },
        ),
        (
            VOICE_VOLUME_OFFSET,
            "radio.VoiceAnnounceVolume",
            ExpectedCodec::Byte { min: 0, max: 7 },
        ),
        (
            BATTERY_SAVER_OFFSET,
            "radio.BatterySaver",
            ExpectedCodec::Byte { min: 0, max: 9 },
        ),
        (
            AUTO_POWER_OFF_OFFSET,
            "radio.AutoPowerOff",
            ExpectedCodec::Byte { min: 0, max: 3 },
        ),
        (
            BLUETOOTH_OFFSET,
            "radio.BluetoothOnOff",
            ExpectedCodec::Bool,
        ),
        (
            BT_AUTO_CONNECT_OFFSET,
            "radio.BluetoothAutoConnect",
            ExpectedCodec::Bool,
        ),
        (
            PF_KEY1_OFFSET,
            "radio.Pf1PfKey",
            ExpectedCodec::Byte { min: 0, max: 30 },
        ),
        (
            PF_KEY2_OFFSET,
            "radio.Pf2PfKey",
            ExpectedCodec::Byte { min: 0, max: 30 },
        ),
        (
            KEY_LOCK_OFFSET,
            "radio.KeyLockTypeKeyLock",
            ExpectedCodec::BitBool { mask: 0x01 },
        ),
        (
            KEY_LOCK_OFFSET,
            "radio.KeyLockTypeFrequencyLock",
            ExpectedCodec::BitBool { mask: 0x02 },
        ),
        (
            VOLUME_LOCK_OFFSET,
            "radio.VolumeLockOnOff",
            ExpectedCodec::Bool,
        ),
        (
            SPEED_DISTANCE_UNIT_OFFSET,
            "radio.SpeedDistance",
            ExpectedCodec::Byte { min: 0, max: 2 },
        ),
        (
            ALTITUDE_RAIN_UNIT_OFFSET,
            "radio.AltitudeRain",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            TEMPERATURE_UNIT_OFFSET,
            "radio.Temperature",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            COORDINATE_FORMAT_OFFSET,
            "radio.LatitudeLongitude",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            GRID_SQUARE_FORMAT_OFFSET,
            "radio.GridSquareFormat",
            ExpectedCodec::Byte { min: 0, max: 2 },
        ),
        (
            GPS_BT_INTERFACE_OFFSET,
            "radio.PcOutputInterfaceGps",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            APRS_USB_MODE_OFFSET,
            "radio.PcOutputInterfaceAprs",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            LANGUAGE_OFFSET,
            "radio.Language",
            ExpectedCodec::Byte { min: 0, max: 1 },
        ),
        (
            VOICE_SPEED_OFFSET,
            "radio.VoiceGuidanceSpeed",
            ExpectedCodec::Byte { min: 0, max: 3 },
        ),
        (
            POWER_ON_MESSAGE_OFFSET,
            "radio.PowerOnMessage",
            ExpectedCodec::FixedString {
                len: POWER_ON_MESSAGE_SIZE,
            },
        ),
        (
            APRS_LOCK_OFFSET,
            "aprs.Frequency",
            ExpectedCodec::BitBool { mask: 0x01 },
        ),
        (
            APRS_LOCK_OFFSET,
            "aprs.Ptt",
            ExpectedCodec::BitBool { mask: 0x02 },
        ),
        (
            APRS_LOCK_OFFSET,
            "aprs.AprsKey",
            ExpectedCodec::BitBool { mask: 0x04 },
        ),
        (
            EMR_VOLUME_LEVEL_OFFSET,
            "dv.EmrVolumeLevelTxRx",
            ExpectedCodec::Byte { min: 1, max: 50 },
        ),
    ];

    #[test]
    fn every_offset_constant_binds_the_intended_registry_field() -> TestResult {
        for (offset, name, expected) in REGISTRY_BINDINGS {
            let field =
                menu_field(name).ok_or_else(|| format!("registry field {name} is missing"))?;
            assert_eq!(
                field.descriptor.offset, *offset,
                "{name}: registry offset does not match the settings constant"
            );
            match (expected, field.descriptor.codec) {
                (ExpectedCodec::Bool, FieldCodec::Bool) => {}
                (
                    ExpectedCodec::Byte { min, max },
                    FieldCodec::Byte {
                        min: reg_min,
                        max: reg_max,
                    },
                ) => {
                    assert_eq!(
                        (*min, *max),
                        (reg_min, reg_max),
                        "{name}: accessor domain diverges from the registry domain"
                    );
                }
                (ExpectedCodec::BitBool { mask }, FieldCodec::BitBool { mask: reg_mask }) => {
                    assert_eq!(
                        *mask, reg_mask,
                        "{name}: accessor bit mask diverges from the registry mask"
                    );
                }
                (
                    ExpectedCodec::FixedString { len },
                    FieldCodec::FixedString { len: reg_len, .. },
                ) => {
                    assert_eq!(
                        *len, reg_len,
                        "{name}: accessor string length diverges from the registry"
                    );
                }
                (_, other) => {
                    return Err(format!("{name}: unexpected registry codec {other:?}").into());
                }
            }
        }
        Ok(())
    }

    /// Byte length occupied by a registry codec, for span checks.
    fn codec_len(codec: FieldCodec) -> usize {
        match codec {
            FieldCodec::Byte { .. }
            | FieldCodec::Bool
            | FieldCodec::BitBool { .. }
            | FieldCodec::BitField { .. } => 1,
            FieldCodec::FixedString { len, .. } | FieldCodec::Bytes { len } => len,
            FieldCodec::Unsigned { width, .. } | FieldCodec::Signed { width, .. } => {
                usize::from(width)
            }
        }
    }

    /// The three hardware-verified band-state cells sit below 0x1000
    /// where the MCP-D75 registry has no coverage. If a registry
    /// regeneration ever claims those offsets, a human must re-audit
    /// the accessors before trusting either layer.
    #[test]
    fn band_state_cells_stay_outside_the_registry() {
        for offset in [POWER_LEVEL_A_OFFSET, ATTENUATOR_A_OFFSET, DUAL_BAND_OFFSET] {
            for field in MCP_D75_MENU_FIELDS {
                let start = field.descriptor.offset;
                let len = codec_len(field.descriptor.codec);
                assert!(
                    offset < start || offset >= start + len,
                    "hardware-verified band-state cell 0x{offset:04X} collides with registry \
                     field {}; re-audit the accessor before trusting either layer",
                    field.descriptor.name
                );
            }
        }
    }

    /// The PF assignment type encodes the registry's gapped domain; pin
    /// the variant set to the generated option list so a registry change
    /// cannot silently outdate the type.
    #[test]
    fn pf_key_validation_matches_the_registry_options() -> TestResult {
        for name in ["radio.Pf1PfKey", "radio.Pf2PfKey"] {
            let field = menu_field(name).ok_or_else(|| format!("{name} is missing"))?;
            for raw in 0..=30_u8 {
                let in_registry = field.option(u64::from(raw)).is_some();
                assert_eq!(
                    FrontPanelPfFunction::try_from(raw).is_ok(),
                    in_registry,
                    "{name}: typed domain and registry disagree about raw value {raw}"
                );
            }
        }
        Ok(())
    }
}
