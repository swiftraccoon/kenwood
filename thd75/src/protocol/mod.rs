//! Pure-logic CAT (Computer Aided Transceiver) command codec.
//!
//! This module handles serialization and parsing of the TH-D75's serial
//! command protocol. Commands are ASCII text terminated by carriage return
//! (`\r`), with parameters separated by commas. The protocol layer has no
//! async or I/O dependencies — it operates purely on byte slices.
//!
//! All 55 CAT commands (53 from the firmware dispatch table, plus 2 extra
//! mnemonics TY and 0E) are represented as variants of [`Command`] (outgoing) and [`Response`]
//! (incoming). Use [`serialize`] and [`parse`] to convert between typed
//! representations and wire format.

pub mod aprs;
pub mod bluetooth;
pub mod codec;
pub mod control;
pub mod core;
pub mod dstar;
pub mod gps;
pub mod memory;
pub mod programming;
pub mod scan;
pub mod sd;
pub mod service;
pub mod tone;
pub mod user;
pub mod vfo;

pub use codec::Codec;

use crate::error::ProtocolError;
#[expect(
    unused_imports,
    reason = "Broad re-export of every D75 type the protocol layer constructs. Some variants \
              only appear behind `#[cfg(test)]` paths or in docstrings; the umbrella import \
              keeps the `use` block consistent with the enum variants regardless of which \
              features/targets are enabled."
)]
use crate::types::{
    AfGainLevel, Band, BeaconMode, CallsignSlot, ChannelMemory, DetectOutputMode, DstarSlot,
    DvGatewayMode, FilterMode, FilterWidthIndex, FineStep, GpsRadioMode, KeyLockType, Mode,
    PowerLevel, SMeterReading, ScanResumeMethod, SquelchLevel, StepSize, TncBaud, TncMode,
    ToneCode, VfoMemoryMode, VoxDelay, VoxGain,
};

/// A CAT command to send to the radio.
#[derive(Debug, Clone)]
pub enum Command {
    // === Core (FQ, FO, FV, PS, ID, PC, BC, VM, FR) ===
    /// Get frequency (FQ read).
    ///
    /// Returns the current frequency data for the target band.
    /// Works in any mode.
    GetFrequency {
        /// Target band.
        band: Band,
    },
    /// Set frequency (FQ write) -- takes full channel data.
    ///
    /// # Hardware note
    ///
    /// FQ write may be rejected by the TH-D75 (returns `?`). Prefer
    /// [`SetFrequencyFull`](Command::SetFrequencyFull) (FO write) for
    /// reliable VFO frequency changes, or use
    /// [`Radio::tune_frequency`](crate::radio::Radio::tune_frequency)
    /// which handles mode switching and uses FO write internally.
    ///
    /// # Mode requirement
    /// Radio must be in VFO mode on the target band.
    SetFrequency {
        /// Target band.
        band: Band,
        /// Channel memory data.
        channel: ChannelMemory,
    },
    /// Get full frequency and settings (FO read).
    ///
    /// Returns full channel data including tone, shift, and step settings.
    /// Works in any mode.
    GetFrequencyFull {
        /// Target band.
        band: Band,
    },
    /// Set full frequency and settings (FO write).
    ///
    /// # Mode requirement
    /// Radio must be in VFO mode on the target band.
    /// Use [`Radio::tune_frequency`](crate::radio::Radio::tune_frequency)
    /// for automatic mode handling.
    SetFrequencyFull {
        /// Target band.
        band: Band,
        /// Channel memory data.
        channel: ChannelMemory,
    },
    /// Get firmware version (FV).
    GetFirmwareVersion,
    /// Set firmware version (FV write) -- factory programming command.
    ///
    /// Wire format: `FV version\r`.
    ///
    /// # Safety
    ///
    /// **DANGEROUS FACTORY COMMAND.** This is intended for factory programming
    /// only. Writing an incorrect firmware version string may brick the radio,
    /// cause firmware validation failures, or void your warranty. **Do not use
    /// unless you fully understand the consequences.**
    SetFirmwareVersion {
        /// Firmware version string to write.
        version: String,
    },
    /// Get power on/off status (PS read).
    GetPowerStatus,
    /// Get radio model ID (ID).
    GetRadioId,
    /// Set radio model ID (ID write) -- factory programming command.
    ///
    /// Wire format: `ID model\r`.
    ///
    /// # Safety
    ///
    /// **DANGEROUS FACTORY COMMAND.** This is intended for factory programming
    /// only. Writing an incorrect model ID may cause the radio to behave as a
    /// different model, disable features, or brick the device. **Do not use
    /// unless you fully understand the consequences.**
    SetRadioId {
        /// Model identification string to write.
        model: String,
    },
    /// Get beep setting (BE read).
    ///
    /// D75 RE: `BE x` (x: 0=off, 1=on).
    /// Sends bare `BE\r`.
    ///
    /// # Mode requirement
    /// Hardware-verified: returns `N` (not available) in certain modes.
    /// The beep setting may not be readable depending on the radio's
    /// current operating state.
    GetBeep,
    /// Set beep on/off (BE write).
    ///
    /// D75 RE: `BE x` (x: 0=off, 1=on).
    /// Sends `BE 0\r` or `BE 1\r`.
    ///
    /// # D75 firmware note
    ///
    /// The D75 `cat_be_handler` is a stub that always returns `?` for writes.
    /// The beep setting can only be changed via MCP programming mode (direct
    /// memory writes) or the radio's menu. The read (`BE\r`) works normally.
    /// The handler copies a model string on bare read but immediately calls
    /// `cat_send_error_response()` for all other lengths.
    SetBeep {
        /// Whether key beep is enabled.
        enabled: bool,
    },
    /// Get power level (PC read).
    GetPowerLevel {
        /// Target band.
        band: Band,
    },
    /// Set power level (PC write).
    SetPowerLevel {
        /// Target band.
        band: Band,
        /// Power level to set.
        level: PowerLevel,
    },
    /// Get the current active band (BC bare read).
    GetBand,
    /// Set the active band (BC write).
    ///
    /// # Warning
    /// This is an ACTION command that switches the radio's active band.
    SetBand {
        /// Target band.
        band: Band,
    },
    /// Get VFO/Memory mode (VM read).
    ///
    /// Mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    /// Works in any mode. The response is used to update the
    /// [`Radio`](crate::radio::Radio) cached mode state.
    GetVfoMemoryMode {
        /// Target band.
        band: Band,
    },
    /// Set VFO/Memory mode (VM write).
    ///
    /// Mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    /// This is an ACTION command that changes the radio's operating mode.
    /// Prefer the safe tuning methods which handle mode switching
    /// automatically.
    SetVfoMemoryMode {
        /// Target band.
        band: Band,
        /// VFO/Memory mode.
        mode: VfoMemoryMode,
    },
    /// Get FM radio on/off state (FR read).
    GetFmRadio,
    /// Set FM radio on/off state (FR write).
    SetFmRadio {
        /// Whether FM radio is enabled.
        enabled: bool,
    },

    // === VFO (AG, SQ, SM, MD, FS, FT, SH, UP, RA) ===
    /// Get AF gain level for a band (AG read).
    ///
    /// Per KI4LAX CAT reference: `AG` returns gain level 000-099.
    /// Hardware observation: bare `AG\r` returns global gain. Band-indexed
    /// `AG band\r` returns `?`. Read is bare only.
    GetAfGain,
    /// Set AF gain level (AG write).
    ///
    /// Per KI4LAX CAT reference: `AG AAA` (AAA: 000-099, 3-digit zero-padded).
    /// Sends bare `AG level\r` (no band parameter — firmware rejects band-indexed writes).
    ///
    /// **Important: the `band` field is ignored by the firmware.** The AG
    /// command on the TH-D75 is a global (non-band-specific) control.
    /// Attempting to send a band-indexed write (`AG band,level`) results
    /// in a `?` error response from the radio. The `band` field is
    /// retained in this variant solely for API symmetry with other
    /// band-indexed commands (e.g., [`Command::SetSquelch`],
    /// [`Command::SetMode`]) so that callers can use a uniform
    /// band+value pattern. The serializer discards it.
    SetAfGain {
        /// Target band. **Ignored by firmware** — AF gain is a global
        /// control on the TH-D75. This field exists for API symmetry
        /// with other band-indexed commands; the serializer discards it
        /// and sends a bare `AG level\r`.
        band: Band,
        /// Gain level (0-99).
        level: AfGainLevel,
    },
    /// Get squelch level (SQ read).
    GetSquelch {
        /// Target band.
        band: Band,
    },
    /// Set squelch level (SQ write).
    ///
    /// Per KI4LAX CAT reference: `SQ x,yy` (x: band, yy: squelch level 0-6).
    /// Sends `SQ band,level\r`.
    SetSquelch {
        /// Target band.
        band: Band,
        /// Squelch level (0-6 on D75).
        level: SquelchLevel,
    },
    /// Get S-meter reading (SM read).
    GetSmeter {
        /// Target band.
        band: Band,
    },
    /// Set S-meter value (SM write) -- calibration/test interface.
    ///
    /// Wire format: `SM band,level\r` (band 0-1, level is a hex nibble value).
    ///
    /// # Warning
    ///
    /// This is likely a calibration or test/debug interface. Setting the S-meter
    /// value directly may interfere with normal signal strength readings. The
    /// exact behavior and persistence of written values is undocumented.
    SetSmeter {
        /// Target band.
        band: Band,
        /// S-meter level value.
        level: SMeterReading,
    },
    /// Get operating mode (MD read).
    GetMode {
        /// Target band.
        band: Band,
    },
    /// Set operating mode (MD write).
    ///
    /// # Mode requirement
    /// Radio must be in VFO mode on the target band. Setting the
    /// operating mode (FM/DV/NFM/AM) only applies to the current VFO.
    SetMode {
        /// Target band.
        band: Band,
        /// Operating mode to set.
        mode: Mode,
    },
    /// Get fine step (FS bare read).
    ///
    /// Firmware-verified: FS = Fine Step. Bare `FS\r` returns `FS value`
    /// (single value, no band). Band-indexed reads are not supported.
    GetFineStep,
    /// Set fine step for a band (FS write).
    ///
    /// Firmware-verified: `FS band,step\r` (band 0-1, step 0-3).
    ///
    /// # Firmware bug (v1.03)
    ///
    /// FS write is broken on firmware 1.03 — the radio returns `N`
    /// (not available) for all write attempts.
    SetFineStep {
        /// Target band.
        band: Band,
        /// Fine step to set (0-3).
        step: FineStep,
    },
    /// Get function type (FT bare read, no band parameter).
    ///
    /// Sends `FT\r` (bare). The radio returns the current function type.
    GetFunctionType,
    /// Set fine tune on/off (FT write).
    ///
    /// Wire format: `FT band,value\r` (band 0-1, value 0=off, 1=on).
    ///
    /// Per Operating Tips section 5.10.6: Fine Tune only works with AM modulation
    /// and Band B. The write form takes a band parameter unlike the bare read.
    /// Set fine tune on/off (FT write).
    ///
    /// Wire format: `FT value\r` (bare, no band parameter per ARFC-D75 RE).
    /// ARFC sends `FT 0\r` (off) or `FT 1\r` (on).
    SetFunctionType {
        /// Whether fine tune is enabled.
        enabled: bool,
    },
    /// Get filter width by mode index (SH read).
    ///
    /// Per Operating Tips §5.10: SSB high-cut 2.2–3.0 kHz (Menu 120),
    /// CW bandwidth 0.3–2.0 kHz (Menu 121), AM high-cut 3.0–7.5 kHz
    /// (Menu 122). `mode_index`: 0 = SSB, 1 = CW, 2 = AM.
    GetFilterWidth {
        /// Receiver filter mode.
        mode: FilterMode,
    },
    /// Set filter width by mode index (SH write).
    ///
    /// Sets the IF receive filter width for the specified mode. The width
    /// value maps to the filter selection index for that mode (see
    /// [`GetFilterWidth`](Command::GetFilterWidth) for mode descriptions).
    SetFilterWidth {
        /// Receiver filter mode.
        mode: FilterMode,
        /// Filter width index (0-4 for SSB/CW, 0-3 for AM).
        width: FilterWidthIndex,
    },
    /// Step frequency up by one increment (UP action).
    ///
    /// # Mode requirement
    /// Radio should be in VFO mode for frequency stepping. In Memory mode,
    /// this steps through memory channels instead.
    ///
    /// # Warning
    /// This is an ACTION command that changes the radio's active frequency.
    /// There is no undo -- the previous frequency is not preserved.
    FrequencyUp {
        /// Target band.
        band: Band,
    },
    /// Tune frequency down by one step (DW action).
    ///
    /// Per KI4LAX CAT reference: DW tunes the current band's frequency
    /// down by the current step size. This is a write-only action command
    /// (like UP). The band parameter selects which band to step.
    FrequencyDown {
        /// Target band.
        band: Band,
    },
    /// Get attenuator state (RA read).
    GetAttenuator {
        /// Target band.
        band: Band,
    },
    /// Set attenuator on/off (RA write).
    SetAttenuator {
        /// Target band.
        band: Band,
        /// Whether attenuator is enabled.
        enabled: bool,
    },

    // === Control (AI, BY, DL, RX, TX, LC, IO, BL, VD, VG, VX) ===
    /// Set auto-info notification mode (AI write).
    ///
    /// The firmware accepts values 0-200 (not just 0/1).
    /// Values beyond 1 may control notification verbosity or
    /// filter settings. The exact semantics of values 2-200
    /// are undocumented.
    SetAutoInfo {
        /// Whether auto-info is enabled.
        enabled: bool,
    },
    /// Get busy state (BY read).
    GetBusy {
        /// Target band.
        band: Band,
    },
    /// Set busy/squelch state (BY write) -- test/debug interface.
    ///
    /// Wire format: `BY band,state\r` (band 0-1, state 0=not busy, 1=busy).
    ///
    /// # Warning
    ///
    /// This is likely a test or debug interface. Setting the busy state directly
    /// may interfere with normal squelch operation. Use with caution.
    SetBusy {
        /// Target band.
        band: Band,
        /// Whether the channel is busy (squelch open).
        busy: bool,
    },
    /// Get dual-band mode (DL read).
    GetDualBand,
    /// Set dual-band mode (DL write).
    SetDualBand {
        /// Whether dual-band is enabled.
        enabled: bool,
    },
    /// Switch to receive mode (RX action).
    Receive {
        /// Target band.
        band: Band,
    },
    /// Key the transmitter (TX action).
    ///
    /// # Safety
    /// **This transmits on air.** Ensure you are authorized to transmit on
    /// the current frequency, have proper identification, and comply with
    /// all applicable regulations. Use [`Command::Receive`] to return to receive mode.
    Transmit {
        /// Target band.
        band: Band,
    },
    /// Get lock/control settings (LC read).
    ///
    /// Returns the primary lock state as a boolean. For reading all
    /// lock fields, use MCP memory offsets 0x1060–0x1065.
    GetLock,
    /// Set lock/control state — simple boolean form (LC write).
    ///
    /// Sends `LC 0` or `LC 1`. The `locked` field uses **wire semantics**:
    /// on the D75 the wire value is inverted (`true` on the wire means
    /// *unlocked*). The high-level `Radio::set_lock()` method handles
    /// this inversion so callers can pass logical lock state.
    ///
    /// For full lock configuration, use
    /// [`SetLockFull`](Command::SetLockFull).
    SetLock {
        /// Whether key lock is engaged (wire semantics — inverted on D75).
        locked: bool,
    },
    /// Set all lock/control fields (LC 6-field write).
    ///
    /// Sends `LC a,b,c,d,e,f` where each field is a control flag:
    /// - `a`: Key lock (0=off, 1=on) — MCP 0x1060
    /// - `b`: Key lock type (0=key, 1=PTT, 2=key+PTT) — MCP 0x1061
    /// - `c`: Lock key A (0=off, 1=on) — MCP 0x1062
    /// - `d`: Lock key B (0=off, 1=on) — MCP 0x1063
    /// - `e`: Lock key C (0=off, 1=on) — MCP 0x1064
    /// - `f`: Lock PTT (0=off, 1=on) — MCP 0x1065
    SetLockFull {
        /// Key lock enabled.
        locked: bool,
        /// Key lock type (key-only, key+PTT, key+PTT+dial).
        lock_type: KeyLockType,
        /// Lock key A.
        lock_a: bool,
        /// Lock key B.
        lock_b: bool,
        /// Lock key C.
        lock_c: bool,
        /// Lock PTT.
        lock_ptt: bool,
    },
    /// Get AF/IF/Detect output mode (IO read).
    GetIoPort,
    /// Set AF/IF/Detect output mode (IO write).
    SetIoPort {
        /// Output mode (AF/IF/Detect).
        value: DetectOutputMode,
    },
    /// Get battery level (BL read).
    ///
    /// Per KI4LAX CAT reference: BL returns battery charge state.
    /// 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green),
    /// 4=Charging (observed on hardware when USB power is connected).
    GetBatteryLevel,
    /// Set battery level display (BL write).
    ///
    /// Wire format: `BL display,level\r` (7 bytes with comma).
    ///
    /// # Warning
    ///
    /// The exact purpose of this command is unclear. It may control the battery
    /// display indicator or be a calibration/test interface. The `display` and
    /// `level` parameter semantics are undocumented.
    SetBatteryLevel {
        /// Display parameter (semantics unknown).
        display: u8,
        /// Level parameter (semantics unknown).
        level: u8,
    },
    /// Get VOX delay (VD read).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VD read/write to succeed.
    /// Returns `N` (not available) when VOX is off.
    GetVoxDelay,
    /// Set VOX delay (VD write).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VD writes to succeed.
    /// Returns `N` (not available) when VOX is off. Enable VOX first
    /// with [`SetVox`](Command::SetVox), then set the delay, then
    /// optionally disable VOX again.
    SetVoxDelay {
        /// VOX delay (0-30, in 100ms units).
        delay: VoxDelay,
    },
    /// Get VOX gain (VG read).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VG read/write to succeed.
    /// Returns `N` (not available) when VOX is off.
    GetVoxGain,
    /// Set VOX gain (VG write).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VG writes to succeed.
    /// Returns `N` (not available) when VOX is off.
    SetVoxGain {
        /// VOX gain (0-9).
        gain: VoxGain,
    },
    /// Get VOX state (VX read).
    GetVox,
    /// Set VOX on/off (VX write).
    SetVox {
        /// Whether VOX is enabled.
        enabled: bool,
    },

    // === Memory (ME, MR, 0M) ===
    /// Get the current memory channel number for a band (MR read).
    ///
    /// Hardware-verified: `MR band\r` returns `MR bandCCC` where CCC is
    /// the 3-digit channel number (no comma separator in the response).
    /// Example: `MR 0\r` returns `MR 021` meaning band A, channel 21.
    ///
    /// This is a READ that queries which channel is active, not an action.
    GetCurrentChannel {
        /// Target band.
        band: Band,
    },
    /// Get memory channel data (ME read).
    GetMemoryChannel {
        /// Channel number.
        channel: u16,
    },
    /// Set memory channel data (ME write).
    SetMemoryChannel {
        /// Channel number.
        channel: u16,
        /// Channel memory data.
        data: ChannelMemory,
    },
    /// Recall memory channel — switches the radio's active channel (MR write).
    ///
    /// # Mode requirement
    /// Radio must be in Memory mode on the target band.
    /// Use [`Radio::tune_channel`](crate::radio::Radio::tune_channel)
    /// for automatic mode handling.
    ///
    /// # Warning
    /// This is an ACTION command that changes the radio's active channel.
    /// Format is `MR band,channel`. Despite the "get" in the name of the
    /// Kenwood documentation, this command changes radio state.
    RecallMemoryChannel {
        /// Target band.
        band: Band,
        /// Channel number (3-digit, 000-999).
        channel: u16,
    },
    /// Enter MCP programming mode (0M action).
    ///
    /// # Safety
    /// **DANGEROUS:** This puts the radio into programming mode where it
    /// stops responding to normal CAT commands. The radio must be manually
    /// restarted to recover. Do not use unless implementing a full MCP
    /// programming interface.
    EnterProgrammingMode,

    // === TNC / D-STAR / Clock (TN, DC, RT) ===
    /// Get TNC mode (TN bare read).
    ///
    /// Hardware-verified: bare `TN\r` returns `TN mode,setting`.
    /// Band-indexed `TN band\r` returns `?` (rejected).
    ///
    /// The D75 RE misidentified this as CTCSS tone. On hardware, TN
    /// returns TNC mode data (e.g., `TN 0,0`).
    ///
    /// Valid mode values per firmware validation: 0, 1, 2, 3.
    /// Mode 3 may correspond to MMDVM or Reflector Terminal mode.
    GetTncMode,
    /// Set TNC mode (TN write).
    ///
    /// Wire format: `TN mode,setting\r`.
    ///
    /// Valid mode values per firmware validation: 0, 1, 2, 3.
    /// Mode 3 may correspond to MMDVM or Reflector Terminal mode.
    SetTncMode {
        /// TNC operating mode (APRS/NAVITRA/KISS/MMDVM).
        mode: TncMode,
        /// TNC data speed setting.
        setting: TncBaud,
    },
    /// Get D-STAR callsign data for a slot (DC read).
    ///
    /// Hardware-verified: `DC slot\r` where slot is 1-6.
    /// `DC 0` returns `N` (not available). Slots 1-6 return callsign
    /// data in format `DC slot,callsign,suffix`.
    ///
    /// The D75 RE misidentified this as DCS code. On hardware, DC
    /// is the D-STAR callsign command.
    GetDstarCallsign {
        /// Callsign slot (1-6). Slot 0 returns `N`.
        slot: DstarSlot,
    },
    /// Set D-STAR callsign for a slot (DC write).
    ///
    /// Wire format: `DC slot,callsign,suffix\r` where slot is 1-6,
    /// callsign is 8 characters (space-padded), and suffix is up to
    /// 4 characters.
    SetDstarCallsign {
        /// Callsign slot (1-6).
        slot: DstarSlot,
        /// Callsign string (8 chars, space-padded).
        callsign: String,
        /// Callsign suffix (up to 4 chars).
        suffix: String,
    },
    /// Get real-time clock (RT bare read).
    ///
    /// Hardware-verified: bare `RT\r` returns `RT YYMMDDHHmmss`.
    /// Band-indexed `RT band\r` returns `?` (rejected).
    ///
    /// The D75 RE misidentified this as repeater tone. On hardware, RT
    /// returns the radio's real-time clock.
    GetRealTimeClock,

    // === Scan (SR, SF, BS) ===
    /// Set scan resume mode (SR write-only).
    ///
    /// Hardware-verified: bare `SR\r` returns `?` (no read form).
    /// SR is write-only. Sends `SR mode\r`.
    ///
    /// # Safety warning
    /// On hardware, `SR 0` was observed to reboot the radio. The D75 RE
    /// identifies this as scan resume, but the behavior may coincide with
    /// a reset action. Use with caution.
    SetScanResume {
        /// Scan resume method.
        mode: ScanResumeMethod,
    },
    /// Get step size for a band (SF read, band-indexed).
    ///
    /// Firmware-verified: SF = Step Size. `SF band\r` returns `SF band,step`.
    /// Both `SF 0` and `SF 1` confirmed working.
    GetStepSize {
        /// Target band.
        band: Band,
    },
    /// Set step size for a band (SF write).
    ///
    /// Firmware-verified: `SF band,step\r` (band 0-1, step index 0-11).
    SetStepSize {
        /// Target band.
        band: Band,
        /// Step size to set (0-11).
        step: StepSize,
    },
    /// Get band scope data (BS read).
    GetBandScope {
        /// Target band.
        band: Band,
    },
    /// Set band scope configuration (BS write).
    ///
    /// Wire format: `BS band,value\r` (band 0-1, value meaning unknown).
    SetBandScope {
        /// Target band.
        band: Band,
        /// Band scope value (semantics unknown).
        value: u8,
    },

    // === APRS (AS, PT, MS) ===
    /// Get TNC baud rate (AS read).
    ///
    /// Returns 0 = 1200 baud, 1 = 9600 baud.
    GetTncBaud,
    /// Set TNC baud rate (AS write).
    ///
    /// Values: 0 = 1200 baud, 1 = 9600 baud.
    SetTncBaud {
        /// Baud rate.
        rate: TncBaud,
    },

    // === Serial Info (AE) ===
    /// Get serial number and model code (AE read).
    ///
    /// Despite the AE mnemonic (historically "APRS Extended"), this command
    /// returns the radio's serial number and model code.
    GetSerialInfo,
    /// Get beacon TX control mode (PT read).
    GetBeaconType,
    /// Set beacon TX control mode (PT write).
    SetBeaconType {
        /// Beacon transmission mode.
        mode: BeaconMode,
    },
    /// Get APRS position source (MS read).
    GetPositionSource,
    /// Send message (MS write).
    SendMessage {
        /// Message text to send.
        text: String,
    },

    // === D-STAR (DS, CS, GW) ===
    /// Get active D-STAR callsign slot (DS read).
    GetDstarSlot,
    /// Set active D-STAR callsign slot (DS write).
    SetDstarSlot {
        /// D-STAR memory slot (1-6).
        slot: DstarSlot,
    },
    /// Get the active callsign slot number (CS bare read).
    ///
    /// CS returns a slot number (0-10), NOT the callsign text itself.
    /// The actual callsign text is read via DC (D-STAR callsign) slots 1-6.
    GetActiveCallsignSlot,
    /// Set the active callsign slot (CS write).
    ///
    /// Selects which callsign slot is active. Format: `CS N` where N is
    /// the slot number. The callsign text itself is read via DC slots.
    SetActiveCallsignSlot {
        /// Callsign slot to select (0-10).
        slot: CallsignSlot,
    },
    /// Get gateway (GW read).
    GetGateway,
    /// Set DV Gateway mode (GW write).
    SetGateway {
        /// DV Gateway mode (Off or Reflector Terminal).
        value: DvGatewayMode,
    },

    // === GPS (GP, GM, GS) ===
    /// Get GPS configuration (GP read).
    ///
    /// Returns GPS enabled and PC output enabled flags.
    GetGpsConfig,
    /// Set GPS configuration (GP write).
    ///
    /// Sets GPS enabled and PC output enabled flags.
    SetGpsConfig {
        /// Whether GPS is enabled.
        gps_enabled: bool,
        /// Whether GPS PC output is enabled.
        pc_output: bool,
    },
    /// Get GPS/Radio mode status (GM bare read).
    ///
    /// # Warning
    /// Only use bare `GM\r` (no parameter). Sending `GM 1\r` **reboots the
    /// radio** into GPS-only mode. This command only supports the bare read.
    GetGpsMode,
    /// Get GPS NMEA sentence enable flags (GS read).
    ///
    /// Returns 6 boolean flags for GGA, GLL, GSA, GSV, RMC, VTG.
    GetGpsSentences,
    /// Set GPS NMEA sentence enable flags (GS write).
    ///
    /// Sets 6 boolean flags for GGA, GLL, GSA, GSV, RMC, VTG.
    SetGpsSentences {
        /// GGA (Global Positioning System Fix Data) enabled.
        gga: bool,
        /// GLL (Geographic Position - Latitude/Longitude) enabled.
        gll: bool,
        /// GSA (GNSS DOP and Active Satellites) enabled.
        gsa: bool,
        /// GSV (GNSS Satellites in View) enabled.
        gsv: bool,
        /// RMC (Recommended Minimum Navigation Information) enabled.
        rmc: bool,
        /// VTG (Course Over Ground and Ground Speed) enabled.
        vtg: bool,
    },

    // === Bluetooth (BT) ===
    /// Get Bluetooth state (BT read).
    GetBluetooth,
    /// Set Bluetooth on/off (BT write).
    SetBluetooth {
        /// Whether Bluetooth is enabled.
        enabled: bool,
    },

    // === SD (SD) ===
    /// Query SD card / programming interface status (SD read).
    ///
    /// Note: The firmware's SD handler primarily checks for `SD PROGRAM`
    /// to enter MCP programming mode. The bare `SD` read response (`SD 0/1`)
    /// appears to indicate programming interface readiness, not SD card
    /// presence. Do NOT send `SD PROGRAM` — it enters programming mode
    /// and the radio stops responding to normal CAT commands.
    GetSdCard,

    // === User (US) ===
    /// Get user settings (US read).
    ///
    /// # Hardware note
    ///
    /// US returns `?` on all tested formats on the TH-D75 and may not be
    /// implemented. Both bare `US` and indexed `US NN` formats were rejected
    /// during gap probe testing with firmware 1.03.
    GetUserSettings,

    // === Extra (TY, 0E) ===
    /// Get radio type/region code (TY read).
    ///
    /// Not in the firmware's 53-command dispatch table — likely processed
    /// by a separate code path. Returns a region string and variant number
    /// (e.g., `TY K,2` for US region, variant 2).
    GetRadioType,
    /// Get MCP status (0E read).
    ///
    /// Returns `N` (not available) in normal operating mode. This mnemonic
    /// appears to be MCP-related. Its full behavior is unknown.
    GetMcpStatus,

    // === Service Mode (factory calibration/test — requires `0G KENWOOD` first) ===
    /// Enter factory service mode (0G write).
    ///
    /// Wire format: `0G KENWOOD\r`. The radio validates the "KENWOOD"
    /// password and switches from the standard 53-command CAT table to
    /// the 34-entry service mode table. Normal commands will not work
    /// until service mode is exited with [`ExitServiceMode`](Command::ExitServiceMode).
    ///
    /// Discovered via Ghidra RE of TH-D75 V1.03 firmware at 0xC006F464.
    EnterServiceMode,

    /// Exit factory service mode (0G bare).
    ///
    /// Wire format: `0G\r`. Exits service mode and restores the standard
    /// CAT command table. The 0G handler accepts both the bare form (exit)
    /// and the `0G KENWOOD` form (entry).
    ///
    /// Discovered via Ghidra RE of TH-D75 V1.03 firmware at 0xC006F464.
    ExitServiceMode,

    /// Read factory calibration data (0S read).
    ///
    /// Wire format: `0S\r`. Reads 200 bytes of factory calibration data
    /// (118 bytes from 0x4E000 + 82 bytes from a second address).
    /// Response is hex-encoded calibration data.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F508.
    ReadCalibrationData,

    /// Write factory calibration data (0R write).
    ///
    /// Wire format: `0R data\r` where data is 400 hex characters encoding
    /// 200 bytes. Total wire length is 404 bytes (2 mnemonic + 1 space +
    /// 400 hex + 1 CR). Writes to the same addresses as 0S.
    ///
    /// # Safety
    ///
    /// **CRITICAL: Can corrupt factory calibration.** Incorrect data will
    /// desynchronize RF calibration tables. Recovery may require professional
    /// recalibration with test equipment. Always read calibration first (0S)
    /// and keep a backup before writing.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F546.
    WriteCalibrationData {
        /// 400 hex characters (200 bytes of calibration data).
        data: String,
    },

    /// Get service/MCP status (0E service mode read).
    ///
    /// Wire format: `0E\r`. Reads 3 bytes from address 0x110 (hardware
    /// status register). In service mode, 0E returns actual status data
    /// rather than `N` (not available) as in normal mode.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F4B0.
    GetServiceStatus,

    /// Service calibration parameter read/write (1A).
    ///
    /// Wire format: `1A\r` (read, 3 bytes). Delegates to the firmware's
    /// command executor for calibration parameter access.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F5D0.
    ServiceCalibrate1A,

    /// Service calibration parameter read/write (1D).
    ///
    /// Wire format: `1D\r` (read, 3 bytes). Same executor-based pattern
    /// as 1A.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F5E4.
    ServiceCalibrate1D,

    /// Service calibration parameter read/write (1E).
    ///
    /// Wire format: `1E\r` (read, 3 bytes) or `1E XXX\r` (write, 6 bytes
    /// = 2 mnemonic + 1 space + 3 value). The firmware accepts both forms.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F5F8.
    ServiceCalibrate1E {
        /// Optional 3-character value for write. `None` for read.
        value: Option<String>,
    },

    /// Service calibration parameter read/write (1N).
    ///
    /// Wire format: `1N\r` (read, 3 bytes). Same executor-based pattern
    /// as 1A.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F6D0.
    ServiceCalibrate1N,

    /// Service calibration parameter read/write (1V).
    ///
    /// Wire format: `1V\r` (read, 3 bytes) or `1V XXX\r` (write, 6 bytes).
    /// Same dual-mode pattern as 1E.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F740.
    ServiceCalibrate1V {
        /// Optional 3-character value for write. `None` for read.
        value: Option<String>,
    },

    /// Service calibration single parameter write (1W).
    ///
    /// Wire format: `1W X\r` (write only, 5 bytes total).
    /// Single-character parameter, likely a mode or flag toggle.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F766.
    ServiceCalibrate1W {
        /// Single-character parameter value.
        value: String,
    },

    /// Write factory callsign/serial number (1I write).
    ///
    /// Wire format: `1I XXXXXXXX,YYY\r` (16 bytes total = 2 mnemonic +
    /// 1 space + 8 hex chars + 1 comma + 3 hex chars + 1 CR). The firmware
    /// validates all characters are alphanumeric (0-9, A-Z, a-z).
    ///
    /// # Safety
    ///
    /// **HIGH RISK: Changes the radio's factory serial number / callsign.**
    /// This may void the warranty and could cause regulatory issues.
    /// The original values should be backed up before any modification.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F61E.
    ServiceWriteId {
        /// 8-character hex identifier (factory callsign part).
        id: String,
        /// 3-character hex code.
        code: String,
    },

    /// Raw flash memory read/write (1F).
    ///
    /// Wire format for write: `1F AAAAAA,data\r` where AAAAAA is a 6-digit
    /// hex address (max 0x4FFFF) and data is hex-encoded bytes. The firmware
    /// validates that address + length does not exceed 0x50000. Read form
    /// is context-dependent on the executor.
    ///
    /// # Safety
    ///
    /// **CRITICAL: Can brick the radio.** Raw flash writes can overwrite
    /// boot code, calibration data, or firmware. There is no recovery
    /// mechanism short of JTAG or factory repair. Never write to flash
    /// addresses without understanding the memory map.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F780.
    ServiceFlashRead,

    /// Raw flash memory write (1F write).
    ///
    /// Wire format: `1F AAAAAA,data\r` where AAAAAA is 6-digit hex address
    /// and data is hex-encoded bytes.
    ///
    /// # Safety
    ///
    /// **CRITICAL: Can brick the radio.** See [`ServiceFlashRead`](Command::ServiceFlashRead).
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F780.
    ServiceFlashWrite {
        /// 6-digit hex flash address (0x000000 to 0x04FFFF).
        address: String,
        /// Hex-encoded data bytes to write.
        data: String,
    },

    /// Generic write via executor (0W).
    ///
    /// Wire format: `0W\r` (3 bytes). Delegates to the firmware's command
    /// executor. The exact write operation depends on the executor's
    /// internal state.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F5BC.
    ServiceWriteConfig,

    /// Service mode band selection (0Y).
    ///
    /// Wire format: `0Y band\r` (5 bytes total). Band is 0 or 1.
    /// Band 0 calls `radio_caller_06ef1c()`, band 1 calls
    /// `ipc_caller_06eef6()` — different code paths for the two
    /// receiver chains.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F4D0.
    ServiceBandSelect {
        /// Band number (0 or 1).
        band: u8,
    },

    /// Bulk EEPROM/calibration data export (9E read).
    ///
    /// Wire format: `9E AAAAAA,LL\r` (13 bytes = 2 mnemonic + 1 space +
    /// 6-digit hex address + 1 comma + 2-digit hex length + 1 CR).
    /// Reads up to 256 bytes from the specified address. Length 0 means 256.
    /// Address + length must not exceed 0x50000.
    ///
    /// Response is 128-byte formatted hex data.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F826.
    ServiceReadEeprom {
        /// 6-digit hex address (max 0x4FFFF).
        address: String,
        /// 2-digit hex length (00 = 256 bytes).
        length: String,
    },

    /// Targeted calibration read at specific offset (9R read).
    ///
    /// Wire format: `9R\r` (3 bytes). Returns 4 bytes of formatted
    /// calibration data at the current offset. The offset is determined
    /// by internal firmware state.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F8CA.
    ServiceReadEepromAddr,

    /// Get internal version/variant information (2V).
    ///
    /// Wire format: `2V XX,YYY\r` (10 bytes = 2 mnemonic + 1 space +
    /// 2-digit hex param + 1 comma + 3-digit hex param + 1 CR).
    /// Returns internal model code (e.g., EX-5210), build date, hardware
    /// revision, and calibration date.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F910.
    ServiceGetVersion {
        /// 2-digit hex parameter.
        param1: String,
        /// 3-digit hex parameter.
        param2: String,
    },

    /// Get hardware register / GPIO status (1G read).
    ///
    /// Wire format: `1G\r` (3 bytes). Returns hex-encoded hardware
    /// register values with a comma separator. Used for factory testing
    /// of GPIO and peripheral status.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F9A8.
    ServiceGetHardware,

    /// New calibration command in D75 (1C write).
    ///
    /// Wire format: `1C XXX\r` (7 bytes = 2 mnemonic + 1 space + 3-digit
    /// hex value + 1 CR). Value must be less than 0x100 (256). Not present
    /// in the D74 firmware — likely related to the 220 MHz band (D75A)
    /// or enhanced DSP.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006FA08.
    ServiceCalibrateNew {
        /// 3-digit hex value (0x000 to 0x0FF).
        value: String,
    },

    /// Dynamic-length hardware configuration (1U).
    ///
    /// Wire format: `1U\r` (read, 3 bytes) or `1U data\r` (write, dynamic
    /// length determined by reading a hardware register). The firmware
    /// calls `os_disable_interrupts()` in the error path — this is a
    /// low-level hardware configuration command.
    ///
    /// Requires service mode (`0G KENWOOD` first).
    /// Discovered via Ghidra RE at 0xC006F6E4.
    ServiceDynamicParam {
        /// Optional data for write. `None` for read.
        data: Option<String>,
    },
}

/// A parsed response from the radio.
#[derive(Debug, Clone)]
pub enum Response {
    // === Core ===
    /// Frequency response (FQ).
    Frequency {
        /// Band the frequency is on.
        band: Band,
        /// Channel memory data.
        channel: ChannelMemory,
    },
    /// Full frequency and settings response (FO).
    FrequencyFull {
        /// Band the data is for.
        band: Band,
        /// Channel memory data.
        channel: ChannelMemory,
    },
    /// Firmware version response (FV).
    FirmwareVersion {
        /// Version string.
        version: String,
    },
    /// Power status response (PS).
    PowerStatus {
        /// Whether the radio is on.
        on: bool,
    },
    /// Radio model ID response (ID).
    RadioId {
        /// Model identification string.
        model: String,
    },
    /// Power level response (PC).
    PowerLevel {
        /// Band the level is for.
        band: Band,
        /// Current power level.
        level: PowerLevel,
    },
    /// Band response (BC read).
    BandResponse {
        /// Current active band.
        band: Band,
    },
    /// VFO/Memory mode response (VM).
    ///
    /// Mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    VfoMemoryMode {
        /// Band the mode is for.
        band: Band,
        /// VFO/Memory mode.
        mode: VfoMemoryMode,
    },
    /// FM radio on/off response (FR).
    FmRadio {
        /// Whether FM radio is enabled.
        enabled: bool,
    },

    // === VFO ===
    /// AF gain response (AG).
    ///
    /// Per KI4LAX CAT reference: gain range 000-099.
    AfGain {
        /// Gain level (0-99). Global, not per-band.
        level: AfGainLevel,
    },
    /// Squelch level response (SQ).
    Squelch {
        /// Band the squelch is for.
        band: Band,
        /// Squelch level (0-6).
        level: SquelchLevel,
    },
    /// S-meter reading response (SM).
    Smeter {
        /// Band the reading is for.
        band: Band,
        /// S-meter level (0-5).
        level: SMeterReading,
    },
    /// Operating mode response (MD).
    Mode {
        /// Band the mode is for.
        band: Band,
        /// Current operating mode.
        mode: Mode,
    },
    /// Fine step response (FS).
    ///
    /// Firmware-verified: bare `FS\r` returns `FS value` (single value, no band).
    FineStep {
        /// Current fine step setting.
        step: FineStep,
    },
    /// Function type response (FT).
    FunctionType {
        /// Fine tune enabled (0=off, 1=on).
        enabled: bool,
    },
    /// Filter width response (SH).
    FilterWidth {
        /// Receiver filter mode queried.
        mode: FilterMode,
        /// Filter width index (0-4 for SSB/CW, 0-3 for AM).
        width: FilterWidthIndex,
    },
    /// Attenuator state response (RA).
    Attenuator {
        /// Band the state is for.
        band: Band,
        /// Whether attenuator is enabled.
        enabled: bool,
    },

    // === Control ===
    /// Auto-info mode response (AI).
    AutoInfo {
        /// Whether auto-info is enabled.
        enabled: bool,
    },
    /// Busy state response (BY).
    Busy {
        /// Band the state is for.
        band: Band,
        /// Whether the channel is busy.
        busy: bool,
    },
    /// Dual-band mode response (DL).
    DualBand {
        /// Whether dual-band is enabled.
        enabled: bool,
    },
    /// Frequency down acknowledgement (DW).
    FrequencyDown,
    /// Beep setting response (BE).
    ///
    /// D75 RE: `BE x` (x: 0=off, 1=on).
    Beep {
        /// Whether key beep is enabled.
        enabled: bool,
    },
    /// Key lock state response (LC).
    ///
    /// The `locked` field uses **wire semantics**: on the D75 the wire
    /// value is inverted (`true` = unlocked on wire). `Radio::set_lock()`
    /// and `Radio::get_lock()` handle the inversion so callers see
    /// logical lock state.
    Lock {
        /// Whether key lock is engaged (wire semantics — inverted on D75).
        locked: bool,
    },
    /// AF/IF/Detect output mode response (IO).
    IoPort {
        /// Output mode.
        value: DetectOutputMode,
    },
    /// Battery level response (BL).
    ///
    /// 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green),
    /// 4=Charging (USB power connected).
    BatteryLevel {
        /// Battery charge level (0–4, where 4 = charging).
        level: crate::types::BatteryLevel,
    },
    /// VOX delay response (VD).
    VoxDelay {
        /// Current VOX delay (0-30, in 100ms units).
        delay: VoxDelay,
    },
    /// VOX gain response (VG).
    VoxGain {
        /// Current VOX gain (0-9).
        gain: VoxGain,
    },
    /// VOX state response (VX).
    Vox {
        /// Whether VOX is enabled.
        enabled: bool,
    },

    // === Memory ===
    /// Memory channel data response (ME).
    MemoryChannel {
        /// Channel number.
        channel: u16,
        /// Channel memory data.
        data: ChannelMemory,
    },
    /// Memory recall echo response (MR write acknowledgment).
    ///
    /// When `MR band,channel` is sent as a write/recall, the radio echoes
    /// back the band and channel as acknowledgment.
    MemoryRecall {
        /// Target band.
        band: Band,
        /// Channel number.
        channel: u16,
    },
    /// Current channel number response (MR read).
    ///
    /// Hardware-verified: `MR band\r` returns `MR bandCCC` (no comma).
    /// Example: `MR 0\r` returns `MR 021` = band A, channel 21.
    CurrentChannel {
        /// Band queried.
        band: Band,
        /// Current channel number on that band.
        channel: u16,
    },
    /// Programming mode acknowledgment (0M).
    ///
    /// The radio enters MCP programming mode and stops responding to
    /// normal CAT commands. This response should never actually be
    /// received in practice.
    ProgrammingMode,

    // === TNC / D-STAR / Clock ===
    /// TNC mode response (TN).
    ///
    /// Hardware-verified: bare `TN\r` returns `TN mode,setting`.
    /// Example: `TN 0,0`.
    ///
    /// Valid mode values per firmware validation: 0, 1, 2, 3.
    /// Mode 3 may correspond to MMDVM or Reflector Terminal mode.
    TncMode {
        /// TNC operating mode.
        mode: TncMode,
        /// TNC data speed setting.
        setting: TncBaud,
    },
    /// D-STAR callsign data response (DC).
    ///
    /// Hardware-verified: `DC slot\r` returns `DC slot,callsign,suffix`.
    /// Example: `DC 1,KQ4NIT  ,D75A`.
    DstarCallsign {
        /// Callsign slot (1-6).
        slot: DstarSlot,
        /// Callsign string (may be space-padded).
        callsign: String,
        /// Callsign suffix/module.
        suffix: String,
    },
    /// Real-time clock response (RT).
    ///
    /// Hardware-verified: bare `RT\r` returns `RT YYMMDDHHmmss`.
    /// Example: `RT 240104095700`.
    RealTimeClock {
        /// Raw datetime string in `YYMMDDHHmmss` format.
        datetime: String,
    },

    // === Scan ===
    /// Step size response (SF).
    ///
    /// Firmware-verified: SF = Step Size. Format: `band,step` where band is 0/1
    /// and step is the step size index (0-11).
    StepSize {
        /// Band the step is for.
        band: Band,
        /// Current step size.
        step: StepSize,
    },
    /// Band scope data response (BS).
    ///
    /// BS echoes the band number when queried.
    BandScope {
        /// Band the scope is for.
        band: Band,
    },

    // === APRS ===
    /// TNC baud rate response (AS).
    ///
    /// Values: 0 = 1200 baud, 1 = 9600 baud.
    TncBaud {
        /// Baud rate.
        rate: TncBaud,
    },
    /// Serial number and model code response (AE).
    ///
    /// Despite the AE mnemonic (historically "APRS Extended"), this command
    /// returns the radio's serial number and model code.
    /// Format: `serial,model_code` (e.g., `C3C10368,K01`).
    SerialInfo {
        /// Radio serial number.
        serial: String,
        /// Model code (e.g., "K01").
        model_code: String,
    },
    /// Beacon TX control mode response (PT).
    BeaconType {
        /// Beacon transmission mode.
        mode: BeaconMode,
    },
    /// APRS position source response (MS read).
    PositionSource {
        /// Position source index (0-based).
        source: u8,
    },

    // === D-STAR ===
    /// Active D-STAR callsign slot response (DS).
    DstarSlot {
        /// Active D-STAR memory slot (1-6).
        slot: DstarSlot,
    },
    /// Active callsign slot number response (CS).
    ///
    /// CS returns a slot number, NOT the callsign text. The actual callsign
    /// text is accessible via DC (D-STAR callsign) slots 1-6.
    ActiveCallsignSlot {
        /// Active callsign slot (0-10).
        slot: CallsignSlot,
    },
    /// DV Gateway mode response (GW).
    Gateway {
        /// DV Gateway mode.
        value: DvGatewayMode,
    },

    // === GPS ===
    /// GPS configuration response (GP).
    ///
    /// Two boolean fields: GPS enabled and PC output enabled.
    /// Format: `gps_enabled,pc_output` (e.g., `0,0`).
    GpsConfig {
        /// Whether GPS is enabled.
        gps_enabled: bool,
        /// Whether GPS PC output is enabled.
        pc_output: bool,
    },
    /// GPS/Radio mode status response (GM).
    ///
    /// 0 = Normal transceiver mode, 1 = GPS receiver mode.
    /// Firmware-verified: `cat_gm_handler` guard `local_18 < 2`.
    GpsMode {
        /// GPS/Radio operating mode.
        mode: GpsRadioMode,
    },
    /// GPS NMEA sentence enable flags response (GS).
    ///
    /// Six boolean fields controlling which NMEA sentences are output:
    /// GGA, GLL, GSA, GSV, RMC, VTG.
    GpsSentences {
        /// GGA (Global Positioning System Fix Data) enabled.
        gga: bool,
        /// GLL (Geographic Position - Latitude/Longitude) enabled.
        gll: bool,
        /// GSA (GNSS DOP and Active Satellites) enabled.
        gsa: bool,
        /// GSV (GNSS Satellites in View) enabled.
        gsv: bool,
        /// RMC (Recommended Minimum Navigation Information) enabled.
        rmc: bool,
        /// VTG (Course Over Ground and Ground Speed) enabled.
        vtg: bool,
    },

    // === Bluetooth ===
    /// Bluetooth state response (BT).
    Bluetooth {
        /// Whether Bluetooth is enabled.
        enabled: bool,
    },

    // === SD ===
    /// SD card / programming interface status response (SD).
    ///
    /// The firmware's SD handler primarily checks for `SD PROGRAM` to enter
    /// MCP programming mode. The bare `SD` read response (`SD 0/1`) appears
    /// to indicate programming interface readiness, not SD card presence.
    SdCard {
        /// Programming interface readiness flag.
        present: bool,
    },

    // === User ===
    /// User settings response (US).
    ///
    /// Note: US returns `?` on all tested formats on the TH-D75 and may not
    /// be implemented. This variant exists for completeness but may never be
    /// received from the radio.
    UserSettings {
        /// User settings value.
        value: u8,
    },

    // === Extra (TY, 0E) ===
    /// Radio type/region code response (TY).
    ///
    /// Returns the radio's region code and hardware variant.
    /// Example: `TY K,2` (K = US region, variant 2).
    RadioType {
        /// Region code string (e.g., "K" for US).
        region: String,
        /// Hardware variant number.
        variant: u8,
    },
    /// MCP status response (0E).
    ///
    /// Placeholder — always returns `N` (not available) in normal mode.
    McpStatus {
        /// Raw status value.
        value: String,
    },

    // === Service Mode ===
    /// Service mode entry/exit response (0G).
    ServiceMode {
        /// Raw response data.
        data: String,
    },
    /// Factory calibration data response (0S).
    ServiceCalibrationData {
        /// Hex-encoded 200-byte calibration data.
        data: String,
    },
    /// Calibration data write acknowledgment (0R).
    ServiceCalibrationWrite {
        /// Response data (typically echo).
        data: String,
    },
    /// Individual calibration parameter response (1A, 1D, 1E, 1N, 1V, 1W, 1C, 1U).
    ServiceCalibrationParam {
        /// The command mnemonic that generated this response.
        mnemonic: String,
        /// Raw response data.
        data: String,
    },
    /// Write config acknowledgment (0W).
    ServiceWriteConfig {
        /// Response data.
        data: String,
    },
    /// Band select response (0Y).
    ServiceBandSelect {
        /// Response data.
        data: String,
    },
    /// Factory ID write acknowledgment (1I).
    ServiceWriteId {
        /// Response data.
        data: String,
    },
    /// Flash read/write response (1F).
    ServiceFlash {
        /// Hex-encoded flash data or write acknowledgment.
        data: String,
    },
    /// EEPROM bulk data response (9E).
    ServiceEepromData {
        /// Hex-encoded EEPROM/calibration data.
        data: String,
    },
    /// EEPROM targeted read response (9R).
    ServiceEepromAddr {
        /// 4-byte formatted calibration data.
        data: String,
    },
    /// Internal version/variant response (2V).
    ServiceVersion {
        /// Version/variant information string.
        data: String,
    },
    /// Hardware register/GPIO status response (1G).
    ServiceHardware {
        /// Hex-encoded register values.
        data: String,
    },

    // === Special ===
    /// Write acknowledgment (radio echoes the command).
    Ok,
    /// Error response (`?\r`).
    Error,
    /// Not available response (`N\r`) — command not supported in current mode.
    NotAvailable,
}

/// Get the CAT mnemonic for a command (for logging).
#[must_use]
pub const fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::GetFrequency { .. } | Command::SetFrequency { .. } => "FQ",
        Command::GetFrequencyFull { .. } | Command::SetFrequencyFull { .. } => "FO",
        Command::GetFirmwareVersion | Command::SetFirmwareVersion { .. } => "FV",
        Command::GetPowerStatus => "PS",
        Command::GetRadioId | Command::SetRadioId { .. } => "ID",
        Command::GetBeep | Command::SetBeep { .. } => "BE",
        Command::GetPowerLevel { .. } | Command::SetPowerLevel { .. } => "PC",
        Command::GetBand | Command::SetBand { .. } => "BC",
        Command::GetVfoMemoryMode { .. } | Command::SetVfoMemoryMode { .. } => "VM",
        Command::GetFmRadio | Command::SetFmRadio { .. } => "FR",
        Command::GetAfGain | Command::SetAfGain { .. } => "AG",
        Command::GetSquelch { .. } | Command::SetSquelch { .. } => "SQ",
        Command::GetSmeter { .. } | Command::SetSmeter { .. } => "SM",
        Command::GetMode { .. } | Command::SetMode { .. } => "MD",
        Command::GetFineStep | Command::SetFineStep { .. } => "FS",
        Command::GetFunctionType | Command::SetFunctionType { .. } => "FT",
        Command::GetFilterWidth { .. } | Command::SetFilterWidth { .. } => "SH",
        Command::FrequencyUp { .. } => "UP",
        Command::FrequencyDown { .. } => "DW",
        Command::GetAttenuator { .. } | Command::SetAttenuator { .. } => "RA",
        Command::SetAutoInfo { .. } => "AI",
        Command::GetBusy { .. } | Command::SetBusy { .. } => "BY",
        Command::GetDualBand | Command::SetDualBand { .. } => "DL",
        Command::Receive { .. } => "RX",
        Command::Transmit { .. } => "TX",
        Command::GetLock | Command::SetLock { .. } | Command::SetLockFull { .. } => "LC",
        Command::GetIoPort | Command::SetIoPort { .. } => "IO",
        Command::GetBatteryLevel | Command::SetBatteryLevel { .. } => "BL",
        Command::GetVoxDelay | Command::SetVoxDelay { .. } => "VD",
        Command::GetVoxGain | Command::SetVoxGain { .. } => "VG",
        Command::GetVox | Command::SetVox { .. } => "VX",
        Command::GetCurrentChannel { .. } | Command::RecallMemoryChannel { .. } => "MR",
        Command::GetMemoryChannel { .. } | Command::SetMemoryChannel { .. } => "ME",
        Command::EnterProgrammingMode => "0M",
        Command::GetTncMode | Command::SetTncMode { .. } => "TN",
        Command::GetDstarCallsign { .. } | Command::SetDstarCallsign { .. } => "DC",
        Command::GetRealTimeClock => "RT",
        Command::SetScanResume { .. } => "SR",
        Command::GetStepSize { .. } | Command::SetStepSize { .. } => "SF",
        Command::GetBandScope { .. } | Command::SetBandScope { .. } => "BS",
        Command::GetTncBaud | Command::SetTncBaud { .. } => "AS",
        Command::GetSerialInfo => "AE",
        Command::GetBeaconType | Command::SetBeaconType { .. } => "PT",
        Command::GetPositionSource | Command::SendMessage { .. } => "MS",
        Command::GetDstarSlot | Command::SetDstarSlot { .. } => "DS",
        Command::GetActiveCallsignSlot | Command::SetActiveCallsignSlot { .. } => "CS",
        Command::GetGateway | Command::SetGateway { .. } => "GW",
        Command::GetGpsConfig | Command::SetGpsConfig { .. } => "GP",
        Command::GetGpsMode => "GM",
        Command::GetGpsSentences | Command::SetGpsSentences { .. } => "GS",
        Command::GetBluetooth | Command::SetBluetooth { .. } => "BT",
        Command::GetSdCard => "SD",
        Command::GetUserSettings => "US",
        Command::GetRadioType => "TY",
        Command::GetMcpStatus | Command::GetServiceStatus => "0E",
        // Service mode
        Command::EnterServiceMode | Command::ExitServiceMode => "0G",
        Command::ReadCalibrationData => "0S",
        Command::WriteCalibrationData { .. } => "0R",
        Command::ServiceCalibrate1A => "1A",
        Command::ServiceCalibrate1D => "1D",
        Command::ServiceCalibrate1E { .. } => "1E",
        Command::ServiceCalibrate1N => "1N",
        Command::ServiceCalibrate1V { .. } => "1V",
        Command::ServiceCalibrate1W { .. } => "1W",
        Command::ServiceWriteId { .. } => "1I",
        Command::ServiceFlashRead | Command::ServiceFlashWrite { .. } => "1F",
        Command::ServiceWriteConfig => "0W",
        Command::ServiceBandSelect { .. } => "0Y",
        Command::ServiceReadEeprom { .. } => "9E",
        Command::ServiceReadEepromAddr => "9R",
        Command::ServiceGetVersion { .. } => "2V",
        Command::ServiceGetHardware => "1G",
        Command::ServiceCalibrateNew { .. } => "1C",
        Command::ServiceDynamicParam { .. } => "1U",
    }
}

/// Serialize a command to wire format (bytes ending with `\r`).
///
/// Converts a [`Command`] into the byte sequence expected by the radio's
/// CAT protocol. Each serialized command ends with a carriage return.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Dispatch table over every CAT Command variant — one match arm per command is the \
              clearest mapping of the Command enum to its wire format. Splitting by submodule \
              would hide the complete command inventory; fall-through helpers already exist \
              (core::serialize_core_write, memory::serialize_memory_write, etc.)."
)]
pub fn serialize(cmd: &Command) -> Vec<u8> {
    let cmd_mnemonic = command_name(cmd);
    tracing::debug!(command = %cmd_mnemonic, "serializing command");

    // Try core write serialization first (FO write, FQ write)
    if let Some(body) = core::serialize_core_write(cmd) {
        let mut bytes = body.into_bytes();
        bytes.push(b'\r');
        tracing::trace!(wire = %String::from_utf8_lossy(&bytes), "serialized wire format");
        return bytes;
    }

    // Try memory write serialization (ME write)
    if let Some(body) = memory::serialize_memory_write(cmd) {
        let mut bytes = body.into_bytes();
        bytes.push(b'\r');
        tracing::trace!(wire = %String::from_utf8_lossy(&bytes), "serialized wire format");
        return bytes;
    }

    let body = match cmd {
        // Core
        Command::GetFrequency { band } => {
            format!("FQ {}", u8::from(*band))
        }
        Command::SetFrequency { .. } => {
            // Handled by serialize_core_write above; this arm is unreachable
            // but required for exhaustive matching.
            unreachable!("SetFrequency handled by core::serialize_core_write")
        }
        Command::GetFrequencyFull { band } => {
            format!("FO {}", u8::from(*band))
        }
        Command::SetFrequencyFull { .. } => {
            unreachable!("SetFrequencyFull handled by core::serialize_core_write")
        }
        Command::GetFirmwareVersion => "FV".to_owned(),
        Command::SetFirmwareVersion { version } => format!("FV {version}"),
        Command::GetPowerStatus => "PS".to_owned(),
        Command::GetRadioId => "ID".to_owned(),
        Command::SetRadioId { model } => format!("ID {model}"),
        Command::GetBeep => "BE".to_owned(),
        Command::SetBeep { enabled } => format!("BE {}", u8::from(*enabled)),
        Command::GetPowerLevel { band } => format!("PC {}", u8::from(*band)),
        Command::SetPowerLevel { band, level } => {
            format!("PC {},{}", u8::from(*band), u8::from(*level))
        }
        Command::GetBand => "BC".to_owned(),
        Command::SetBand { band } => format!("BC {}", u8::from(*band)),
        Command::GetVfoMemoryMode { band } => format!("VM {}", u8::from(*band)),
        Command::SetVfoMemoryMode { band, mode } => {
            format!("VM {},{}", u8::from(*band), u8::from(*mode))
        }
        Command::GetFmRadio => "FR".to_owned(),
        Command::SetFmRadio { enabled } => format!("FR {}", u8::from(*enabled)),

        // VFO
        Command::GetAfGain => "AG".to_owned(),
        Command::SetAfGain { band: _, level } => {
            // D75 firmware AG write handler expects bare `AG AAA\r`.
            // Band-indexed `AG band,level` is rejected with `?`.
            // Per KI4LAX: 3-digit zero-padded, range 000-099.
            format!("AG {:03}", level.as_u8())
        }
        Command::GetSquelch { band } => format!("SQ {}", u8::from(*band)),
        Command::SetSquelch { band, level } => {
            format!("SQ {},{}", u8::from(*band), level.as_u8())
        }
        Command::GetSmeter { band } => format!("SM {}", u8::from(*band)),
        Command::SetSmeter { band, level } => {
            format!("SM {},{}", u8::from(*band), level.as_u8())
        }
        Command::GetMode { band } => format!("MD {}", u8::from(*band)),
        Command::SetMode { band, mode } => {
            format!("MD {},{}", u8::from(*band), u8::from(*mode))
        }
        Command::GetFineStep => "FS".to_owned(),
        Command::SetFineStep { band, step } => {
            format!("FS {},{}", u8::from(*band), u8::from(*step))
        }
        Command::GetFunctionType => "FT".to_owned(),
        Command::SetFunctionType { enabled } => {
            format!("FT {}", u8::from(*enabled))
        }
        Command::GetFilterWidth { mode } => format!("SH {}", u8::from(*mode)),
        Command::SetFilterWidth { mode, width } => {
            format!("SH {},{}", u8::from(*mode), width.as_u8())
        }
        Command::FrequencyUp { band } => format!("UP {}", u8::from(*band)),
        Command::FrequencyDown { band } => format!("DW {}", u8::from(*band)),
        Command::GetAttenuator { band } => format!("RA {}", u8::from(*band)),
        Command::SetAttenuator { band, enabled } => {
            format!("RA {},{}", u8::from(*band), u8::from(*enabled))
        }

        // Control
        Command::SetAutoInfo { enabled } => format!("AI {}", u8::from(*enabled)),
        Command::GetBusy { band } => format!("BY {}", u8::from(*band)),
        Command::SetBusy { band, busy } => {
            format!("BY {},{}", u8::from(*band), u8::from(*busy))
        }
        Command::GetDualBand => "DL".to_owned(),
        Command::SetDualBand { enabled } => format!("DL {}", u8::from(*enabled)),
        Command::Receive { band } => format!("RX {}", u8::from(*band)),
        Command::Transmit { band } => format!("TX {}", u8::from(*band)),
        Command::GetLock => "LC".to_owned(),
        Command::SetLock { locked } => format!("LC {}", u8::from(*locked)),
        Command::SetLockFull {
            locked,
            lock_type,
            lock_a,
            lock_b,
            lock_c,
            lock_ptt,
        } => format!(
            "LC {},{},{},{},{},{}",
            u8::from(*locked),
            u8::from(*lock_type),
            u8::from(*lock_a),
            u8::from(*lock_b),
            u8::from(*lock_c),
            u8::from(*lock_ptt),
        ),
        Command::GetIoPort => "IO".to_owned(),
        Command::SetIoPort { value } => format!("IO {}", u8::from(*value)),
        Command::GetBatteryLevel => "BL".to_owned(),
        Command::SetBatteryLevel { display, level } => format!("BL {display},{level}"),
        Command::GetVoxDelay => "VD".to_owned(),
        Command::SetVoxDelay { delay } => format!("VD {}", delay.as_u8()),
        Command::GetVoxGain => "VG".to_owned(),
        Command::SetVoxGain { gain } => format!("VG {}", gain.as_u8()),
        Command::GetVox => "VX".to_owned(),
        Command::SetVox { enabled } => format!("VX {}", u8::from(*enabled)),

        // Memory
        Command::GetCurrentChannel { band } => {
            format!("MR {}", u8::from(*band))
        }
        Command::GetMemoryChannel { channel } => {
            format!("ME {channel:03}")
        }
        Command::SetMemoryChannel { .. } => {
            unreachable!("SetMemoryChannel handled by memory::serialize_memory_write")
        }
        Command::RecallMemoryChannel { band, channel } => {
            format!("MR {},{channel:03}", u8::from(*band))
        }
        Command::EnterProgrammingMode => "0M PROGRAM".to_owned(),

        // TNC / D-STAR / Clock
        Command::GetTncMode => "TN".to_owned(),
        Command::SetTncMode { mode, setting } => {
            format!("TN {},{}", u8::from(*mode), u8::from(*setting))
        }
        Command::GetDstarCallsign { slot } => format!("DC {}", slot.as_u8()),
        Command::SetDstarCallsign {
            slot,
            callsign,
            suffix,
        } => format!("DC {},{callsign},{suffix}", slot.as_u8()),
        Command::GetRealTimeClock => "RT".to_owned(),

        // Scan
        Command::SetScanResume { mode } => format!("SR {}", mode.to_raw()),
        Command::GetStepSize { band } => format!("SF {}", u8::from(*band)),
        Command::SetStepSize { band, step } => {
            format!("SF {},{:X}", u8::from(*band), u8::from(*step))
        }
        Command::GetBandScope { band } => format!("BS {}", u8::from(*band)),
        Command::SetBandScope { band, value } => {
            format!("BS {},{}", u8::from(*band), value)
        }

        // APRS
        Command::GetTncBaud => "AS".to_owned(),
        Command::SetTncBaud { rate } => format!("AS {}", u8::from(*rate)),
        Command::GetSerialInfo => "AE".to_owned(),
        Command::GetBeaconType => "PT".to_owned(),
        Command::SetBeaconType { mode } => format!("PT {}", u8::from(*mode)),
        Command::GetPositionSource => "MS".to_owned(),
        Command::SendMessage { text } => format!("MS {text}"),

        // D-STAR
        Command::GetDstarSlot => "DS".to_owned(),
        Command::SetDstarSlot { slot } => format!("DS {}", slot.as_u8()),
        Command::GetActiveCallsignSlot => "CS".to_owned(),
        Command::SetActiveCallsignSlot { slot } => format!("CS {}", slot.as_u8()),
        Command::GetGateway => "GW".to_owned(),
        Command::SetGateway { value } => format!("GW {}", u8::from(*value)),

        // GPS
        Command::GetGpsConfig => "GP".to_owned(),
        Command::SetGpsConfig {
            gps_enabled,
            pc_output,
        } => format!("GP {},{}", u8::from(*gps_enabled), u8::from(*pc_output)),
        Command::GetGpsMode => "GM".to_owned(),
        Command::GetGpsSentences => "GS".to_owned(),
        Command::SetGpsSentences {
            gga,
            gll,
            gsa,
            gsv,
            rmc,
            vtg,
        } => format!(
            "GS {},{},{},{},{},{}",
            u8::from(*gga),
            u8::from(*gll),
            u8::from(*gsa),
            u8::from(*gsv),
            u8::from(*rmc),
            u8::from(*vtg)
        ),

        // Bluetooth
        Command::GetBluetooth => "BT".to_owned(),
        Command::SetBluetooth { enabled } => format!("BT {}", u8::from(*enabled)),

        // SD
        Command::GetSdCard => "SD".to_owned(),

        // User
        Command::GetUserSettings => "US".to_owned(),

        // Extra
        Command::GetRadioType => "TY".to_owned(),
        Command::GetMcpStatus | Command::GetServiceStatus => "0E".to_owned(),

        // Service mode
        Command::EnterServiceMode => "0G KENWOOD".to_owned(),
        Command::ExitServiceMode => "0G".to_owned(),
        Command::ReadCalibrationData => "0S".to_owned(),
        Command::WriteCalibrationData { data } => format!("0R {data}"),
        Command::ServiceCalibrate1A => "1A".to_owned(),
        Command::ServiceCalibrate1D => "1D".to_owned(),
        Command::ServiceCalibrate1E { value: None } => "1E".to_owned(),
        Command::ServiceCalibrate1E {
            value: Some(value), ..
        } => format!("1E {value}"),
        Command::ServiceCalibrate1N => "1N".to_owned(),
        Command::ServiceCalibrate1V { value: None } => "1V".to_owned(),
        Command::ServiceCalibrate1V {
            value: Some(value), ..
        } => format!("1V {value}"),
        Command::ServiceCalibrate1W { value } => format!("1W {value}"),
        Command::ServiceWriteId { id, code } => format!("1I {id},{code}"),
        Command::ServiceFlashRead => "1F".to_owned(),
        Command::ServiceFlashWrite { address, data } => format!("1F {address},{data}"),
        Command::ServiceWriteConfig => "0W".to_owned(),
        Command::ServiceBandSelect { band } => format!("0Y {band}"),
        Command::ServiceReadEeprom { address, length } => format!("9E {address},{length}"),
        Command::ServiceReadEepromAddr => "9R".to_owned(),
        Command::ServiceGetVersion { param1, param2 } => format!("2V {param1},{param2}"),
        Command::ServiceGetHardware => "1G".to_owned(),
        Command::ServiceCalibrateNew { value } => format!("1C {value}"),
        Command::ServiceDynamicParam { data: None } => "1U".to_owned(),
        Command::ServiceDynamicParam {
            data: Some(data), ..
        } => format!("1U {data}"),
    };

    let mut bytes = body.into_bytes();
    bytes.push(b'\r');
    tracing::trace!(wire = %String::from_utf8_lossy(&bytes), "serialized wire format");
    bytes
}

/// Parse a response frame (without trailing `\r`) into a typed [`Response`].
///
/// # Errors
///
/// Returns [`ProtocolError::UnknownCommand`] if the mnemonic is not
/// recognized. Returns [`ProtocolError::FieldParse`] for recognised
/// commands whose payload parsing is not yet implemented.
pub fn parse(frame: &[u8]) -> Result<Response, ProtocolError> {
    // Error response
    if frame == b"?" {
        tracing::debug!(mnemonic = "?", "parsing error response");
        return Ok(Response::Error);
    }

    // Not-available response
    if frame == b"N" {
        tracing::debug!(mnemonic = "N", "parsing not-available response");
        return Ok(Response::NotAvailable);
    }

    let frame_str = std::str::from_utf8(frame).map_err(|_| {
        tracing::warn!("failed to parse frame as UTF-8");
        ProtocolError::MalformedFrame(frame.to_vec())
    })?;

    // Extract the mnemonic: first 2 characters.
    // Special case: "0M" starts with a digit.
    if frame_str.len() < 2 {
        tracing::warn!(frame = %frame_str, "frame too short to contain mnemonic");
        return Err(ProtocolError::MalformedFrame(frame.to_vec()));
    }

    let mnemonic = &frame_str[..2];
    tracing::debug!(mnemonic = %mnemonic, "parsing response");

    // The rest of the frame after the mnemonic (may start with a space).
    let payload = if frame_str.len() > 2 {
        frame_str[2..].trim_start()
    } else {
        ""
    };

    // Try each sub-parser in turn.
    let result = core::parse_core(mnemonic, payload)
        .or_else(|| vfo::parse_vfo(mnemonic, payload))
        .or_else(|| control::parse_control(mnemonic, payload))
        .or_else(|| memory::parse_memory(mnemonic, payload))
        .or_else(|| tone::parse_tone(mnemonic, payload))
        .or_else(|| scan::parse_scan(mnemonic, payload))
        .or_else(|| aprs::parse_aprs(mnemonic, payload))
        .or_else(|| dstar::parse_dstar(mnemonic, payload))
        .or_else(|| gps::parse_gps(mnemonic, payload))
        .or_else(|| bluetooth::parse_bluetooth(mnemonic, payload))
        .or_else(|| sd::parse_sd(mnemonic, payload))
        .or_else(|| user::parse_user(mnemonic, payload))
        .or_else(|| service::parse_service(mnemonic, payload));

    match result {
        Some(Ok(response)) => {
            tracing::debug!(mnemonic = %mnemonic, "response parsed successfully");
            Ok(response)
        }
        Some(Err(e)) => {
            tracing::warn!(mnemonic = %mnemonic, error = %e, "failed to parse response");
            Err(e)
        }
        None => {
            tracing::warn!(mnemonic = %mnemonic, "unknown command mnemonic");
            Err(ProtocolError::UnknownCommand(mnemonic.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parse_error_response() -> TestResult {
        let r = parse(b"?")?;
        assert!(matches!(r, Response::Error));
        Ok(())
    }

    #[test]
    fn parse_unknown_command() {
        let r = parse(b"ZZ 123");
        assert!(matches!(r, Err(ProtocolError::UnknownCommand(_))));
    }

    #[test]
    fn serialize_returns_cr_terminated() {
        let bytes = serialize(&Command::GetRadioId);
        assert!(bytes.ends_with(b"\r"));
    }

    #[test]
    fn serialize_get_radio_id() {
        let bytes = serialize(&Command::GetRadioId);
        assert_eq!(bytes, b"ID\r");
    }

    #[test]
    fn parse_ty_response() -> TestResult {
        let r = parse(b"TY K,2")?;
        let Response::RadioType { region, variant } = r else {
            return Err(format!("expected RadioType, got {r:?}").into());
        };
        assert_eq!(region, "K");
        assert_eq!(variant, 2);
        Ok(())
    }

    #[test]
    fn serialize_get_radio_type() {
        let bytes = serialize(&Command::GetRadioType);
        assert_eq!(bytes, b"TY\r");
    }

    #[test]
    fn serialize_get_mcp_status() {
        let bytes = serialize(&Command::GetMcpStatus);
        assert_eq!(bytes, b"0E\r");
    }

    #[test]
    fn serialize_set_dstar_callsign() -> TestResult {
        let bytes = serialize(&Command::SetDstarCallsign {
            slot: DstarSlot::new(1)?,
            callsign: "KQ4NIT  ".to_owned(),
            suffix: "D75A".to_owned(),
        });
        assert_eq!(bytes, b"DC 1,KQ4NIT  ,D75A\r");
        Ok(())
    }

    #[test]
    fn serialize_set_function_type() {
        let bytes = serialize(&Command::SetFunctionType { enabled: true });
        assert_eq!(bytes, b"FT 1\r");
    }

    #[test]
    fn serialize_set_smeter() -> TestResult {
        let bytes = serialize(&Command::SetSmeter {
            band: Band::B,
            level: SMeterReading::new(5)?,
        });
        assert_eq!(bytes, b"SM 1,5\r");
        Ok(())
    }

    #[test]
    fn serialize_set_battery_level() {
        let bytes = serialize(&Command::SetBatteryLevel {
            display: 0,
            level: 3,
        });
        assert_eq!(bytes, b"BL 0,3\r");
    }

    #[test]
    fn serialize_set_busy() {
        let bytes = serialize(&Command::SetBusy {
            band: Band::A,
            busy: true,
        });
        assert_eq!(bytes, b"BY 0,1\r");
    }

    #[test]
    fn serialize_set_band_scope() {
        let bytes = serialize(&Command::SetBandScope {
            band: Band::B,
            value: 1,
        });
        assert_eq!(bytes, b"BS 1,1\r");
    }

    #[test]
    fn serialize_set_firmware_version() {
        let bytes = serialize(&Command::SetFirmwareVersion {
            version: "1.03".to_owned(),
        });
        assert_eq!(bytes, b"FV 1.03\r");
    }

    #[test]
    fn serialize_set_radio_id() {
        let bytes = serialize(&Command::SetRadioId {
            model: "TH-D75".to_owned(),
        });
        assert_eq!(bytes, b"ID TH-D75\r");
    }

    #[test]
    fn serialize_set_tnc_mode() {
        let bytes = serialize(&Command::SetTncMode {
            mode: TncMode::Mmdvm,
            setting: TncBaud::Bps1200,
        });
        assert_eq!(bytes, b"TN 3,0\r");
    }

    #[test]
    fn all_mnemonics_recognized() -> TestResult {
        // All 55 standard mnemonics + 15 service mode mnemonics.
        // SR is write-only but its echo is still recognized by the parser.
        let mnemonics = [
            "AI", "AG", "BC", "BY", "DL", "DW", "ME", "MR", "PC", "RX", "SQ", "SR", "SH", "TX",
            "UP", "VM", "FQ", "FO", "PS", "FV", "BE", "ID", "CS", "TN", "BL", "GP", "GM", "SM",
            "RA", "BT", "FS", "FT", "MD", "SF", "VD", "VG", "VX", "IO", "BS", "LC", "GS", "MS",
            "PT", "AS", "DC", "DS", "RT", "FR", "US", "GW", "SD", "0M", "AE",
            // Extra mnemonics not in main dispatch table
            "TY", "0E",
            // Service mode mnemonics (20 commands, 15 unique mnemonics after 0E overlap)
            "0G", "0S", "0R", "0W", "0Y", "1A", "1D", "1E", "1I", "1N", "1U", "1V", "1W", "1F",
            "9E", "9R", "2V", "1G", "1C",
        ];
        assert_eq!(mnemonics.len(), 74);
        for mnemonic in &mnemonics {
            let input = format!("{mnemonic} 0");
            let result = parse(input.as_bytes());
            if let Err(ProtocolError::UnknownCommand(_)) = result {
                return Err(format!("Mnemonic '{mnemonic}' not recognized by parser").into());
            }
            // Other errors (FieldParse, etc.) are OK — the test only checks recognition
        }
        Ok(())
    }

    // === Service mode serialization tests ===

    #[test]
    fn serialize_enter_service_mode() {
        let bytes = serialize(&Command::EnterServiceMode);
        assert_eq!(bytes, b"0G KENWOOD\r");
    }

    #[test]
    fn serialize_exit_service_mode() {
        let bytes = serialize(&Command::ExitServiceMode);
        assert_eq!(bytes, b"0G\r");
    }

    #[test]
    fn serialize_read_calibration_data() {
        let bytes = serialize(&Command::ReadCalibrationData);
        assert_eq!(bytes, b"0S\r");
    }

    #[test]
    fn serialize_write_calibration_data() {
        let bytes = serialize(&Command::WriteCalibrationData {
            data: "AABBCCDD".to_owned(),
        });
        assert_eq!(bytes, b"0R AABBCCDD\r");
    }

    #[test]
    fn serialize_get_service_status() {
        let bytes = serialize(&Command::GetServiceStatus);
        assert_eq!(bytes, b"0E\r");
    }

    #[test]
    fn serialize_service_calibrate_1a() {
        let bytes = serialize(&Command::ServiceCalibrate1A);
        assert_eq!(bytes, b"1A\r");
    }

    #[test]
    fn serialize_service_calibrate_1d() {
        let bytes = serialize(&Command::ServiceCalibrate1D);
        assert_eq!(bytes, b"1D\r");
    }

    #[test]
    fn serialize_service_calibrate_1e_read() {
        let bytes = serialize(&Command::ServiceCalibrate1E { value: None });
        assert_eq!(bytes, b"1E\r");
    }

    #[test]
    fn serialize_service_calibrate_1e_write() {
        let bytes = serialize(&Command::ServiceCalibrate1E {
            value: Some("0FF".to_owned()),
        });
        assert_eq!(bytes, b"1E 0FF\r");
    }

    #[test]
    fn serialize_service_calibrate_1n() {
        let bytes = serialize(&Command::ServiceCalibrate1N);
        assert_eq!(bytes, b"1N\r");
    }

    #[test]
    fn serialize_service_calibrate_1v_read() {
        let bytes = serialize(&Command::ServiceCalibrate1V { value: None });
        assert_eq!(bytes, b"1V\r");
    }

    #[test]
    fn serialize_service_calibrate_1v_write() {
        let bytes = serialize(&Command::ServiceCalibrate1V {
            value: Some("ABC".to_owned()),
        });
        assert_eq!(bytes, b"1V ABC\r");
    }

    #[test]
    fn serialize_service_calibrate_1w() {
        let bytes = serialize(&Command::ServiceCalibrate1W {
            value: "5".to_owned(),
        });
        assert_eq!(bytes, b"1W 5\r");
    }

    #[test]
    fn serialize_service_write_id() {
        let bytes = serialize(&Command::ServiceWriteId {
            id: "C3C10368".to_owned(),
            code: "K01".to_owned(),
        });
        assert_eq!(bytes, b"1I C3C10368,K01\r");
    }

    #[test]
    fn serialize_service_flash_read() {
        let bytes = serialize(&Command::ServiceFlashRead);
        assert_eq!(bytes, b"1F\r");
    }

    #[test]
    fn serialize_service_flash_write() {
        let bytes = serialize(&Command::ServiceFlashWrite {
            address: "04E000".to_owned(),
            data: "AABB".to_owned(),
        });
        assert_eq!(bytes, b"1F 04E000,AABB\r");
    }

    #[test]
    fn serialize_service_write_config() {
        let bytes = serialize(&Command::ServiceWriteConfig);
        assert_eq!(bytes, b"0W\r");
    }

    #[test]
    fn serialize_service_band_select() {
        let bytes = serialize(&Command::ServiceBandSelect { band: 0 });
        assert_eq!(bytes, b"0Y 0\r");
    }

    #[test]
    fn serialize_service_read_eeprom() {
        let bytes = serialize(&Command::ServiceReadEeprom {
            address: "04E000".to_owned(),
            length: "80".to_owned(),
        });
        assert_eq!(bytes, b"9E 04E000,80\r");
    }

    #[test]
    fn serialize_service_read_eeprom_addr() {
        let bytes = serialize(&Command::ServiceReadEepromAddr);
        assert_eq!(bytes, b"9R\r");
    }

    #[test]
    fn serialize_service_get_version() {
        let bytes = serialize(&Command::ServiceGetVersion {
            param1: "00".to_owned(),
            param2: "000".to_owned(),
        });
        assert_eq!(bytes, b"2V 00,000\r");
    }

    #[test]
    fn serialize_service_get_hardware() {
        let bytes = serialize(&Command::ServiceGetHardware);
        assert_eq!(bytes, b"1G\r");
    }

    #[test]
    fn serialize_service_calibrate_new() {
        let bytes = serialize(&Command::ServiceCalibrateNew {
            value: "0A5".to_owned(),
        });
        assert_eq!(bytes, b"1C 0A5\r");
    }

    #[test]
    fn serialize_service_dynamic_param_read() {
        let bytes = serialize(&Command::ServiceDynamicParam { data: None });
        assert_eq!(bytes, b"1U\r");
    }

    #[test]
    fn serialize_service_dynamic_param_write() {
        let bytes = serialize(&Command::ServiceDynamicParam {
            data: Some("AABB".to_owned()),
        });
        assert_eq!(bytes, b"1U AABB\r");
    }

    // === Service mode parse tests ===

    #[test]
    fn parse_service_mode_response() -> TestResult {
        let r = parse(b"0G")?;
        assert!(matches!(r, Response::ServiceMode { .. }));
        Ok(())
    }

    #[test]
    fn parse_service_calibration_data() -> TestResult {
        let r = parse(b"0S AABBCCDD")?;
        let Response::ServiceCalibrationData { data } = r else {
            return Err(format!("expected ServiceCalibrationData, got {r:?}").into());
        };
        assert_eq!(data, "AABBCCDD");
        Ok(())
    }

    #[test]
    fn parse_service_calibration_param_1a() -> TestResult {
        let r = parse(b"1A 123")?;
        let Response::ServiceCalibrationParam { mnemonic, data } = r else {
            return Err(format!("expected ServiceCalibrationParam, got {r:?}").into());
        };
        assert_eq!(mnemonic, "1A");
        assert_eq!(data, "123");
        Ok(())
    }

    #[test]
    fn parse_service_version() -> TestResult {
        let r = parse(b"2V EX-5210")?;
        let Response::ServiceVersion { data } = r else {
            return Err(format!("expected ServiceVersion, got {r:?}").into());
        };
        assert_eq!(data, "EX-5210");
        Ok(())
    }

    #[test]
    fn parse_service_hardware() -> TestResult {
        let r = parse(b"1G AA,BB")?;
        let Response::ServiceHardware { data } = r else {
            return Err(format!("expected ServiceHardware, got {r:?}").into());
        };
        assert_eq!(data, "AA,BB");
        Ok(())
    }

    #[test]
    fn parse_service_eeprom_data() -> TestResult {
        let r = parse(b"9E AABBCCDD")?;
        let Response::ServiceEepromData { data } = r else {
            return Err(format!("expected ServiceEepromData, got {r:?}").into());
        };
        assert_eq!(data, "AABBCCDD");
        Ok(())
    }

    #[test]
    fn parse_service_eeprom_addr() -> TestResult {
        let r = parse(b"9R 01020304")?;
        let Response::ServiceEepromAddr { data } = r else {
            return Err(format!("expected ServiceEepromAddr, got {r:?}").into());
        };
        assert_eq!(data, "01020304");
        Ok(())
    }
}
