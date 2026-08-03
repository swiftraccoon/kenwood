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

// Removed legacy accessors (offsets disproven against the MCP-D75
// registry and the hardware dump):
//
// - `lock`/`set_lock` (0x1060): that byte is `radio.BacklightControl`.
//   No CAT key-lock state operation is currently verified. The persistent
//   key-lock *configuration* is the bit pair at 0x1084
//   (`key_lock`/`frequency_lock` below).
// - VFO block (0x0020): the hardware dump holds no VFO data there
//   (entry 0 all-0xFF, entries 1-5 zero). Candidate real VFO records
//   were observed at 0x0400/0x0430/0x0460 (48-byte stride, second bank
//   at 0x0600) but remain unverified; no accessor until they are.
// - `model_name` (0x11D0): unmapped gap after `gps.MyPositionSelect`;
//   the famous "TH-D75" string lives at 0x10D0 =
//   `radio.BluetoothDeviceName` (reachable through the registry).
// - `squelch_a`/`squelch_b` (0x100D/0x100E): squelch is runtime state
//   (CAT `SQ`), never serialized by MCP-D75; those bytes are the
//   digital scan-resume and time-restart cells.
// - `callsign_raw` (0x1300): that offset is
//   `aprs.StatusTextList[4].StatusText`. D-STAR MY callsigns live in
//   `dv.MyCallsignDvGatewayList` at 0x1CA8 (see `memory::dstar`).

use crate::error::ValidationError;
use crate::protocol::programming;
use crate::types::PowerLevel;
use crate::types::settings::{
    AltitudeRainUnit, AutoPowerOff, BeatShift, DisplayUnits, Language, SpeedDistanceUnit,
    TemperatureUnit,
};

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
const POWER_ON_MESSAGE_SIZE: usize = 16;

/// True when `value` is a member of the gapped `radio.Pf1PfKey` /
/// `radio.Pf2PfKey` domain (0-30 excluding 5, 23, 25, 26).
const fn pf_key_valid(value: u8) -> bool {
    value <= 30 && !matches!(value, 5 | 23 | 25 | 26)
}

/// Validate a PF-key assignment against the registry's gapped domain.
const fn validate_pf_key(name: &'static str, value: u8) -> Result<(), ValidationError> {
    if pf_key_valid(value) {
        Ok(())
    } else {
        Err(ValidationError::SettingOutOfRange {
            name,
            value,
            detail: "must be 0-30, excluding 5, 23, 25, 26",
        })
    }
}

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
    pub fn raw(&self) -> Option<&[u8]> {
        let end = SETTINGS_OFFSET + SETTINGS_SIZE;
        self.image.get(SETTINGS_OFFSET..end)
    }

    /// Get the power-on message (`radio.PowerOnMessage`, up to 16
    /// characters).
    ///
    /// MCP offset `0x10C0`. Returns the NUL-padded ASCII string with
    /// padding removed.
    #[must_use]
    pub fn power_on_message(&self) -> String {
        extract_string(self.image, POWER_ON_MESSAGE_OFFSET, POWER_ON_MESSAGE_SIZE)
    }

    /// Read an arbitrary byte range from the settings region.
    ///
    /// The offset is relative to the start of the image (MCP byte
    /// address). Returns `None` if the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset + len;
        self.image.get(offset..end)
    }

    // -----------------------------------------------------------------------
    // TX/RX
    // -----------------------------------------------------------------------

    /// Read the beat-shift type (`radio.BeatShift`).
    ///
    /// MCP offset `0x1000`. Returns [`BeatShift::Type1`] if the byte is
    /// out of range or unreadable.
    #[must_use]
    pub fn beat_shift(&self) -> BeatShift {
        self.image
            .get(BEAT_SHIFT_OFFSET)
            .copied()
            .and_then(|b| BeatShift::try_from(b).ok())
            .unwrap_or(BeatShift::Type1)
    }

    /// Read TX inhibit (`radio.TxInhibit`; false if unreadable).
    ///
    /// MCP offset `0x1001`.
    #[must_use]
    pub fn tx_inhibit(&self) -> bool {
        self.image.get(TX_INHIBIT_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read the TX timeout timer index (`radio.TimeOutTimer`, 0-10; 0
    /// if unreadable).
    ///
    /// MCP offset `0x1003`. Indexes the table 0.5, 1.0, 1.5, 2.0, 2.5,
    /// 3.0, 3.5, 4.0, 4.5, 5.0, 10.0 minutes, NOT a minute count.
    #[must_use]
    pub fn timeout_timer(&self) -> u8 {
        self.image
            .get(TIMEOUT_TIMER_OFFSET)
            .copied()
            .map_or(0, |b| b.min(10))
    }

    /// Read microphone sensitivity (`radio.MicSensitivity`, 0-2; 0 if
    /// unreadable).
    ///
    /// MCP offset `0x1006`. The encoding is inverted versus intuition:
    /// **0=High, 1=Medium, 2=Low**.
    #[must_use]
    pub fn mic_sensitivity(&self) -> u8 {
        self.image
            .get(MIC_SENSITIVITY_OFFSET)
            .copied()
            .map_or(0, |b| b.min(2))
    }

    /// Read the SSB high-cut filter (`radio.SsbHighCut`, 0-4 =
    /// 2.2/2.4/2.6/2.8/3.0 kHz; 0 if unreadable).
    ///
    /// MCP offset `0x1008`.
    #[must_use]
    pub fn ssb_high_cut(&self) -> u8 {
        self.image
            .get(SSB_HIGH_CUT_OFFSET)
            .copied()
            .map_or(0, |b| b.min(4))
    }

    /// Read the CW filter width (`radio.CwWidth`, 0-4 =
    /// 0.3/0.5/1.0/1.5/2.0 kHz; 0 if unreadable).
    ///
    /// MCP offset `0x1009`. The D75 menu calls this "CW Width" (it was
    /// previously misnamed "CW high cut" here).
    #[must_use]
    pub fn cw_width(&self) -> u8 {
        self.image
            .get(CW_WIDTH_OFFSET)
            .copied()
            .map_or(0, |b| b.min(4))
    }

    /// Read the AM high-cut filter (`radio.AmHighCut`, 0-3 =
    /// 3.0/4.5/6.0/7.5 kHz; 0 if unreadable).
    ///
    /// MCP offset `0x100A`.
    #[must_use]
    pub fn am_high_cut(&self) -> u8 {
        self.image
            .get(AM_HIGH_CUT_OFFSET)
            .copied()
            .map_or(0, |b| b.min(3))
    }

    // -----------------------------------------------------------------------
    // Scan
    // -----------------------------------------------------------------------

    /// Read the analog scan-resume method (`radio.ScanResumeAnalog`,
    /// 0=Time, 1=Carrier, 2=Seek; 0 if unreadable).
    ///
    /// MCP offset `0x100C`.
    #[must_use]
    pub fn scan_resume(&self) -> u8 {
        self.image
            .get(SCAN_RESUME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(2))
    }

    /// Read the digital scan-resume method (`radio.ScanResumeDigital`,
    /// 0=Time, 1=Carrier, 2=Seek; 0 if unreadable).
    ///
    /// MCP offset `0x100D`.
    #[must_use]
    pub fn digital_scan_resume(&self) -> u8 {
        self.image
            .get(DIGITAL_SCAN_RESUME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(2))
    }

    /// Read the time-operated scan restart time (`radio.TimeRestart`,
    /// 1-10 seconds; 0 if unreadable).
    ///
    /// MCP offset `0x100E`.
    #[must_use]
    pub fn scan_restart_time(&self) -> u8 {
        self.image
            .get(SCAN_RESTART_TIME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(10))
    }

    /// Read the carrier-operated scan restart time
    /// (`radio.CarrierRestart`, 1-10 seconds; 0 if unreadable).
    ///
    /// MCP offset `0x100F`.
    #[must_use]
    pub fn scan_restart_carrier(&self) -> u8 {
        self.image
            .get(SCAN_RESTART_CARRIER_OFFSET)
            .copied()
            .map_or(0, |b| b.min(10))
    }

    // -----------------------------------------------------------------------
    // Repeater
    // -----------------------------------------------------------------------

    /// Read repeater auto offset (`radio.AutoOffset`; false if
    /// unreadable).
    ///
    /// MCP offset `0x1018`.
    #[must_use]
    pub fn repeater_auto_offset(&self) -> bool {
        self.image
            .get(REPEATER_AUTO_OFFSET_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read the CALL key function (`radio.CallKey`, 0=CALL, 1=1750 Hz;
    /// 0 if unreadable).
    ///
    /// MCP offset `0x1019`.
    #[must_use]
    pub fn repeater_call_key(&self) -> u8 {
        self.image
            .get(REPEATER_CALL_KEY_OFFSET)
            .copied()
            .map_or(0, |b| b.min(1))
    }

    // -----------------------------------------------------------------------
    // VOX
    // -----------------------------------------------------------------------

    /// Read VOX enabled (`radio.Vox`; false if unreadable).
    ///
    /// MCP offset `0x101B`. Hardware verified.
    #[must_use]
    pub fn vox_enabled(&self) -> bool {
        self.image.get(VOX_ENABLED_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read VOX gain (`radio.VoxGain`, 0-9; 0 if unreadable).
    ///
    /// MCP offset `0x101C`. Hardware verified.
    #[must_use]
    pub fn vox_gain(&self) -> u8 {
        self.image
            .get(VOX_GAIN_OFFSET)
            .copied()
            .map_or(0, |b| b.min(9))
    }

    /// Read the VOX delay index (`radio.VoxDelay`, 0-6; 0 if
    /// unreadable).
    ///
    /// MCP offset `0x101D`. Indexes the table 250, 500, 750, 1000,
    /// 1500, 2000, 3000 ms, NOT a 100 ms unit count.
    #[must_use]
    pub fn vox_delay(&self) -> u8 {
        self.image
            .get(VOX_DELAY_OFFSET)
            .copied()
            .map_or(0, |b| b.min(6))
    }

    /// Read VOX TX-on-busy (`radio.VoxTxOnBusy`; false if unreadable).
    ///
    /// MCP offset `0x101E`.
    #[must_use]
    pub fn vox_tx_on_busy(&self) -> bool {
        self.image
            .get(VOX_TX_ON_BUSY_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    // -----------------------------------------------------------------------
    // DTMF
    // -----------------------------------------------------------------------

    /// Read DTMF speed (`radio.DtmfSpeed`, 0-2 = 50/100/150 ms; 0 if
    /// unreadable).
    ///
    /// MCP offset `0x101F`.
    #[must_use]
    pub fn dtmf_speed(&self) -> u8 {
        self.image
            .get(DTMF_SPEED_OFFSET)
            .copied()
            .map_or(0, |b| b.min(2))
    }

    /// Read DTMF pause time (`radio.DtmfPauseTime`, 0-6 =
    /// 100/250/500/750/1000/1500/2000 ms; 0 if unreadable).
    ///
    /// MCP offset `0x1020`.
    #[must_use]
    pub fn dtmf_pause_time(&self) -> u8 {
        self.image
            .get(DTMF_PAUSE_TIME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(6))
    }

    /// Read DTMF TX hold (`radio.DtmfTxHold`; false if unreadable).
    ///
    /// MCP offset `0x1021`.
    #[must_use]
    pub fn dtmf_tx_hold(&self) -> bool {
        self.image.get(DTMF_TX_HOLD_OFFSET).is_some_and(|&b| b != 0)
    }

    // -----------------------------------------------------------------------
    // CW receive
    // -----------------------------------------------------------------------

    /// Read the CW pitch index (`radio.CwPitchFreq`, 0-6; 0 if
    /// unreadable).
    ///
    /// MCP offset `0x1024`.
    #[must_use]
    pub fn cw_pitch(&self) -> u8 {
        self.image
            .get(CW_PITCH_OFFSET)
            .copied()
            .map_or(0, |b| b.min(6))
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    /// Read the auto-mute return time (`radio.AutoMuteRetTime`, 1-10;
    /// 0 if unreadable).
    ///
    /// MCP offset `0x1041`.
    #[must_use]
    pub fn auto_mute_return_time(&self) -> u8 {
        self.image
            .get(AUTO_MUTE_RETURN_TIME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(10))
    }

    /// Read the EMR volume level (`dv.EmrVolumeLevelTxRx`, 1-50; 0 if
    /// unreadable).
    ///
    /// MCP offset `0x1A03`.
    #[must_use]
    pub fn emr_volume_level(&self) -> u8 {
        self.image
            .get(EMR_VOLUME_LEVEL_OFFSET)
            .copied()
            .map_or(0, |b| b.min(50))
    }

    // -----------------------------------------------------------------------
    // Display
    // -----------------------------------------------------------------------

    /// Read backlight control (`radio.BacklightControl`, 0=Manual,
    /// 1=On, 2=Auto, 3=Auto (DC-IN); 0 if unreadable).
    ///
    /// MCP offset `0x1060`.
    #[must_use]
    pub fn backlight_control(&self) -> u8 {
        self.image
            .get(BACKLIGHT_CONTROL_OFFSET)
            .copied()
            .map_or(0, |b| b.min(3))
    }

    /// Read the backlight timer (`radio.BacklightTimer`, 3-60 seconds;
    /// 0 if unreadable).
    ///
    /// MCP offset `0x1061`.
    #[must_use]
    pub fn backlight_timer(&self) -> u8 {
        self.image
            .get(BACKLIGHT_TIMER_OFFSET)
            .copied()
            .map_or(0, |b| b.min(60))
    }

    // -----------------------------------------------------------------------
    // Beep / voice guidance
    // -----------------------------------------------------------------------

    /// Read key beep (`radio.Beep`; false if unreadable).
    ///
    /// MCP offset `0x1071`. Hardware verified.
    #[must_use]
    pub fn key_beep(&self) -> bool {
        self.image.get(KEY_BEEP_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read beep volume (`radio.BeepVolume`, 0-7; 0 if unreadable).
    ///
    /// MCP offset `0x1072`. **0 is a legal value meaning "VOL Link"**
    /// (beep follows the main volume); 1-7 are fixed levels.
    #[must_use]
    pub fn beep_volume(&self) -> u8 {
        self.image
            .get(BEEP_VOLUME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(7))
    }

    /// Read the voice announce mode (`radio.VoiceAnnounce`, 0=Off,
    /// 1=Manual, 2=Auto1, 3=Auto2; 0 if unreadable).
    ///
    /// MCP offset `0x1073`.
    #[must_use]
    pub fn announce(&self) -> u8 {
        self.image
            .get(ANNOUNCE_OFFSET)
            .copied()
            .map_or(0, |b| b.min(3))
    }

    /// Read the voice announce volume (`radio.VoiceAnnounceVolume`,
    /// 0-7; 0 if unreadable).
    ///
    /// MCP offset `0x1074`. **0 is a legal value meaning "VOL Link"**;
    /// 1-7 are fixed levels.
    #[must_use]
    pub fn voice_volume(&self) -> u8 {
        self.image
            .get(VOICE_VOLUME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(7))
    }

    /// Read the voice guidance speed (`radio.VoiceGuidanceSpeed`, 0-3 =
    /// Speed 1-4; 0 if unreadable).
    ///
    /// MCP offset `0x1097`.
    #[must_use]
    pub fn voice_speed(&self) -> u8 {
        self.image
            .get(VOICE_SPEED_OFFSET)
            .copied()
            .map_or(0, |b| b.min(3))
    }

    // -----------------------------------------------------------------------
    // Battery / power
    // -----------------------------------------------------------------------

    /// Read the battery saver interval index (`radio.BatterySaver`,
    /// 0=Off, 1-9 select 0.2-5.0 s; 0 if unreadable).
    ///
    /// MCP offset `0x1076`. This is a 10-value selector, not an on/off
    /// switch.
    #[must_use]
    pub fn battery_saver(&self) -> u8 {
        self.image
            .get(BATTERY_SAVER_OFFSET)
            .copied()
            .map_or(0, |b| b.min(9))
    }

    /// Read auto power off (`radio.AutoPowerOff`).
    ///
    /// MCP offset `0x1077`. Returns [`AutoPowerOff::Off`] if the byte
    /// is out of range or unreadable.
    #[must_use]
    pub fn auto_power_off(&self) -> AutoPowerOff {
        self.image
            .get(AUTO_POWER_OFF_OFFSET)
            .copied()
            .and_then(|b| AutoPowerOff::try_from(b).ok())
            .unwrap_or(AutoPowerOff::Off)
    }

    // -----------------------------------------------------------------------
    // Bluetooth
    // -----------------------------------------------------------------------

    /// Read Bluetooth on/off (`radio.BluetoothOnOff`; false if
    /// unreadable).
    ///
    /// MCP offset `0x1078`. Hardware verified.
    #[must_use]
    pub fn bluetooth(&self) -> bool {
        self.image.get(BLUETOOTH_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read Bluetooth auto-connect (`radio.BluetoothAutoConnect`;
    /// false if unreadable).
    ///
    /// MCP offset `0x1079`.
    #[must_use]
    pub fn bt_auto_connect(&self) -> bool {
        self.image
            .get(BT_AUTO_CONNECT_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    // -----------------------------------------------------------------------
    // PF keys
    // -----------------------------------------------------------------------

    /// Read the exact stored PF1 key assignment byte
    /// (`radio.Pf1PfKey`; 0 if unreadable).
    ///
    /// MCP offset `0x107A`. The official writable domain is 0-30 with
    /// gaps, but hardware probes may leave a known off-menu function code
    /// such as 31 (Screen Capture). Reads preserve such values exactly;
    /// [`SettingsWriter::set_pf_key1`] still enforces the official domain.
    #[must_use]
    pub fn pf_key1(&self) -> u8 {
        self.image.get(PF_KEY1_OFFSET).copied().unwrap_or_default()
    }

    /// Read the exact stored PF2 key assignment byte
    /// (`radio.Pf2PfKey`; 0 if unreadable).
    ///
    /// MCP offset `0x107B`. Reads preserve values outside the official
    /// writable domain so diagnostics cannot silently misreport them.
    #[must_use]
    pub fn pf_key2(&self) -> u8 {
        self.image.get(PF_KEY2_OFFSET).copied().unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Locks
    // -----------------------------------------------------------------------

    /// Read the key-lock configuration bit
    /// (`radio.KeyLockTypeKeyLock`; false if unreadable).
    ///
    /// MCP offset `0x1084`, bit `0x01`. This is the "Key Lock"
    /// checkbox of the lock-type menu, not the runtime lock state (the
    /// latter is CAT `LC`/`DL`).
    #[must_use]
    pub fn key_lock(&self) -> bool {
        self.image
            .get(KEY_LOCK_OFFSET)
            .is_some_and(|&b| b & KEY_LOCK_KEY_MASK != 0)
    }

    /// Read the frequency-lock configuration bit
    /// (`radio.KeyLockTypeFrequencyLock`; false if unreadable).
    ///
    /// MCP offset `0x1084`, bit `0x02`.
    #[must_use]
    pub fn frequency_lock(&self) -> bool {
        self.image
            .get(KEY_LOCK_OFFSET)
            .is_some_and(|&b| b & KEY_LOCK_FREQUENCY_MASK != 0)
    }

    /// Read volume lock (`radio.VolumeLockOnOff`; false if
    /// unreadable).
    ///
    /// MCP offset `0x1087`.
    #[must_use]
    pub fn volume_lock(&self) -> bool {
        self.image.get(VOLUME_LOCK_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read the APRS-lock frequency bit (`aprs.Frequency`; false if
    /// unreadable).
    ///
    /// MCP offset `0x120A`, bit `0x01`. Part of the APRS key-lock
    /// checkbox set.
    #[must_use]
    pub fn aprs_lock_frequency(&self) -> bool {
        self.image
            .get(APRS_LOCK_OFFSET)
            .is_some_and(|&b| b & APRS_LOCK_FREQUENCY_MASK != 0)
    }

    /// Read the APRS-lock PTT bit (`aprs.Ptt`; false if unreadable).
    ///
    /// MCP offset `0x120A`, bit `0x02`.
    #[must_use]
    pub fn aprs_lock_ptt(&self) -> bool {
        self.image
            .get(APRS_LOCK_OFFSET)
            .is_some_and(|&b| b & APRS_LOCK_PTT_MASK != 0)
    }

    /// Read the APRS-lock APRS-key bit (`aprs.AprsKey`; false if
    /// unreadable).
    ///
    /// MCP offset `0x120A`, bit `0x04`.
    #[must_use]
    pub fn aprs_lock_key(&self) -> bool {
        self.image
            .get(APRS_LOCK_OFFSET)
            .is_some_and(|&b| b & APRS_LOCK_KEY_MASK != 0)
    }

    // -----------------------------------------------------------------------
    // Units / language
    // -----------------------------------------------------------------------

    /// Read display unit settings (`radio.SpeedDistance`,
    /// `radio.AltitudeRain`, `radio.Temperature`).
    ///
    /// MCP offsets `0x1088` (speed/distance), `0x1089` (altitude/rain),
    /// `0x108A` (temperature).
    #[must_use]
    pub fn display_units(&self) -> DisplayUnits {
        let speed_distance = match self
            .image
            .get(SPEED_DISTANCE_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => SpeedDistanceUnit::KilometersPerHour,
            2 => SpeedDistanceUnit::Knots,
            _ => SpeedDistanceUnit::MilesPerHour,
        };

        let altitude_rain = match self
            .image
            .get(ALTITUDE_RAIN_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => AltitudeRainUnit::MetersMm,
            _ => AltitudeRainUnit::FeetInch,
        };

        let temperature = match self
            .image
            .get(TEMPERATURE_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => TemperatureUnit::Celsius,
            _ => TemperatureUnit::Fahrenheit,
        };

        DisplayUnits {
            speed_distance,
            altitude_rain,
            temperature,
        }
    }

    /// Read language (`radio.Language`).
    ///
    /// MCP offset `0x1092`. Returns [`Language::English`] if the byte
    /// is out of range or unreadable.
    #[must_use]
    pub fn language(&self) -> Language {
        match self.image.get(LANGUAGE_OFFSET).copied().unwrap_or(0) {
            1 => Language::Japanese,
            _ => Language::English,
        }
    }

    // -----------------------------------------------------------------------
    // Interfaces
    // -----------------------------------------------------------------------

    /// Read the GPS PC-output interface (`radio.PcOutputInterfaceGps`,
    /// 0=USB, 1=Bluetooth; 0 if unreadable).
    ///
    /// MCP offset `0x108E`.
    #[must_use]
    pub fn gps_bt_interface(&self) -> u8 {
        self.image
            .get(GPS_BT_INTERFACE_OFFSET)
            .copied()
            .map_or(0, |b| b.min(1))
    }

    /// Read the APRS PC-output interface
    /// (`radio.PcOutputInterfaceAprs`, 0=USB, 1=Bluetooth; 0 if
    /// unreadable).
    ///
    /// MCP offset `0x108F`.
    #[must_use]
    pub fn aprs_usb_mode(&self) -> u8 {
        self.image
            .get(APRS_USB_MODE_OFFSET)
            .copied()
            .map_or(0, |b| b.min(1))
    }

    // -----------------------------------------------------------------------
    // Band state (hardware-verified, outside registry scope)
    // -----------------------------------------------------------------------

    /// Read Band A power level.
    ///
    /// MCP offset `0x0359`. Hardware verified.
    /// Returns `High` if the byte is out of range or unreadable.
    #[must_use]
    pub fn power_level_a(&self) -> PowerLevel {
        self.image
            .get(POWER_LEVEL_A_OFFSET)
            .copied()
            .and_then(|b| PowerLevel::try_from(b).ok())
            .unwrap_or(PowerLevel::High)
    }

    /// Read Band A attenuator setting (0=off, 1=on).
    ///
    /// MCP offset `0x035C`. Hardware verified.
    #[must_use]
    pub fn attenuator_a(&self) -> bool {
        self.image.get(ATTENUATOR_A_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read dual-band display setting (0=single, 1=dual).
    ///
    /// MCP offset `0x0396`. Hardware verified.
    #[must_use]
    pub fn dual_band(&self) -> bool {
        self.image.get(DUAL_BAND_OFFSET).is_some_and(|&b| b != 0)
    }

    // -----------------------------------------------------------------------
    // Raw numeric accessors for enum-typed settings (for UI +/- cycling)
    // -----------------------------------------------------------------------

    /// Read auto power off as raw byte (0=Off, 1=15m, 2=30m, 3=60m).
    ///
    /// MCP offset `0x1077`.
    #[must_use]
    pub fn auto_power_off_raw(&self) -> u8 {
        self.image
            .get(AUTO_POWER_OFF_OFFSET)
            .copied()
            .map_or(0, |b| b.min(3))
    }

    /// Read speed/distance unit as raw byte (0=mph, 1=km/h, 2=knots).
    ///
    /// MCP offset `0x1088`.
    #[must_use]
    pub fn speed_distance_unit_raw(&self) -> u8 {
        self.image
            .get(SPEED_DISTANCE_UNIT_OFFSET)
            .copied()
            .map_or(0, |b| b.min(2))
    }

    /// Read altitude/rain unit as raw byte (0=ft/in, 1=m/mm).
    ///
    /// MCP offset `0x1089`.
    #[must_use]
    pub fn altitude_rain_unit_raw(&self) -> u8 {
        self.image
            .get(ALTITUDE_RAIN_UNIT_OFFSET)
            .copied()
            .map_or(0, |b| b.min(1))
    }

    /// Read temperature unit as raw byte (0=°F, 1=°C).
    ///
    /// MCP offset `0x108A`.
    #[must_use]
    pub fn temperature_unit_raw(&self) -> u8 {
        self.image
            .get(TEMPERATURE_UNIT_OFFSET)
            .copied()
            .map_or(0, |b| b.min(1))
    }
}

// ---------------------------------------------------------------------------
// SettingsWriter (mutable access)
// ---------------------------------------------------------------------------

/// Mutable access to the system settings region of the memory image.
///
/// Every write method targets a registry-verified offset (or one of
/// the three hardware-verified band-state cells) and clamps or
/// validates the value against the registry domain, so a write can
/// never place an out-of-domain byte in an official menu field.
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

    /// Write `value` to the byte at `offset`, if in bounds.
    fn put(&mut self, offset: usize, value: u8) {
        if let Some(b) = self.image.get_mut(offset) {
            *b = value;
        }
    }

    /// Set or clear `mask` within the shared bit byte at `offset`,
    /// preserving all other bits (masked read-modify-write).
    fn put_bit(&mut self, offset: usize, mask: u8, enabled: bool) {
        if let Some(b) = self.image.get_mut(offset) {
            if enabled {
                *b |= mask;
            } else {
                *b &= !mask;
            }
        }
    }

    // -----------------------------------------------------------------------
    // TX/RX
    // -----------------------------------------------------------------------

    /// Set the beat-shift type (`radio.BeatShift`).
    ///
    /// MCP offset `0x1000`.
    pub fn set_beat_shift(&mut self, value: BeatShift) {
        self.put(BEAT_SHIFT_OFFSET, u8::from(value));
    }

    /// Set TX inhibit on/off (`radio.TxInhibit`).
    ///
    /// MCP offset `0x1001`.
    pub fn set_tx_inhibit(&mut self, enabled: bool) {
        self.put(TX_INHIBIT_OFFSET, u8::from(enabled));
    }

    /// Set the TX timeout timer index (`radio.TimeOutTimer`, clamped
    /// to 0-10).
    ///
    /// MCP offset `0x1003`. The value indexes the 0.5-10.0 minute
    /// table (see [`SettingsAccess::timeout_timer`]).
    pub fn set_timeout_timer(&mut self, value: u8) {
        self.put(TIMEOUT_TIMER_OFFSET, value.min(10));
    }

    /// Set microphone sensitivity (`radio.MicSensitivity`, clamped to
    /// 0-2; **0=High, 1=Medium, 2=Low**).
    ///
    /// MCP offset `0x1006`.
    pub fn set_mic_sensitivity(&mut self, value: u8) {
        self.put(MIC_SENSITIVITY_OFFSET, value.min(2));
    }

    /// Set the SSB high-cut filter (`radio.SsbHighCut`, clamped to
    /// 0-4).
    ///
    /// MCP offset `0x1008`.
    pub fn set_ssb_high_cut(&mut self, value: u8) {
        self.put(SSB_HIGH_CUT_OFFSET, value.min(4));
    }

    /// Set the CW filter width (`radio.CwWidth`, clamped to 0-4).
    ///
    /// MCP offset `0x1009`.
    pub fn set_cw_width(&mut self, value: u8) {
        self.put(CW_WIDTH_OFFSET, value.min(4));
    }

    /// Set the AM high-cut filter (`radio.AmHighCut`, clamped to 0-3).
    ///
    /// MCP offset `0x100A`.
    pub fn set_am_high_cut(&mut self, value: u8) {
        self.put(AM_HIGH_CUT_OFFSET, value.min(3));
    }

    // -----------------------------------------------------------------------
    // Scan
    // -----------------------------------------------------------------------

    /// Set the analog scan-resume method (`radio.ScanResumeAnalog`,
    /// clamped to 0-2).
    ///
    /// MCP offset `0x100C`.
    pub fn set_scan_resume(&mut self, value: u8) {
        self.put(SCAN_RESUME_OFFSET, value.min(2));
    }

    /// Set the digital scan-resume method (`radio.ScanResumeDigital`,
    /// clamped to 0-2).
    ///
    /// MCP offset `0x100D`.
    pub fn set_digital_scan_resume(&mut self, value: u8) {
        self.put(DIGITAL_SCAN_RESUME_OFFSET, value.min(2));
    }

    /// Set the time-operated scan restart time (`radio.TimeRestart`,
    /// clamped to 1-10 seconds).
    ///
    /// MCP offset `0x100E`. 0 is not a legal value for this field.
    pub fn set_scan_restart_time(&mut self, value: u8) {
        self.put(SCAN_RESTART_TIME_OFFSET, value.clamp(1, 10));
    }

    /// Set the carrier-operated scan restart time
    /// (`radio.CarrierRestart`, clamped to 1-10 seconds).
    ///
    /// MCP offset `0x100F`. 0 is not a legal value for this field.
    pub fn set_scan_restart_carrier(&mut self, value: u8) {
        self.put(SCAN_RESTART_CARRIER_OFFSET, value.clamp(1, 10));
    }

    // -----------------------------------------------------------------------
    // Repeater
    // -----------------------------------------------------------------------

    /// Set repeater auto offset on/off (`radio.AutoOffset`).
    ///
    /// MCP offset `0x1018`.
    pub fn set_repeater_auto_offset(&mut self, enabled: bool) {
        self.put(REPEATER_AUTO_OFFSET_OFFSET, u8::from(enabled));
    }

    /// Set the CALL key function (`radio.CallKey`, clamped to 0-1;
    /// 0=CALL, 1=1750 Hz).
    ///
    /// MCP offset `0x1019`.
    pub fn set_repeater_call_key(&mut self, value: u8) {
        self.put(REPEATER_CALL_KEY_OFFSET, value.min(1));
    }

    // -----------------------------------------------------------------------
    // VOX
    // -----------------------------------------------------------------------

    /// Set VOX enabled on/off (`radio.Vox`).
    ///
    /// MCP offset `0x101B`. Hardware verified.
    pub fn set_vox_enabled(&mut self, enabled: bool) {
        self.put(VOX_ENABLED_OFFSET, u8::from(enabled));
    }

    /// Set VOX gain level (`radio.VoxGain`, clamped to 0-9).
    ///
    /// MCP offset `0x101C`. Hardware verified.
    pub fn set_vox_gain(&mut self, gain: u8) {
        self.put(VOX_GAIN_OFFSET, gain.min(9));
    }

    /// Set the VOX delay index (`radio.VoxDelay`, clamped to 0-6).
    ///
    /// MCP offset `0x101D`. The value indexes the 250-3000 ms table
    /// (see [`SettingsAccess::vox_delay`]); it is NOT a 100 ms unit
    /// count.
    pub fn set_vox_delay(&mut self, delay: u8) {
        self.put(VOX_DELAY_OFFSET, delay.min(6));
    }

    /// Set VOX TX-on-busy on/off (`radio.VoxTxOnBusy`).
    ///
    /// MCP offset `0x101E`.
    pub fn set_vox_tx_on_busy(&mut self, enabled: bool) {
        self.put(VOX_TX_ON_BUSY_OFFSET, u8::from(enabled));
    }

    // -----------------------------------------------------------------------
    // DTMF
    // -----------------------------------------------------------------------

    /// Set DTMF speed (`radio.DtmfSpeed`, clamped to 0-2).
    ///
    /// MCP offset `0x101F`.
    pub fn set_dtmf_speed(&mut self, value: u8) {
        self.put(DTMF_SPEED_OFFSET, value.min(2));
    }

    /// Set DTMF pause time (`radio.DtmfPauseTime`, clamped to 0-6).
    ///
    /// MCP offset `0x1020`.
    pub fn set_dtmf_pause_time(&mut self, value: u8) {
        self.put(DTMF_PAUSE_TIME_OFFSET, value.min(6));
    }

    /// Set DTMF TX hold on/off (`radio.DtmfTxHold`).
    ///
    /// MCP offset `0x1021`.
    pub fn set_dtmf_tx_hold(&mut self, enabled: bool) {
        self.put(DTMF_TX_HOLD_OFFSET, u8::from(enabled));
    }

    // -----------------------------------------------------------------------
    // CW receive
    // -----------------------------------------------------------------------

    /// Set the CW pitch index (`radio.CwPitchFreq`, clamped to 0-6).
    ///
    /// MCP offset `0x1024`.
    pub fn set_cw_pitch(&mut self, value: u8) {
        self.put(CW_PITCH_OFFSET, value.min(6));
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    /// Set the auto-mute return time (`radio.AutoMuteRetTime`, clamped
    /// to 1-10).
    ///
    /// MCP offset `0x1041`. 0 is not a legal value for this field.
    pub fn set_auto_mute_return_time(&mut self, value: u8) {
        self.put(AUTO_MUTE_RETURN_TIME_OFFSET, value.clamp(1, 10));
    }

    /// Set the EMR volume level (`dv.EmrVolumeLevelTxRx`, clamped to
    /// 1-50).
    ///
    /// MCP offset `0x1A03`. 0 is not a legal value for this field.
    pub fn set_emr_volume_level(&mut self, value: u8) {
        self.put(EMR_VOLUME_LEVEL_OFFSET, value.clamp(1, 50));
    }

    // -----------------------------------------------------------------------
    // Display
    // -----------------------------------------------------------------------

    /// Set backlight control (`radio.BacklightControl`, clamped to
    /// 0-3).
    ///
    /// MCP offset `0x1060`.
    pub fn set_backlight_control(&mut self, value: u8) {
        self.put(BACKLIGHT_CONTROL_OFFSET, value.min(3));
    }

    /// Set the backlight timer (`radio.BacklightTimer`, clamped to
    /// 3-60 seconds).
    ///
    /// MCP offset `0x1061`. Values below 3 are not legal for this
    /// field.
    pub fn set_backlight_timer(&mut self, value: u8) {
        self.put(BACKLIGHT_TIMER_OFFSET, value.clamp(3, 60));
    }

    // -----------------------------------------------------------------------
    // Beep / voice guidance
    // -----------------------------------------------------------------------

    /// Set key beep on/off (`radio.Beep`).
    ///
    /// MCP offset `0x1071`. Hardware verified.
    pub fn set_key_beep(&mut self, enabled: bool) {
        self.put(KEY_BEEP_OFFSET, u8::from(enabled));
    }

    /// Set beep volume (`radio.BeepVolume`, clamped to 0-7; 0 = VOL
    /// Link).
    ///
    /// MCP offset `0x1072`.
    pub fn set_beep_volume(&mut self, volume: u8) {
        self.put(BEEP_VOLUME_OFFSET, volume.min(7));
    }

    /// Set the voice announce mode (`radio.VoiceAnnounce`, clamped to
    /// 0-3; 0=Off, 1=Manual, 2=Auto1, 3=Auto2).
    ///
    /// MCP offset `0x1073`.
    pub fn set_announce(&mut self, value: u8) {
        self.put(ANNOUNCE_OFFSET, value.min(3));
    }

    /// Set the voice announce volume (`radio.VoiceAnnounceVolume`,
    /// clamped to 0-7; 0 = VOL Link).
    ///
    /// MCP offset `0x1074`.
    pub fn set_voice_volume(&mut self, value: u8) {
        self.put(VOICE_VOLUME_OFFSET, value.min(7));
    }

    /// Set the voice guidance speed (`radio.VoiceGuidanceSpeed`,
    /// clamped to 0-3).
    ///
    /// MCP offset `0x1097`.
    pub fn set_voice_speed(&mut self, value: u8) {
        self.put(VOICE_SPEED_OFFSET, value.min(3));
    }

    // -----------------------------------------------------------------------
    // Battery / power
    // -----------------------------------------------------------------------

    /// Set the battery saver interval index (`radio.BatterySaver`,
    /// clamped to 0-9; 0 = Off).
    ///
    /// MCP offset `0x1076`.
    pub fn set_battery_saver(&mut self, value: u8) {
        self.put(BATTERY_SAVER_OFFSET, value.min(9));
    }

    /// Set auto power off (`radio.AutoPowerOff`).
    ///
    /// MCP offset `0x1077`.
    pub fn set_auto_power_off(&mut self, value: AutoPowerOff) {
        self.put(AUTO_POWER_OFF_OFFSET, u8::from(value));
    }

    // -----------------------------------------------------------------------
    // Bluetooth
    // -----------------------------------------------------------------------

    /// Set Bluetooth on/off (`radio.BluetoothOnOff`).
    ///
    /// MCP offset `0x1078`. Hardware verified.
    pub fn set_bluetooth(&mut self, enabled: bool) {
        self.put(BLUETOOTH_OFFSET, u8::from(enabled));
    }

    /// Set Bluetooth auto-connect on/off
    /// (`radio.BluetoothAutoConnect`).
    ///
    /// MCP offset `0x1079`.
    pub fn set_bt_auto_connect(&mut self, enabled: bool) {
        self.put(BT_AUTO_CONNECT_OFFSET, u8::from(enabled));
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
    /// Returns [`ValidationError::SettingOutOfRange`] if `value` is
    /// outside the gapped domain (0-30 excluding 5, 23, 25, 26); a
    /// plain clamp cannot express the gaps, and writing a gap value
    /// would store an invalid menu selection.
    pub fn set_pf_key1(&mut self, value: u8) -> Result<(), ValidationError> {
        validate_pf_key("PF1 key", value)?;
        self.put(PF_KEY1_OFFSET, value);
        Ok(())
    }

    /// Set the PF2 key assignment (`radio.Pf2PfKey`).
    ///
    /// MCP offset `0x107B`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value` is
    /// outside the gapped domain (0-30 excluding 5, 23, 25, 26).
    pub fn set_pf_key2(&mut self, value: u8) -> Result<(), ValidationError> {
        validate_pf_key("PF2 key", value)?;
        self.put(PF_KEY2_OFFSET, value);
        Ok(())
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
    pub fn set_key_lock(&mut self, enabled: bool) {
        self.put_bit(KEY_LOCK_OFFSET, KEY_LOCK_KEY_MASK, enabled);
    }

    /// Set the frequency-lock configuration bit
    /// (`radio.KeyLockTypeFrequencyLock`).
    ///
    /// MCP offset `0x1084`, bit `0x02`. Masked read-modify-write.
    pub fn set_frequency_lock(&mut self, enabled: bool) {
        self.put_bit(KEY_LOCK_OFFSET, KEY_LOCK_FREQUENCY_MASK, enabled);
    }

    /// Set volume lock on/off (`radio.VolumeLockOnOff`).
    ///
    /// MCP offset `0x1087`.
    pub fn set_volume_lock(&mut self, enabled: bool) {
        self.put(VOLUME_LOCK_OFFSET, u8::from(enabled));
    }

    /// Set the APRS-lock frequency bit (`aprs.Frequency`).
    ///
    /// MCP offset `0x120A`, bit `0x01`. Masked read-modify-write: the
    /// other APRS-lock bits and the five unowned bits are preserved.
    pub fn set_aprs_lock_frequency(&mut self, enabled: bool) {
        self.put_bit(APRS_LOCK_OFFSET, APRS_LOCK_FREQUENCY_MASK, enabled);
    }

    /// Set the APRS-lock PTT bit (`aprs.Ptt`).
    ///
    /// MCP offset `0x120A`, bit `0x02`. Masked read-modify-write.
    pub fn set_aprs_lock_ptt(&mut self, enabled: bool) {
        self.put_bit(APRS_LOCK_OFFSET, APRS_LOCK_PTT_MASK, enabled);
    }

    /// Set the APRS-lock APRS-key bit (`aprs.AprsKey`).
    ///
    /// MCP offset `0x120A`, bit `0x04`. Masked read-modify-write.
    pub fn set_aprs_lock_key(&mut self, enabled: bool) {
        self.put_bit(APRS_LOCK_OFFSET, APRS_LOCK_KEY_MASK, enabled);
    }

    // -----------------------------------------------------------------------
    // Units / language
    // -----------------------------------------------------------------------

    /// Set speed/distance display unit (`radio.SpeedDistance`).
    ///
    /// MCP offset `0x1088`.
    pub fn set_speed_distance_unit(&mut self, value: SpeedDistanceUnit) {
        let raw = match value {
            SpeedDistanceUnit::MilesPerHour => 0,
            SpeedDistanceUnit::KilometersPerHour => 1,
            SpeedDistanceUnit::Knots => 2,
        };
        self.put(SPEED_DISTANCE_UNIT_OFFSET, raw);
    }

    /// Set altitude/rain display unit (`radio.AltitudeRain`).
    ///
    /// MCP offset `0x1089`.
    pub fn set_altitude_rain_unit(&mut self, value: AltitudeRainUnit) {
        let raw = match value {
            AltitudeRainUnit::FeetInch => 0,
            AltitudeRainUnit::MetersMm => 1,
        };
        self.put(ALTITUDE_RAIN_UNIT_OFFSET, raw);
    }

    /// Set temperature display unit (`radio.Temperature`).
    ///
    /// MCP offset `0x108A`.
    pub fn set_temperature_unit(&mut self, value: TemperatureUnit) {
        let raw = match value {
            TemperatureUnit::Fahrenheit => 0,
            TemperatureUnit::Celsius => 1,
        };
        self.put(TEMPERATURE_UNIT_OFFSET, raw);
    }

    /// Set language (`radio.Language`).
    ///
    /// MCP offset `0x1092`.
    pub fn set_language(&mut self, value: Language) {
        let raw = match value {
            Language::English => 0,
            Language::Japanese => 1,
        };
        self.put(LANGUAGE_OFFSET, raw);
    }

    // -----------------------------------------------------------------------
    // Interfaces
    // -----------------------------------------------------------------------

    /// Set the GPS PC-output interface (`radio.PcOutputInterfaceGps`,
    /// clamped to 0-1; 0=USB, 1=Bluetooth).
    ///
    /// MCP offset `0x108E`.
    pub fn set_gps_bt_interface(&mut self, value: u8) {
        self.put(GPS_BT_INTERFACE_OFFSET, value.min(1));
    }

    /// Set the APRS PC-output interface
    /// (`radio.PcOutputInterfaceAprs`, clamped to 0-1; 0=USB,
    /// 1=Bluetooth).
    ///
    /// MCP offset `0x108F`.
    pub fn set_aprs_usb_mode(&mut self, value: u8) {
        self.put(APRS_USB_MODE_OFFSET, value.min(1));
    }

    // -----------------------------------------------------------------------
    // Band state (hardware-verified, outside registry scope)
    // -----------------------------------------------------------------------

    /// Set Band A power level.
    ///
    /// MCP offset `0x0359`. Hardware verified.
    pub fn set_power_level_a(&mut self, level: PowerLevel) {
        self.put(POWER_LEVEL_A_OFFSET, u8::from(level));
    }

    /// Set Band A attenuator on/off.
    ///
    /// MCP offset `0x035C`. Hardware verified.
    pub fn set_attenuator_a(&mut self, enabled: bool) {
        self.put(ATTENUATOR_A_OFFSET, u8::from(enabled));
    }

    /// Set dual-band display on/off.
    ///
    /// MCP offset `0x0396`. Hardware verified.
    pub fn set_dual_band(&mut self, enabled: bool) {
        self.put(DUAL_BAND_OFFSET, u8::from(enabled));
    }

    // -----------------------------------------------------------------------
    // Raw numeric setters for enum-typed settings (for UI +/- cycling)
    // -----------------------------------------------------------------------

    /// Set auto power off as raw byte (0=Off, 1=15m, 2=30m, 3=60m;
    /// clamped to 0-3).
    ///
    /// MCP offset `0x1077`.
    pub fn set_auto_power_off_raw(&mut self, value: u8) {
        self.put(AUTO_POWER_OFF_OFFSET, value.min(3));
    }

    /// Set speed/distance unit as raw byte (0=mph, 1=km/h, 2=knots;
    /// clamped to 0-2).
    ///
    /// MCP offset `0x1088`.
    pub fn set_speed_distance_unit_raw(&mut self, value: u8) {
        self.put(SPEED_DISTANCE_UNIT_OFFSET, value.min(2));
    }

    /// Set altitude/rain unit as raw byte (0=ft/in, 1=m/mm; clamped to
    /// 0-1).
    ///
    /// MCP offset `0x1089`.
    pub fn set_altitude_rain_unit_raw(&mut self, value: u8) {
        self.put(ALTITUDE_RAIN_UNIT_OFFSET, value.min(1));
    }

    /// Set temperature unit as raw byte (0=°F, 1=°C; clamped to 0-1).
    ///
    /// MCP offset `0x108A`.
    pub fn set_temperature_unit_raw(&mut self, value: u8) {
        self.put(TEMPERATURE_UNIT_OFFSET, value.min(1));
    }
}

/// Extract a null-terminated ASCII string from the image at a given offset.
fn extract_string(image: &[u8], offset: usize, max_len: usize) -> String {
    let Some(slice) = image.get(offset..offset + max_len) else {
        return String::new();
    };
    let nul = slice.iter().position(|&b| b == 0).unwrap_or(max_len);
    let Some(trimmed) = slice.get(..nul) else {
        return String::new();
    };
    String::from_utf8_lossy(trimmed).trim().to_string()
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
        assert_eq!(settings.power_on_message(), "Hello D75!");
        Ok(())
    }

    #[test]
    fn settings_raw_not_none() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let settings = mi.settings();
        let raw = settings
            .raw()
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
        assert!(mi.settings().key_beep());
        Ok(())
    }

    #[test]
    fn settings_key_beep_off() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.settings().key_beep());
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
        assert!(settings.vox_enabled());
        assert_eq!(settings.vox_gain(), 7);
        assert_eq!(settings.vox_delay(), 5);
        Ok(())
    }

    #[test]
    fn settings_vox_delay_clamped_to_registry_domain() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, VOX_DELAY_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().vox_delay(), 6);
        Ok(())
    }

    #[test]
    fn settings_dual_band() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, DUAL_BAND_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().dual_band());
        Ok(())
    }

    #[test]
    fn settings_dual_band_off() -> TestResult {
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.settings().dual_band());
        Ok(())
    }

    #[test]
    fn settings_attenuator_a() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, ATTENUATOR_A_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().attenuator_a());
        Ok(())
    }

    #[test]
    fn settings_power_level_a() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, POWER_LEVEL_A_OFFSET, 2)?; // Lo
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().power_level_a(), PowerLevel::Low);
        Ok(())
    }

    #[test]
    fn settings_power_level_a_invalid_defaults_to_high() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, POWER_LEVEL_A_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().power_level_a(), PowerLevel::High);
        Ok(())
    }

    #[test]
    fn settings_bluetooth() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BLUETOOTH_OFFSET, 1)?;
        set_byte(&mut image, BT_AUTO_CONNECT_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().bluetooth());
        assert!(mi.settings().bt_auto_connect());
        Ok(())
    }

    #[test]
    fn settings_beep_volume_zero_is_vol_link() -> TestResult {
        // 0 is a legal stored value (VOL Link), not an error or a
        // clamped minimum.
        let image = make_settings_image()?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().beep_volume(), 0);
        Ok(())
    }

    #[test]
    fn settings_beep_volume_clamped() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BEEP_VOLUME_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().beep_volume(), 7);
        Ok(())
    }

    #[test]
    fn settings_backlight_control() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BACKLIGHT_CONTROL_OFFSET, 2)?; // Auto
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().backlight_control(), 2);
        Ok(())
    }

    #[test]
    fn settings_auto_power_off() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, AUTO_POWER_OFF_OFFSET, 2)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        // Raw 2 is 30 minutes on the D75 (0=Off, 1=15, 2=30, 3=60).
        assert_eq!(mi.settings().auto_power_off(), AutoPowerOff::Min30);
        Ok(())
    }

    #[test]
    fn settings_auto_power_off_raw_one_is_fifteen_minutes() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, AUTO_POWER_OFF_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().auto_power_off(), AutoPowerOff::Min15);
        assert_eq!(mi.settings().auto_power_off_raw(), 1);
        Ok(())
    }

    #[test]
    fn settings_auto_power_off_unknown_defaults_to_off() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, AUTO_POWER_OFF_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().auto_power_off(), AutoPowerOff::Off);
        Ok(())
    }

    #[test]
    fn settings_battery_saver_is_an_interval_index() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BATTERY_SAVER_OFFSET, 9)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().battery_saver(), 9);
        Ok(())
    }

    #[test]
    fn settings_beat_shift() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BEAT_SHIFT_OFFSET, 7)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().beat_shift(), BeatShift::Type8);
        Ok(())
    }

    #[test]
    fn settings_beat_shift_invalid_defaults_to_type1() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, BEAT_SHIFT_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().beat_shift(), BeatShift::Type1);
        Ok(())
    }

    #[test]
    fn settings_key_lock_bits() -> TestResult {
        let mut image = make_settings_image()?;
        // Both configuration bits set, plus unowned upper bits that
        // must not leak into either reader.
        set_byte(&mut image, KEY_LOCK_OFFSET, 0xFC | 0x01)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().key_lock());
        assert!(!mi.settings().frequency_lock());
        Ok(())
    }

    #[test]
    fn settings_aprs_lock_bits() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, APRS_LOCK_OFFSET, 0x05)?; // frequency + APRS key
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.settings().aprs_lock_frequency());
        assert!(!mi.settings().aprs_lock_ptt());
        assert!(mi.settings().aprs_lock_key());
        Ok(())
    }

    #[test]
    fn settings_display_units() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, SPEED_DISTANCE_UNIT_OFFSET, 1)?; // km/h
        set_byte(&mut image, ALTITUDE_RAIN_UNIT_OFFSET, 1)?; // m/mm
        set_byte(&mut image, TEMPERATURE_UNIT_OFFSET, 1)?; // Celsius
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let units = mi.settings().display_units();
        assert_eq!(units.speed_distance, SpeedDistanceUnit::KilometersPerHour);
        assert_eq!(units.altitude_rain, AltitudeRainUnit::MetersMm);
        assert_eq!(units.temperature, TemperatureUnit::Celsius);
        Ok(())
    }

    #[test]
    fn settings_language() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, LANGUAGE_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.settings().language(), Language::Japanese);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Write accessor tests (SettingsWriter)
    // -----------------------------------------------------------------------

    #[test]
    fn write_key_beep() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.settings().key_beep());
        mi.settings_mut().set_key_beep(true);
        assert!(mi.settings().key_beep());
        mi.settings_mut().set_key_beep(false);
        assert!(!mi.settings().key_beep());
        Ok(())
    }

    #[test]
    fn write_vox_gain_clamped() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_vox_gain(0xFF);
        assert_eq!(mi.settings().vox_gain(), 9);
        Ok(())
    }

    #[test]
    fn write_vox_delay_clamped_to_registry_domain() -> TestResult {
        // The legacy setter accepted up to 30 ("100 ms units" fiction);
        // the registry domain is 0-6 (250-3000 ms table).
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_vox_delay(30);
        assert_eq!(get_byte(mi.as_raw(), VOX_DELAY_OFFSET)?, 6);
        Ok(())
    }

    #[test]
    fn write_backlight_timer_clamps_to_min_three() -> TestResult {
        // radio.BacklightTimer has domain 3-60: values below 3 are not
        // legal stored bytes.
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_backlight_timer(0);
        assert_eq!(get_byte(mi.as_raw(), BACKLIGHT_TIMER_OFFSET)?, 3);
        mi.settings_mut().set_backlight_timer(0xFF);
        assert_eq!(get_byte(mi.as_raw(), BACKLIGHT_TIMER_OFFSET)?, 60);
        Ok(())
    }

    #[test]
    fn write_scan_restart_times_clamp_to_min_one() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_scan_restart_time(0);
        mi.settings_mut().set_scan_restart_carrier(0);
        assert_eq!(get_byte(mi.as_raw(), SCAN_RESTART_TIME_OFFSET)?, 1);
        assert_eq!(get_byte(mi.as_raw(), SCAN_RESTART_CARRIER_OFFSET)?, 1);
        Ok(())
    }

    #[test]
    fn write_emr_volume_level_clamps_to_domain() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_emr_volume_level(0);
        assert_eq!(get_byte(mi.as_raw(), EMR_VOLUME_LEVEL_OFFSET)?, 1);
        mi.settings_mut().set_emr_volume_level(0xFF);
        assert_eq!(get_byte(mi.as_raw(), EMR_VOLUME_LEVEL_OFFSET)?, 50);
        Ok(())
    }

    #[test]
    fn write_beat_shift() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_beat_shift(BeatShift::Type5);
        assert_eq!(mi.settings().beat_shift(), BeatShift::Type5);
        assert_eq!(get_byte(mi.as_raw(), BEAT_SHIFT_OFFSET)?, 4);
        Ok(())
    }

    #[test]
    fn write_auto_power_off_enum_encoding() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_auto_power_off(AutoPowerOff::Min15);
        assert_eq!(get_byte(mi.as_raw(), AUTO_POWER_OFF_OFFSET)?, 1);
        mi.settings_mut().set_auto_power_off(AutoPowerOff::Min60);
        assert_eq!(get_byte(mi.as_raw(), AUTO_POWER_OFF_OFFSET)?, 3);
        Ok(())
    }

    #[test]
    fn write_pf_keys_validate_the_gapped_domain() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;
        mi.settings_mut().set_pf_key1(30)?;
        assert_eq!(mi.settings().pf_key1(), 30);
        mi.settings_mut().set_pf_key2(11)?;
        assert_eq!(mi.settings().pf_key2(), 11);

        for invalid in [5u8, 23, 25, 26, 31, 0xFF] {
            let result = mi.settings_mut().set_pf_key1(invalid);
            assert!(
                result.is_err(),
                "PF1 raw {invalid} must be rejected: {result:?}"
            );
            let result = mi.settings_mut().set_pf_key2(invalid);
            assert!(
                result.is_err(),
                "PF2 raw {invalid} must be rejected: {result:?}"
            );
        }
        // Rejected writes must not have modified the stored bytes.
        assert_eq!(mi.settings().pf_key1(), 30);
        assert_eq!(mi.settings().pf_key2(), 11);
        Ok(())
    }

    #[test]
    fn read_pf_keys_preserves_noncanonical_raw_values() -> TestResult {
        let mut image = make_settings_image()?;
        set_byte(&mut image, PF_KEY1_OFFSET, 31)?;
        set_byte(&mut image, PF_KEY2_OFFSET, 0xFF)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;

        assert_eq!(mi.settings().pf_key1(), 31);
        assert_eq!(mi.settings().pf_key2(), 0xFF);
        Ok(())
    }

    #[test]
    fn key_lock_setters_preserve_unowned_bits() -> TestResult {
        let mut image = make_settings_image()?;
        // Seed the shared byte with all six unowned bits set.
        set_byte(&mut image, KEY_LOCK_OFFSET, 0xFC)?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;

        mi.settings_mut().set_key_lock(true);
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFD);
        mi.settings_mut().set_frequency_lock(true);
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFF);
        mi.settings_mut().set_key_lock(false);
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFE);
        mi.settings_mut().set_frequency_lock(false);
        assert_eq!(get_byte(mi.as_raw(), KEY_LOCK_OFFSET)?, 0xFC);
        Ok(())
    }

    #[test]
    fn aprs_lock_setters_preserve_unowned_bits() -> TestResult {
        let mut image = make_settings_image()?;
        // Seed the shared byte with all five unowned bits set.
        set_byte(&mut image, APRS_LOCK_OFFSET, 0xF8)?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;

        mi.settings_mut().set_aprs_lock_frequency(true);
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xF9);
        mi.settings_mut().set_aprs_lock_ptt(true);
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xFB);
        mi.settings_mut().set_aprs_lock_key(true);
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xFF);
        mi.settings_mut().set_aprs_lock_ptt(false);
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xFD);
        mi.settings_mut().set_aprs_lock_frequency(false);
        mi.settings_mut().set_aprs_lock_key(false);
        assert_eq!(get_byte(mi.as_raw(), APRS_LOCK_OFFSET)?, 0xF8);
        Ok(())
    }

    #[test]
    fn write_roundtrip_hardware_verified_cells() -> TestResult {
        let image = make_settings_image()?;
        let mut mi = crate::memory::MemoryImage::from_raw(image)?;

        // Set everything to non-default values.
        mi.settings_mut().set_key_beep(true);
        mi.settings_mut().set_vox_enabled(true);
        mi.settings_mut().set_vox_gain(9);
        mi.settings_mut().set_dual_band(true);
        mi.settings_mut().set_attenuator_a(true);
        mi.settings_mut().set_power_level_a(PowerLevel::ExtraLow);
        mi.settings_mut().set_bluetooth(true);

        // Verify reads match.
        let s = mi.settings();
        assert!(s.key_beep());
        assert!(s.vox_enabled());
        assert_eq!(s.vox_gain(), 9);
        assert!(s.dual_band());
        assert!(s.attenuator_a());
        assert_eq!(s.power_level_a(), PowerLevel::ExtraLow);
        assert!(s.bluetooth());

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
    type ApplySetter = fn(&mut SettingsWriter<'_>);

    /// Every settings setter, paired with the flash offset it must
    /// write, transcribed INDEPENDENTLY from the MCP-D75 registry map
    /// (hex literals, not the `*_OFFSET` constants): a transposed
    /// digit in a constant would corrupt an unrelated radio setting on
    /// MCP write-back, and the offset audit below is the test that
    /// catches it. The third column carries the owned bit mask for
    /// setters that share a byte with other registry fields.
    static SETTER_OFFSET_TABLE: &[(&str, usize, Option<u8>, ApplySetter)] = &[
        ("set_beat_shift", 0x1000, None, |s| {
            s.set_beat_shift(BeatShift::Type2);
        }),
        ("set_tx_inhibit", 0x1001, None, |s| s.set_tx_inhibit(true)),
        ("set_timeout_timer", 0x1003, None, |s| {
            s.set_timeout_timer(1);
        }),
        ("set_mic_sensitivity", 0x1006, None, |s| {
            s.set_mic_sensitivity(1);
        }),
        ("set_ssb_high_cut", 0x1008, None, |s| s.set_ssb_high_cut(1)),
        ("set_cw_width", 0x1009, None, |s| s.set_cw_width(1)),
        ("set_am_high_cut", 0x100A, None, |s| s.set_am_high_cut(1)),
        ("set_scan_resume", 0x100C, None, |s| s.set_scan_resume(1)),
        ("set_digital_scan_resume", 0x100D, None, |s| {
            s.set_digital_scan_resume(1);
        }),
        ("set_scan_restart_time", 0x100E, None, |s| {
            s.set_scan_restart_time(1);
        }),
        ("set_scan_restart_carrier", 0x100F, None, |s| {
            s.set_scan_restart_carrier(1);
        }),
        ("set_repeater_auto_offset", 0x1018, None, |s| {
            s.set_repeater_auto_offset(true);
        }),
        ("set_repeater_call_key", 0x1019, None, |s| {
            s.set_repeater_call_key(1);
        }),
        ("set_vox_enabled", 0x101B, None, |s| s.set_vox_enabled(true)),
        ("set_vox_gain", 0x101C, None, |s| s.set_vox_gain(1)),
        ("set_vox_delay", 0x101D, None, |s| s.set_vox_delay(1)),
        ("set_vox_tx_on_busy", 0x101E, None, |s| {
            s.set_vox_tx_on_busy(true);
        }),
        ("set_dtmf_speed", 0x101F, None, |s| s.set_dtmf_speed(1)),
        ("set_dtmf_pause_time", 0x1020, None, |s| {
            s.set_dtmf_pause_time(1);
        }),
        ("set_dtmf_tx_hold", 0x1021, None, |s| {
            s.set_dtmf_tx_hold(true);
        }),
        ("set_cw_pitch", 0x1024, None, |s| s.set_cw_pitch(1)),
        ("set_auto_mute_return_time", 0x1041, None, |s| {
            s.set_auto_mute_return_time(2);
        }),
        ("set_backlight_control", 0x1060, None, |s| {
            s.set_backlight_control(1);
        }),
        ("set_backlight_timer", 0x1061, None, |s| {
            s.set_backlight_timer(10);
        }),
        ("set_key_beep", 0x1071, None, |s| s.set_key_beep(true)),
        ("set_beep_volume", 0x1072, None, |s| s.set_beep_volume(1)),
        ("set_announce", 0x1073, None, |s| s.set_announce(1)),
        ("set_voice_volume", 0x1074, None, |s| s.set_voice_volume(1)),
        ("set_battery_saver", 0x1076, None, |s| {
            s.set_battery_saver(1);
        }),
        ("set_auto_power_off", 0x1077, None, |s| {
            s.set_auto_power_off(AutoPowerOff::Min15);
        }),
        ("set_auto_power_off_raw", 0x1077, None, |s| {
            s.set_auto_power_off_raw(1);
        }),
        ("set_bluetooth", 0x1078, None, |s| s.set_bluetooth(true)),
        ("set_bt_auto_connect", 0x1079, None, |s| {
            s.set_bt_auto_connect(true);
        }),
        ("set_pf_key1", 0x107A, None, |s| {
            drop(s.set_pf_key1(1));
        }),
        ("set_pf_key2", 0x107B, None, |s| {
            drop(s.set_pf_key2(1));
        }),
        ("set_key_lock", 0x1084, Some(0x01), |s| s.set_key_lock(true)),
        ("set_frequency_lock", 0x1084, Some(0x02), |s| {
            s.set_frequency_lock(true);
        }),
        ("set_volume_lock", 0x1087, None, |s| s.set_volume_lock(true)),
        ("set_speed_distance_unit", 0x1088, None, |s| {
            s.set_speed_distance_unit(SpeedDistanceUnit::KilometersPerHour);
        }),
        ("set_speed_distance_unit_raw", 0x1088, None, |s| {
            s.set_speed_distance_unit_raw(1);
        }),
        ("set_altitude_rain_unit", 0x1089, None, |s| {
            s.set_altitude_rain_unit(AltitudeRainUnit::MetersMm);
        }),
        ("set_altitude_rain_unit_raw", 0x1089, None, |s| {
            s.set_altitude_rain_unit_raw(1);
        }),
        ("set_temperature_unit", 0x108A, None, |s| {
            s.set_temperature_unit(TemperatureUnit::Celsius);
        }),
        ("set_temperature_unit_raw", 0x108A, None, |s| {
            s.set_temperature_unit_raw(1);
        }),
        ("set_gps_bt_interface", 0x108E, None, |s| {
            s.set_gps_bt_interface(1);
        }),
        ("set_aprs_usb_mode", 0x108F, None, |s| {
            s.set_aprs_usb_mode(1);
        }),
        ("set_language", 0x1092, None, |s| {
            s.set_language(Language::Japanese);
        }),
        ("set_voice_speed", 0x1097, None, |s| s.set_voice_speed(1)),
        ("set_aprs_lock_frequency", 0x120A, Some(0x01), |s| {
            s.set_aprs_lock_frequency(true);
        }),
        ("set_aprs_lock_ptt", 0x120A, Some(0x02), |s| {
            s.set_aprs_lock_ptt(true);
        }),
        ("set_aprs_lock_key", 0x120A, Some(0x04), |s| {
            s.set_aprs_lock_key(true);
        }),
        ("set_emr_volume_level", 0x1A03, None, |s| {
            s.set_emr_volume_level(2);
        }),
        ("set_power_level_a", 0x0359, None, |s| {
            s.set_power_level_a(PowerLevel::ExtraLow);
        }),
        ("set_attenuator_a", 0x035C, None, |s| {
            s.set_attenuator_a(true);
        }),
        ("set_dual_band", 0x0396, None, |s| s.set_dual_band(true)),
    ];

    /// Applies every entry of [`SETTER_OFFSET_TABLE`] to a fresh image
    /// and asserts exactly one byte changed, at the documented offset
    /// (no neighbor stomping, no wrong-offset writes). For masked bit
    /// setters, additionally asserts the change stayed within the
    /// owned bits.
    #[test]
    fn every_setter_writes_exactly_one_byte_at_its_documented_offset() -> TestResult {
        for (name, offset, mask, apply) in SETTER_OFFSET_TABLE {
            let image = make_settings_image()?;
            let mut mi = crate::memory::MemoryImage::from_raw(image)?;
            let before = mi.as_raw().to_vec();
            apply(&mut mi.settings_mut());
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
    /// Byte domains are the clamp bounds the setters enforce, so a
    /// registry regeneration that changes a domain fails this test
    /// until the accessor is re-audited.
    enum ExpectedCodec {
        /// `FieldCodec::Bool` (one byte holding 0/1).
        Bool,
        /// `FieldCodec::Byte` with this inclusive domain.
        Byte {
            /// Smallest legal raw value (setter clamp floor).
            min: u8,
            /// Largest legal raw value (setter clamp ceiling).
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
                        "{name}: accessor clamp bounds diverge from the registry domain"
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

    /// The PF-key setters hand-code the registry's gapped domain; pin
    /// the gap set to the generated option list so a registry change
    /// cannot silently outdate the validator.
    #[test]
    fn pf_key_validation_matches_the_registry_options() -> TestResult {
        for name in ["radio.Pf1PfKey", "radio.Pf2PfKey"] {
            let field = menu_field(name).ok_or_else(|| format!("{name} is missing"))?;
            for raw in 0..=30_u8 {
                let in_registry = field.option(u64::from(raw)).is_some();
                assert_eq!(
                    pf_key_valid(raw),
                    in_registry,
                    "{name}: validator and registry disagree about raw value {raw}"
                );
            }
        }
        Ok(())
    }
}
