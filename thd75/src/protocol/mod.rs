//! Pure-logic CAT (Computer Aided Transceiver) command codec.
//!
//! This module handles serialization and parsing of the TH-D75's serial
//! command protocol. Commands are ASCII text terminated by carriage return
//! (`\r`), with parameters separated by commas. The protocol layer has no
//! async or I/O dependencies; it operates purely on byte slices.
//!
//! The standard CAT command set is represented as [`Command`] and [`Response`]
//! variants. Use [`serialize`] and [`parse`] to convert between typed
//! representations and wire format. Unverified factory-service mnemonics are
//! deliberately absent; the established MCP entry command remains paired with
//! its dedicated binary programming implementation.

pub mod aprs;
pub mod bluetooth;
pub mod codec;
pub mod control;
pub mod core;
pub mod dstar;
pub mod gps;
pub mod memory;
pub mod memread;
pub mod programming;
pub mod scan;
pub mod sd;
pub mod tone;
pub mod user;
pub mod vfo;

pub use codec::Codec;

use crate::error::ProtocolError;
use crate::types::{
    AfGainLevel, AprsCallsign, BacklightControl, Band, BeaconMode, CatChannelRecord,
    DetectOutputMode, DstarSlot, DvGatewayMode, FilterMode, FilterWidthIndex, FineStep, Frequency,
    GpsRadioMode, MemoryChannelRecord, MemoryReadOffset, MemorySelector, Mode, MyPositionSelection,
    PowerLevel, RadioClock, ReadLen, SMeterReading, ScanResumeMethod, SquelchLevel, StepSize,
    TncBaud, TncMode, VfoMemoryMode, VoxDelay, VoxGain,
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
    /// Get full frequency and settings (FO read).
    ///
    /// Returns full channel data including tone, shift, and step settings.
    /// Works in any mode.
    GetFrequencyFull {
        /// Target band.
        band: Band,
    },
    /// Get firmware version (FV).
    GetFirmwareVersion,
    /// Get power on/off status (PS read).
    GetPowerStatus,
    /// Get radio model ID (ID).
    GetRadioId,
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
    /// Exact wire format: `AG AAA` (AAA: 000-200, three decimal digits).
    /// AF gain is global; firmware rejects band-indexed writes.
    SetAfGain {
        /// Global AF gain level (0-200).
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
    FrequencyUp,
    /// Tune frequency down by one step (DW action).
    ///
    /// The firmware accepts only bare `DW`; it acts on the current context.
    FrequencyDown,
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
    /// Get auto-info notification mode (bare AI read).
    GetAutoInfo,
    /// Set auto-info notification mode (AI write).
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
    /// Switch the current operating context to receive mode (bare RX action).
    Receive,
    /// Key the transmitter (TX action).
    ///
    /// # Safety
    /// **This transmits on air.** Ensure you are authorized to transmit on
    /// the current frequency, have proper identification, and comply with
    /// all applicable regulations. Use [`Command::Receive`] to return to receive mode.
    Transmit,
    /// Get LCD backlight control mode (LC read).
    GetBacklightControl,
    /// Set LCD backlight control mode (LC write).
    SetBacklightControl {
        /// Backlight mode (0=Manual, 1=On, 2=Auto, 3=Auto DC-IN).
        mode: BacklightControl,
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
    /// Hardware-verified: `MR band\r` returns `MR selector`; the response
    /// does not repeat the request band. Example: `MR 0\r` may return
    /// `MR 021`, while special memories use selectors such as `L00` or `Pri`.
    ///
    /// This is a READ that queries which channel is active, not an action.
    GetCurrentChannel {
        /// Target band.
        band: Band,
    },
    /// Get memory channel data (ME read).
    GetMemoryChannel {
        /// Exact memory selector.
        selector: MemorySelector,
    },
    /// Recall memory channel: switches the radio's active channel (MR write).
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
        /// Exact memory selector.
        selector: MemorySelector,
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
    /// Get the MW/SW receive antenna selection (BS read).
    GetBarAntenna,
    /// Select the MW/SW receive antenna (BS write).
    SetBarAntenna {
        /// `true` selects the internal bar antenna; `false` selects ANT Connector.
        enabled: bool,
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
    /// Get the selected APRS/GPS My Position entry (MS read).
    GetMyPositionSelection,
    /// Set the selected APRS/GPS My Position entry (MS write).
    SetMyPositionSelection {
        /// Validated My Position selection (0-5).
        selection: MyPositionSelection,
    },
    /// Get the APRS My Callsign value stored at MCP `aprs.MyCallsign` (CS read).
    GetAprsCallsign,
    /// Set the APRS My Callsign value (CS write, at most nine characters).
    SetAprsCallsign {
        /// Validated APRS callsign, optionally including an SSID.
        callsign: AprsCallsign,
    },

    // === D-STAR (DS, GW) ===
    /// Get active D-STAR callsign slot (DS read).
    GetDstarSlot,
    /// Set active D-STAR callsign slot (DS write).
    SetDstarSlot {
        /// D-STAR memory slot (1-6).
        slot: DstarSlot,
    },
    /// Get gateway (GW read).
    GetGateway,

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
    /// presence. Do NOT send `SD PROGRAM`: it enters programming mode
    /// and the radio stops responding to normal CAT commands.
    GetSdCard,

    // === Memory read (requires modified firmware) ===
    /// Read `len` bytes at `offset` from the radio's readable memory window.
    ///
    /// # Firmware requirement
    ///
    /// Requires firmware modified by the `thd75-fw` project. It reuses the `GM`
    /// mnemonic, which on stock firmware selects the GPS operating mode, so a
    /// stock radio refuses this request and [`Command::GetGpsMode`] stops
    /// working on a modified one. See [`memread`] for the wire format.
    ReadMemory {
        /// Offset into the readable window. Not an absolute address.
        offset: MemoryReadOffset,
        /// Number of bytes to read, 1 to 256.
        len: ReadLen,
    },

    // === Extra (TY) ===
    /// Get radio type/region code (TY read).
    ///
    /// Not in the firmware's 53-command dispatch table, likely processed
    /// by a separate code path. Returns a region string and variant number
    /// (e.g., `TY K,2` for US region, variant 2).
    GetRadioType,
}

/// A parsed response from the radio.
#[derive(Debug, Clone)]
pub enum Response {
    // === Core ===
    /// Frequency response (FQ).
    Frequency {
        /// Band the frequency is on.
        band: Band,
        /// Current receive frequency.
        frequency: Frequency,
    },
    /// Full frequency and settings response (FO).
    FrequencyFull {
        /// Band the data is for.
        band: Band,
        /// Lossless shared FO/ME channel record.
        channel: CatChannelRecord,
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
    /// Bare acknowledgment emitted by some firmware after an AI write.
    ///
    /// This carries no state and must not be interpreted as `AI 0`.
    AutoInfoAck,
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
    /// Frequency up acknowledgement (UP).
    FrequencyUp,
    /// LCD backlight control mode response (LC).
    BacklightControl {
        /// Current backlight mode.
        mode: BacklightControl,
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
        /// Exact memory selector returned by the radio.
        selector: MemorySelector,
        /// Lossless ME record, including both unidentified ME-only fields.
        record: MemoryChannelRecord,
    },
    /// Memory recall echo response (MR write acknowledgment).
    ///
    /// When `MR band,channel` is sent as a write/recall, the radio echoes
    /// back the band and channel as acknowledgment.
    MemoryRecall {
        /// Target band.
        band: Band,
        /// Exact selector echoed by the radio.
        selector: MemorySelector,
    },
    /// Current memory selector response (MR read).
    ///
    /// Hardware-verified: an `MR band\r` request returns only the selected
    /// memory (`MR 021`, `MR L00`, `MR Pri`, and so on). The request band is
    /// not present in this frame and must not be inferred from the selector.
    CurrentChannel {
        /// Selector returned by the bandless MR response.
        selector: MemorySelector,
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
    /// The exact `RT ------------` response represents an unavailable clock.
    RealTimeClock {
        /// Calendar-valid date/time or the explicit unavailable state.
        clock: RadioClock,
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
    /// MW/SW receive antenna selection response (BS).
    BarAntenna {
        /// `true` means internal bar antenna; `false` means ANT Connector.
        enabled: bool,
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
    /// APRS/GPS My Position selection response (MS).
    MyPositionSelection {
        /// Current validated selection (0-5).
        selection: MyPositionSelection,
    },
    /// APRS My Callsign response (CS).
    AprsCallsign {
        /// Current APRS station callsign, optionally including an SSID.
        callsign: AprsCallsign,
    },

    // === D-STAR ===
    /// Active D-STAR callsign slot response (DS).
    DstarSlot {
        /// Active D-STAR memory slot (1-6).
        slot: DstarSlot,
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

    // === Memory read (requires modified firmware) ===
    /// Bytes read from the radio's readable memory window.
    ///
    /// The radio echoes the requested offset in its reply, and this is that
    /// echoed value rather than anything the host supplied, so comparing it
    /// against the request detects a stale or mis-routed answer.
    MemoryData {
        /// Offset echoed back by the radio.
        offset: MemoryReadOffset,
        /// The decoded bytes.
        bytes: Vec<u8>,
    },

    // === Extra (TY) ===
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

    // === Special ===
    /// Write acknowledgment (radio echoes the command).
    Ok,
    /// Error response (`?\r`).
    Error,
    /// Not available response (`N\r`): command not supported in current mode.
    NotAvailable,
}

/// Band a band-indexed command addresses, `None` for global commands.
///
/// Used by the response-matching loop: with AI mode enabled the radio
/// pushes `BY`/`FQ`/`MD`/`SQ` frames unsolicited (the same mnemonics
/// as the reads), so an in-flight band-A query must not accept a
/// band-B push as its answer.
pub(crate) const fn command_band(cmd: &Command) -> Option<Band> {
    match cmd {
        Command::GetFrequency { band }
        | Command::GetFrequencyFull { band }
        | Command::GetPowerLevel { band }
        | Command::SetPowerLevel { band, .. }
        | Command::GetVfoMemoryMode { band }
        | Command::SetVfoMemoryMode { band, .. }
        | Command::GetSquelch { band }
        | Command::SetSquelch { band, .. }
        | Command::GetSmeter { band }
        | Command::GetMode { band }
        | Command::SetMode { band, .. }
        | Command::GetBusy { band }
        | Command::GetAttenuator { band }
        | Command::SetAttenuator { band, .. }
        | Command::GetStepSize { band }
        | Command::SetStepSize { band, .. }
        | Command::GetCurrentChannel { band }
        | Command::RecallMemoryChannel { band, .. }
        | Command::SetBand { band } => Some(*band),
        _ => None,
    }
}

/// Band a band-indexed response pertains to, `None` for global ones.
///
/// Counterpart of [`command_band`] for the response side of the
/// unsolicited-push disambiguation.
pub(crate) const fn response_band(response: &Response) -> Option<Band> {
    match response {
        Response::Frequency { band, .. }
        | Response::FrequencyFull { band, .. }
        | Response::PowerLevel { band, .. }
        | Response::VfoMemoryMode { band, .. }
        | Response::Squelch { band, .. }
        | Response::Smeter { band, .. }
        | Response::Mode { band, .. }
        | Response::Busy { band, .. }
        | Response::Attenuator { band, .. }
        | Response::StepSize { band, .. }
        | Response::MemoryRecall { band, .. }
        | Response::BandResponse { band } => Some(*band),
        _ => None,
    }
}

/// Get the CAT mnemonic for a command (for logging).
#[must_use]
pub const fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::GetFrequency { .. } => "FQ",
        Command::GetFrequencyFull { .. } => "FO",
        Command::GetFirmwareVersion => "FV",
        Command::GetPowerStatus => "PS",
        Command::GetRadioId => "ID",
        Command::GetPowerLevel { .. } | Command::SetPowerLevel { .. } => "PC",
        Command::GetBand | Command::SetBand { .. } => "BC",
        Command::GetVfoMemoryMode { .. } | Command::SetVfoMemoryMode { .. } => "VM",
        Command::GetFmRadio | Command::SetFmRadio { .. } => "FR",
        Command::GetAfGain | Command::SetAfGain { .. } => "AG",
        Command::GetSquelch { .. } | Command::SetSquelch { .. } => "SQ",
        Command::GetSmeter { .. } => "SM",
        Command::GetMode { .. } | Command::SetMode { .. } => "MD",
        Command::GetFineStep => "FS",
        Command::GetFunctionType | Command::SetFunctionType { .. } => "FT",
        Command::GetFilterWidth { .. } | Command::SetFilterWidth { .. } => "SH",
        Command::FrequencyUp => "UP",
        Command::FrequencyDown => "DW",
        Command::GetAttenuator { .. } | Command::SetAttenuator { .. } => "RA",
        Command::GetAutoInfo | Command::SetAutoInfo { .. } => "AI",
        Command::GetBusy { .. } => "BY",
        Command::GetDualBand | Command::SetDualBand { .. } => "DL",
        Command::Receive => "RX",
        Command::Transmit => "TX",
        Command::GetBacklightControl | Command::SetBacklightControl { .. } => "LC",
        Command::GetIoPort | Command::SetIoPort { .. } => "IO",
        Command::GetBatteryLevel => "BL",
        Command::GetVoxDelay | Command::SetVoxDelay { .. } => "VD",
        Command::GetVoxGain | Command::SetVoxGain { .. } => "VG",
        Command::GetVox | Command::SetVox { .. } => "VX",
        Command::GetCurrentChannel { .. } | Command::RecallMemoryChannel { .. } => "MR",
        Command::GetMemoryChannel { .. } => "ME",
        Command::EnterProgrammingMode => "0M",
        Command::GetTncMode | Command::SetTncMode { .. } => "TN",
        Command::GetDstarCallsign { .. } | Command::SetDstarCallsign { .. } => "DC",
        Command::GetRealTimeClock => "RT",
        Command::SetScanResume { .. } => "SR",
        Command::GetStepSize { .. } | Command::SetStepSize { .. } => "SF",
        Command::GetBarAntenna | Command::SetBarAntenna { .. } => "BS",
        Command::GetTncBaud | Command::SetTncBaud { .. } => "AS",
        Command::GetSerialInfo => "AE",
        Command::GetBeaconType | Command::SetBeaconType { .. } => "PT",
        Command::GetMyPositionSelection | Command::SetMyPositionSelection { .. } => "MS",
        Command::GetAprsCallsign | Command::SetAprsCallsign { .. } => "CS",
        Command::GetDstarSlot | Command::SetDstarSlot { .. } => "DS",
        Command::GetGateway => "GW",
        Command::GetGpsConfig | Command::SetGpsConfig { .. } => "GP",
        Command::GetGpsMode => "GM",
        Command::GetGpsSentences | Command::SetGpsSentences { .. } => "GS",
        Command::GetBluetooth | Command::SetBluetooth { .. } => "BT",
        Command::GetSdCard => "SD",
        Command::ReadMemory { .. } => memread::MEM_READ_MNEMONIC,
        Command::GetRadioType => "TY",
    }
}

/// Serialize a command to wire format (bytes ending with `\r`).
///
/// Converts a [`Command`] into the byte sequence expected by the radio's
/// CAT protocol. Each serialized command ends with a carriage return.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Dispatch table over every CAT Command variant: one match arm per command is the \
              clearest mapping of the Command enum to its wire format. Splitting by submodule \
              would hide the complete command inventory."
)]
pub fn serialize(cmd: &Command) -> Vec<u8> {
    let cmd_mnemonic = command_name(cmd);
    tracing::debug!(command = %cmd_mnemonic, "serializing command");

    let body = match cmd {
        // Core
        Command::GetFrequency { band } => {
            format!("FQ {}", u8::from(*band))
        }
        Command::GetFrequencyFull { band } => {
            format!("FO {}", u8::from(*band))
        }
        Command::GetFirmwareVersion => "FV".to_owned(),
        Command::GetPowerStatus => "PS".to_owned(),
        Command::GetRadioId => "ID".to_owned(),
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
        Command::SetAfGain { level } => {
            // D75 firmware AG write handler expects bare `AG AAA\r`.
            // Band-indexed `AG band,level` is rejected with `?`; the exact
            // global domain is 000-200.
            format!("AG {:03}", level.as_u8())
        }
        Command::GetSquelch { band } => format!("SQ {}", u8::from(*band)),
        Command::SetSquelch { band, level } => {
            format!("SQ {},{}", u8::from(*band), level.as_u8())
        }
        Command::GetSmeter { band } => format!("SM {}", u8::from(*band)),
        Command::GetMode { band } => format!("MD {}", u8::from(*band)),
        Command::SetMode { band, mode } => {
            format!("MD {},{}", u8::from(*band), u8::from(*mode))
        }
        Command::GetFineStep => "FS".to_owned(),
        Command::GetFunctionType => "FT".to_owned(),
        Command::SetFunctionType { enabled } => {
            format!("FT {}", u8::from(*enabled))
        }
        Command::GetFilterWidth { mode } => format!("SH {}", u8::from(*mode)),
        Command::SetFilterWidth { mode, width } => {
            format!("SH {},{}", u8::from(*mode), width.as_u8())
        }
        Command::FrequencyUp => "UP".to_owned(),
        Command::FrequencyDown => "DW".to_owned(),
        Command::GetAttenuator { band } => format!("RA {}", u8::from(*band)),
        Command::SetAttenuator { band, enabled } => {
            format!("RA {},{}", u8::from(*band), u8::from(*enabled))
        }

        // Control
        Command::GetAutoInfo => "AI".to_owned(),
        Command::SetAutoInfo { enabled } => format!("AI {}", u8::from(*enabled)),
        Command::GetBusy { band } => format!("BY {}", u8::from(*band)),
        Command::GetDualBand => "DL".to_owned(),
        Command::SetDualBand { enabled } => format!("DL {}", u8::from(*enabled)),
        Command::Receive => "RX".to_owned(),
        Command::Transmit => "TX".to_owned(),
        Command::GetBacklightControl => "LC".to_owned(),
        Command::SetBacklightControl { mode } => format!("LC {}", u8::from(*mode)),
        Command::GetIoPort => "IO".to_owned(),
        Command::SetIoPort { value } => format!("IO {}", u8::from(*value)),
        Command::GetBatteryLevel => "BL".to_owned(),
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
        Command::GetMemoryChannel { selector } => {
            format!("ME {selector}")
        }
        Command::RecallMemoryChannel { band, selector } => {
            format!("MR {},{selector}", u8::from(*band))
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
        Command::GetBarAntenna => "BS".to_owned(),
        Command::SetBarAntenna { enabled } => format!("BS {}", u8::from(*enabled)),

        // APRS
        Command::GetTncBaud => "AS".to_owned(),
        Command::SetTncBaud { rate } => format!("AS {}", u8::from(*rate)),
        Command::GetSerialInfo => "AE".to_owned(),
        Command::GetBeaconType => "PT".to_owned(),
        Command::SetBeaconType { mode } => format!("PT {}", u8::from(*mode)),
        Command::GetMyPositionSelection => "MS".to_owned(),
        Command::SetMyPositionSelection { selection } => {
            format!("MS {}", u8::from(*selection))
        }
        Command::GetAprsCallsign => "CS".to_owned(),
        Command::SetAprsCallsign { callsign } => format!("CS {}", callsign.as_str()),

        // D-STAR
        Command::GetDstarSlot => "DS".to_owned(),
        Command::SetDstarSlot { slot } => format!("DS {}", slot.as_u8()),
        Command::GetGateway => "GW".to_owned(),

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

        // Memory read (requires modified firmware)
        Command::ReadMemory { offset, len } => memread::serialize_read(*offset, *len),

        // Extra
        Command::GetRadioType => "TY".to_owned(),
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

    // Char-boundary-safe: a valid-UTF-8 frame starting with a
    // multi-byte character has no 2-byte mnemonic; reject it rather
    // than panicking on the slice.
    let Some(mnemonic) = frame_str.get(..2) else {
        tracing::warn!(frame = %frame_str, "frame mnemonic is not two ASCII bytes");
        return Err(ProtocolError::MalformedFrame(frame.to_vec()));
    };
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
        // Memory read must precede the GPS parser: both answer to `GM`, and
        // the GPS parser always returns `Some`, so it would short-circuit the
        // chain. This parser declines on shape, letting GPS replies through.
        .or_else(|| memread::parse_memread(mnemonic, payload))
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
    fn parse_multibyte_utf8_frame_is_error_not_panic() {
        // A valid-UTF-8 frame whose first character is multi-byte
        // ("€" = 3 bytes) lands mid-character at byte index 2; the
        // mnemonic extraction must reject it, not panic.
        let r = parse("€X 1".as_bytes());
        assert!(r.is_err(), "non-ASCII mnemonic must be an error: {r:?}");
    }

    #[test]
    fn parse_mr_multibyte_payload_is_error_not_panic() {
        // The MR payload splitter indexes bytes 0 and 1..; a
        // multi-byte first character must not panic it.
        let r = parse("MR €,005".as_bytes());
        assert!(r.is_err(), "non-ASCII MR payload must be an error: {r:?}");
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
    fn serialize_set_bar_antenna() {
        let bytes = serialize(&Command::SetBarAntenna { enabled: true });
        assert_eq!(bytes, b"BS 1\r");
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
        // All standard mnemonics.
        // SR is write-only but its echo is still recognized by the parser.
        let mnemonics = [
            "AI", "AG", "BC", "BY", "DL", "DW", "ME", "MR", "PC", "RX", "SQ", "SR", "SH", "TX",
            "UP", "VM", "FQ", "FO", "PS", "FV", "ID", "CS", "TN", "BL", "GP", "GM", "SM", "RA",
            "BT", "FS", "FT", "MD", "SF", "VD", "VG", "VX", "IO", "BS", "LC", "GS", "MS", "PT",
            "AS", "DC", "DS", "RT", "FR", "GW", "SD", "0M", "AE",
            // Extra mnemonics not in main dispatch table
            "TY",
        ];
        assert_eq!(mnemonics.len(), 52);
        for mnemonic in &mnemonics {
            let input = format!("{mnemonic} 0");
            let result = parse(input.as_bytes());
            if let Err(ProtocolError::UnknownCommand(_)) = result {
                return Err(format!("Mnemonic '{mnemonic}' not recognized by parser").into());
            }
            // Other errors (FieldParse, etc.) are OK; the test only checks recognition
        }
        Ok(())
    }
}
