//! Pure-logic CAT (Computer Aided Transceiver) command codec.
//!
//! This module handles serialization and parsing of the TH-D75's serial
//! command protocol. Commands are ASCII text terminated by carriage return
//! (`\r`), with parameters separated by commas. The protocol layer has no
//! async or I/O dependencies — it operates purely on byte slices.
//!
//! All 55 CAT commands (53 from the firmware dispatch table plus 2 extra
//! mnemonics TY and 0E) are represented as variants of [`Command`]
//! (outgoing) and [`Response`] (incoming). Use [`serialize`] and [`parse`]
//! to convert between typed representations and wire format.

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
pub mod tone;
pub mod user;
pub mod vfo;

pub use codec::Codec;

use crate::error::ProtocolError;
#[allow(unused_imports)]
use crate::types::{Band, ChannelMemory, Mode, PowerLevel, StepSize, ToneCode};

/// A CAT command to send to the radio.
#[derive(Debug, Clone)]
#[allow(clippy::module_name_repetitions)]
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
    /// Get power on/off status (PS read).
    GetPowerStatus,
    /// Get radio model ID (ID).
    GetRadioId,
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
        /// VFO/Memory mode index (0 = VFO, 1 = Memory, 2 = Call, 3 = WX).
        mode: u8,
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
    SetAfGain {
        /// Target band (ignored by firmware — AG is global).
        band: Band,
        /// Gain level (0-99).
        level: u8,
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
        level: u8,
    },
    /// Get S-meter reading (SM read).
    GetSmeter {
        /// Target band.
        band: Band,
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
    /// Get frequency step for a band (FS read).
    ///
    /// D75 RE: `FS x,y` (x: band, y: step index 0-11).
    /// Sends `FS band\r`.
    ///
    /// # Mode requirement
    /// Hardware-verified: returns `N` (not available) when the radio is in
    /// Memory mode. The radio must be in VFO mode on the target band for
    /// FS to return data.
    GetFrequencyStep {
        /// Target band.
        band: Band,
    },
    /// Set frequency step for a band (FS write).
    ///
    /// D75 RE: `FS x,y` (x: band, y: step index 0-11).
    /// Sends `FS band,step\r`.
    SetFrequencyStep {
        /// Target band.
        band: Band,
        /// Step size to set.
        step: StepSize,
    },
    /// Get function type (FT bare read, no band parameter).
    ///
    /// Sends `FT\r` (bare). The radio returns the current function type.
    GetFunctionType,
    /// Get filter width by mode index (SH read).
    ///
    /// Per Operating Tips §5.10: SSB high-cut 2.2–3.0 kHz (Menu 120),
    /// CW bandwidth 0.3–2.0 kHz (Menu 121), AM high-cut 3.0–7.5 kHz
    /// (Menu 122). `mode_index`: 0 = SSB, 1 = CW, 2 = AM.
    GetFilterWidth {
        /// Mode index (0 = SSB, 1 = CW, 2 = AM).
        mode_index: u8,
    },
    /// Set filter width by mode index (SH write).
    ///
    /// Sets the IF receive filter width for the specified mode. The width
    /// value maps to the filter selection index for that mode (see
    /// [`GetFilterWidth`](Command::GetFilterWidth) for mode descriptions).
    SetFilterWidth {
        /// Mode index (0 = SSB, 1 = CW, 2 = AM).
        mode_index: u8,
        /// Filter width setting index.
        width: u8,
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
    /// Sends `LC 0` or `LC 1`. CAT value is inverted on the D75
    /// (0 = locked, 1 = unlocked). For full lock configuration, use
    /// [`SetLockFull`](Command::SetLockFull).
    SetLock {
        /// Whether key lock is engaged. CAT value is inverted on D75.
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
        /// Key lock type: 0=key only, 1=PTT only, 2=key+PTT.
        lock_type: u8,
        /// Lock key A.
        lock_a: bool,
        /// Lock key B.
        lock_b: bool,
        /// Lock key C.
        lock_c: bool,
        /// Lock PTT.
        lock_ptt: bool,
    },
    /// Get I/O port state (IO read).
    GetIoPort,
    /// Set I/O port state (IO write).
    SetIoPort {
        /// I/O port value to set.
        value: u8,
    },
    /// Get battery level (BL read).
    ///
    /// Per KI4LAX CAT reference: BL returns battery charge state.
    /// 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green).
    /// Read-only command — the radio does not accept BL writes.
    GetBatteryLevel,
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
        /// VOX delay value.
        delay: u8,
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
        /// VOX gain value.
        gain: u8,
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
    GetTncMode,
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
        slot: u8,
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
        /// Scan resume mode value.
        mode: u8,
    },
    /// Get scan function setting for a band (SF read, band-indexed).
    ///
    /// Hardware-verified: `SF band\r` returns `SF band,value`.
    /// Both `SF 0` and `SF 1` confirmed working.
    GetScanRange {
        /// Target band.
        band: Band,
    },
    /// Get band scope data (BS read).
    GetBandScope {
        /// Target band.
        band: Band,
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
        /// Baud rate index (0 = 1200, 1 = 9600).
        rate: u8,
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
        /// Beacon type mode value.
        mode: u8,
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
        /// Slot number to set.
        slot: u8,
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
        /// Slot number to select.
        slot: u8,
    },
    /// Get gateway (GW read).
    GetGateway,
    /// Set gateway value (GW write).
    SetGateway {
        /// Gateway value to set.
        value: u8,
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
    #[allow(clippy::struct_excessive_bools)]
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
}

/// A parsed response from the radio.
#[derive(Debug, Clone)]
#[allow(clippy::module_name_repetitions)]
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
        /// VFO/Memory mode index (0 = VFO, 1 = Memory, 2 = Call, 3 = WX).
        mode: u8,
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
        level: u8,
    },
    /// Squelch level response (SQ).
    Squelch {
        /// Band the squelch is for.
        band: Band,
        /// Squelch level.
        level: u8,
    },
    /// S-meter reading response (SM).
    Smeter {
        /// Band the reading is for.
        band: Band,
        /// S-meter level.
        level: u8,
    },
    /// Operating mode response (MD).
    Mode {
        /// Band the mode is for.
        band: Band,
        /// Current operating mode.
        mode: Mode,
    },
    /// Frequency step response (FS).
    ///
    /// D75 RE: `FS x,y` (x: band, y: step index 0-11).
    FrequencyStep {
        /// Band the step is for.
        band: Band,
        /// Current step size.
        step: StepSize,
    },
    /// Function type response (FT).
    FunctionType {
        /// Function type value.
        value: u8,
    },
    /// Filter width response (SH).
    FilterWidth {
        /// Mode index queried (0 = SSB, 1 = CW, 2 = AM).
        mode_index: u8,
        /// Filter width setting.
        width: u8,
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
    Lock {
        /// Whether key lock is engaged. CAT value is inverted on D75.
        locked: bool,
    },
    /// I/O port state response (IO).
    IoPort {
        /// I/O port value.
        value: u8,
    },
    /// Battery level response (BL).
    ///
    /// 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green).
    BatteryLevel {
        /// Battery charge level (0-3).
        level: u8,
    },
    /// VOX delay response (VD).
    VoxDelay {
        /// Current VOX delay.
        delay: u8,
    },
    /// VOX gain response (VG).
    VoxGain {
        /// Current VOX gain.
        gain: u8,
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
    TncMode {
        /// TNC mode value.
        mode: u8,
        /// TNC setting value.
        setting: u8,
    },
    /// D-STAR callsign data response (DC).
    ///
    /// Hardware-verified: `DC slot\r` returns `DC slot,callsign,suffix`.
    /// Example: `DC 1,KQ4NIT  ,D75A`.
    DstarCallsign {
        /// Callsign slot (1-6).
        slot: u8,
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
    /// Scan range response (SF).
    ///
    /// Format: `band,value` where band is 0/1 and value is the scan function setting.
    ScanRange {
        /// Band the setting is for.
        band: Band,
        /// Scan function value.
        value: u8,
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
        /// Baud rate index (0 = 1200, 1 = 9600).
        rate: u8,
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
        /// Beacon type mode value.
        mode: u8,
    },
    /// APRS position source response (MS read).
    PositionSource {
        /// Position source index (0-based).
        source: u8,
    },

    // === D-STAR ===
    /// Active D-STAR callsign slot response (DS).
    DstarSlot {
        /// Active callsign slot index.
        slot: u8,
    },
    /// Active callsign slot number response (CS).
    ///
    /// CS returns a slot number, NOT the callsign text. The actual callsign
    /// text is accessible via DC (D-STAR callsign) slots 1-6.
    ActiveCallsignSlot {
        /// Active callsign slot number.
        slot: u8,
    },
    /// Gateway response (GW).
    Gateway {
        /// Gateway value.
        value: u8,
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
    /// Single value: 0 = GPS off, other values TBD.
    GpsMode {
        /// GPS mode value.
        mode: u8,
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
        Command::GetFirmwareVersion => "FV",
        Command::GetPowerStatus => "PS",
        Command::GetRadioId => "ID",
        Command::GetBeep | Command::SetBeep { .. } => "BE",
        Command::GetPowerLevel { .. } | Command::SetPowerLevel { .. } => "PC",
        Command::GetBand | Command::SetBand { .. } => "BC",
        Command::GetVfoMemoryMode { .. } | Command::SetVfoMemoryMode { .. } => "VM",
        Command::GetFmRadio | Command::SetFmRadio { .. } => "FR",
        Command::GetAfGain | Command::SetAfGain { .. } => "AG",
        Command::GetSquelch { .. } | Command::SetSquelch { .. } => "SQ",
        Command::GetSmeter { .. } => "SM",
        Command::GetMode { .. } | Command::SetMode { .. } => "MD",
        Command::GetFrequencyStep { .. } | Command::SetFrequencyStep { .. } => "FS",
        Command::GetFunctionType => "FT",
        Command::GetFilterWidth { .. } | Command::SetFilterWidth { .. } => "SH",
        Command::FrequencyUp { .. } => "UP",
        Command::FrequencyDown { .. } => "DW",
        Command::GetAttenuator { .. } | Command::SetAttenuator { .. } => "RA",
        Command::SetAutoInfo { .. } => "AI",
        Command::GetBusy { .. } => "BY",
        Command::GetDualBand | Command::SetDualBand { .. } => "DL",
        Command::Receive { .. } => "RX",
        Command::Transmit { .. } => "TX",
        Command::GetLock | Command::SetLock { .. } | Command::SetLockFull { .. } => "LC",
        Command::GetIoPort | Command::SetIoPort { .. } => "IO",
        Command::GetBatteryLevel => "BL",
        Command::GetVoxDelay | Command::SetVoxDelay { .. } => "VD",
        Command::GetVoxGain | Command::SetVoxGain { .. } => "VG",
        Command::GetVox | Command::SetVox { .. } => "VX",
        Command::GetCurrentChannel { .. } | Command::RecallMemoryChannel { .. } => "MR",
        Command::GetMemoryChannel { .. } | Command::SetMemoryChannel { .. } => "ME",
        Command::EnterProgrammingMode => "0M",
        Command::GetTncMode => "TN",
        Command::GetDstarCallsign { .. } => "DC",
        Command::GetRealTimeClock => "RT",
        Command::SetScanResume { .. } => "SR",
        Command::GetScanRange { .. } => "SF",
        Command::GetBandScope { .. } => "BS",
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
        Command::GetMcpStatus => "0E",
    }
}

/// Serialize a command to wire format (bytes ending with `\r`).
///
/// Converts a [`Command`] into the byte sequence expected by the radio's
/// CAT protocol. Each serialized command ends with a carriage return.
#[must_use]
#[allow(clippy::too_many_lines)]
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
        Command::GetPowerStatus => "PS".to_owned(),
        Command::GetRadioId => "ID".to_owned(),
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
            format!("VM {},{}", u8::from(*band), mode)
        }
        Command::GetFmRadio => "FR".to_owned(),
        Command::SetFmRadio { enabled } => format!("FR {}", u8::from(*enabled)),

        // VFO
        Command::GetAfGain => "AG".to_owned(),
        Command::SetAfGain { band: _, level } => {
            // D75 firmware AG write handler expects bare `AG AAA\r`.
            // Band-indexed `AG band,level` is rejected with `?`.
            // Per KI4LAX: 3-digit zero-padded, range 000-099.
            format!("AG {level:03}")
        }
        Command::GetSquelch { band } => format!("SQ {}", u8::from(*band)),
        Command::SetSquelch { band, level } => {
            format!("SQ {},{}", u8::from(*band), level)
        }
        Command::GetSmeter { band } => format!("SM {}", u8::from(*band)),
        Command::GetMode { band } => format!("MD {}", u8::from(*band)),
        Command::SetMode { band, mode } => {
            format!("MD {},{}", u8::from(*band), u8::from(*mode))
        }
        Command::GetFrequencyStep { band } => format!("FS {}", u8::from(*band)),
        Command::SetFrequencyStep { band, step } => {
            format!("FS {},{}", u8::from(*band), u8::from(*step))
        }
        Command::GetFunctionType => "FT".to_owned(),
        Command::GetFilterWidth { mode_index } => format!("SH {mode_index}"),
        Command::SetFilterWidth { mode_index, width } => {
            format!("SH {mode_index},{width}")
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
            lock_type,
            u8::from(*lock_a),
            u8::from(*lock_b),
            u8::from(*lock_c),
            u8::from(*lock_ptt),
        ),
        Command::GetIoPort => "IO".to_owned(),
        Command::SetIoPort { value } => format!("IO {value}"),
        Command::GetBatteryLevel => "BL".to_owned(),
        Command::GetVoxDelay => "VD".to_owned(),
        Command::SetVoxDelay { delay } => format!("VD {delay}"),
        Command::GetVoxGain => "VG".to_owned(),
        Command::SetVoxGain { gain } => format!("VG {gain}"),
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
        Command::GetDstarCallsign { slot } => format!("DC {slot}"),
        Command::GetRealTimeClock => "RT".to_owned(),

        // Scan
        Command::SetScanResume { mode } => format!("SR {mode}"),
        Command::GetScanRange { band } => format!("SF {}", u8::from(*band)),
        Command::GetBandScope { band } => format!("BS {}", u8::from(*band)),

        // APRS
        Command::GetTncBaud => "AS".to_owned(),
        Command::SetTncBaud { rate } => format!("AS {rate}"),
        Command::GetSerialInfo => "AE".to_owned(),
        Command::GetBeaconType => "PT".to_owned(),
        Command::SetBeaconType { mode } => format!("PT {mode}"),
        Command::GetPositionSource => "MS".to_owned(),
        Command::SendMessage { text } => format!("MS {text}"),

        // D-STAR
        Command::GetDstarSlot => "DS".to_owned(),
        Command::SetDstarSlot { slot } => format!("DS {slot}"),
        Command::GetActiveCallsignSlot => "CS".to_owned(),
        Command::SetActiveCallsignSlot { slot } => format!("CS {slot}"),
        Command::GetGateway => "GW".to_owned(),
        Command::SetGateway { value } => format!("GW {value}"),

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
        Command::GetMcpStatus => "0E".to_owned(),
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
        .or_else(|| user::parse_user(mnemonic, payload));

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

    #[test]
    fn parse_error_response() {
        let r = parse(b"?").unwrap();
        assert!(matches!(r, Response::Error));
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
    fn parse_ty_response() {
        let r = parse(b"TY K,2").unwrap();
        match r {
            Response::RadioType { region, variant } => {
                assert_eq!(region, "K");
                assert_eq!(variant, 2);
            }
            other => panic!("expected RadioType, got {other:?}"),
        }
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
    fn all_55_mnemonics_recognized() {
        // All 55 known mnemonics. SR is write-only but its echo is still
        // recognized by the parser (returns Ok).
        let mnemonics = [
            "AI", "AG", "BC", "BY", "DL", "DW", "ME", "MR", "PC", "RX", "SQ", "SR", "SH", "TX",
            "UP", "VM", "FQ", "FO", "PS", "FV", "BE", "ID", "CS", "TN", "BL", "GP", "GM", "SM",
            "RA", "BT", "FS", "FT", "MD", "SF", "VD", "VG", "VX", "IO", "BS", "LC", "GS", "MS",
            "PT", "AS", "DC", "DS", "RT", "FR", "US", "GW", "SD", "0M", "AE",
            // Extra mnemonics not in main dispatch table
            "TY", "0E",
        ];
        assert_eq!(mnemonics.len(), 55);
        for mnemonic in &mnemonics {
            let input = format!("{mnemonic} 0");
            let result = parse(input.as_bytes());
            if let Err(ProtocolError::UnknownCommand(_)) = result {
                panic!("Mnemonic '{mnemonic}' not recognized by parser");
            }
            // Other errors (FieldParse, etc.) are OK — the test only checks recognition
        }
    }
}
