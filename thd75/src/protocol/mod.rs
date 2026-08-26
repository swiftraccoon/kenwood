//! Pure-logic CAT (Computer Aided Transceiver) command codec.
//!
//! This module handles serialization and parsing of the TH-D75's serial
//! command protocol. Commands are ASCII text terminated by carriage return
//! (`\r`), with parameters separated by commas. The protocol layer has no
//! async or I/O dependencies; it operates purely on byte slices.
//!
//! The standard CAT command set is represented as [`Command`] and [`Response`]
//! variants. Use [`serialize`] and [`parse`] for offline wire inspection,
//! capture tooling, and protocol tests. Live radio operations go through the
//! operation-specific methods on [`Radio`](crate::radio::Radio), which add
//! firmware qualification, response correlation, and any required multi-step
//! sequencing. Unverified factory-service mnemonics are deliberately absent;
//! the established MCP entry command remains paired with its dedicated binary
//! programming implementation.

mod aprs;
mod bluetooth;
pub mod codec;
mod control;
mod core;
mod dstar;
mod fields;
mod gps;
mod memory;
pub mod memread;
pub mod programming;
mod scan;
mod sd;
mod tone;
mod user;
mod vfo;

pub use codec::Codec;

use crate::error::ProtocolError;
use crate::types::{
    AfGainLevel, AntennaInput, AprsCallsign, BacklightControl, Band, BandMode, BeaconMode,
    CatChannelRecord, CatMemoryChannelRecord, CurrentMemorySelector, DstarCallsign, DstarSlot,
    DstarSuffix, DvGatewayMode, FilterMode, FilterWidthIndex, FineStep, FirmwareIdentity,
    Frequency, GpsRadioMode, GpsSettings, MemoryChannelAddress, MemoryReadOffset,
    MyPositionSelection, NmeaSentence, NmeaSentences, OperatingMode, PacketDataRate, PowerLevel,
    RadioClock, RadioModel, RadioType, ReadLen, SMeterReading, SerialInformation, SquelchLevel,
    StepSize, TncDataBand, TncMode, TuningMode, UsbAudioOutput, VoxDelay, VoxGain,
};

/// A typed CAT wire command.
///
/// This public enum is the offline protocol AST consumed by [`serialize`]. It
/// is not a generic live-radio execution API: callers should use the matching
/// operation-specific method on [`Radio`](crate::radio::Radio), whose contract
/// can enforce the invariants for that operation.
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
    /// Get tuning mode (VM read).
    ///
    /// Tuning mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    /// Works in any tuning mode. The response is used to update the
    /// [`Radio`](crate::radio::Radio) cached tuning-mode state.
    GetTuningMode {
        /// Target band.
        band: Band,
    },
    /// Set tuning mode (VM write).
    ///
    /// Tuning mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    /// This is an ACTION command that changes the radio's tuning mode.
    /// Prefer [`Radio::tune_channel`](crate::radio::Radio::tune_channel) for
    /// qualified memory recall. Direct frequency tuning is quarantined until
    /// exact FO write/readback behavior is qualified.
    SetTuningMode {
        /// Target band.
        band: Band,
        /// Tuning mode.
        mode: TuningMode,
    },
    /// Get FM radio on/off state (FR read).
    GetFmRadio,

    // === VFO (AG, SQ, SM, MD, FS, FT, SH, UP, RA) ===
    /// Get the global AF gain level (AG bare read).
    ///
    /// Hardware observation: bare `AG\r` returns the global 0-200 gain
    /// level. Band-indexed `AG band\r` returns `?`. Read is bare only.
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
    /// Sends `SQ band,level\r`, where level is 0 through 6.
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
    GetOperatingMode {
        /// Target band.
        band: Band,
    },
    /// Set operating mode (MD write).
    ///
    /// # Tuning-mode requirement
    /// Radio must be in VFO mode on the target band. Setting the
    /// operating mode (FM/DV/NFM/AM) only applies to the current VFO.
    SetOperatingMode {
        /// Target band.
        band: Band,
        /// Operating mode to set.
        mode: OperatingMode,
    },
    /// Get fine step (FS bare read).
    ///
    /// Firmware-verified: FS = Fine Step. Bare `FS\r` returns `FS value`
    /// (single value, no band). Band-indexed reads are not supported.
    GetFineStep,
    /// Get Fine Tune state (FT bare read, no band parameter).
    GetFineTune,
    /// Set Fine Tune state (FT write).
    ///
    /// Fine Tune applies only to AM operation on Band B.
    SetFineTune {
        /// Whether Fine Tune is enabled.
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
        /// Mode-qualified filter width index.
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
    /// Get the single-band or dual-band selection (DL read).
    GetBandMode,
    /// Set the single-band or dual-band selection (DL write).
    SetBandMode {
        /// Band presentation to select.
        mode: BandMode,
    },
    /// Switch the current operating context to receive mode (bare RX action).
    /// The radio acknowledges success by echoing bare `RX`.
    Receive,
    /// Key the transmitter (TX action).
    ///
    /// # Safety
    /// **This transmits on air.** Ensure you are authorized to transmit on
    /// the current frequency, have proper identification, and comply with
    /// all applicable regulations. Use [`Command::Receive`] to return to receive mode.
    /// The radio acknowledges success by echoing bare `TX`.
    Transmit,
    /// Get LCD backlight control mode (LC read).
    GetBacklightControl,
    /// Set LCD backlight control mode (LC write).
    SetBacklightControl {
        /// Backlight mode (0=Manual, 1=On, 2=Auto, 3=Auto DC-IN).
        mode: BacklightControl,
    },
    /// Get USB audio output selection (IO read).
    GetUsbAudioOutput,
    /// Set USB audio output selection (IO write).
    SetUsbAudioOutput {
        /// USB output to select.
        output: UsbAudioOutput,
    },
    /// Get battery level (BL read).
    ///
    /// Returns 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green),
    /// 3=Full (Green), 4=Charging, or the semantically unidentified runtime
    /// state 5. Charging was observed on hardware while USB power was
    /// connected.
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
        /// VOX delay selected from the radio's seven discrete durations.
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

    // === Memory (ME, MR) ===
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
        /// Exact memory-channel address.
        selector: MemoryChannelAddress,
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
        /// Exact memory-channel address.
        selector: MemoryChannelAddress,
    },
    // === TNC / D-STAR / Clock (TN, DC, RT) ===
    /// Get TNC mode (TN bare read).
    ///
    /// Hardware-verified: bare `TN\r` returns `TN mode,data-band`.
    /// Band-indexed `TN band\r` returns `?` (rejected).
    ///
    /// Hardware readback confirms that TN returns TNC mode data (for example,
    /// `TN 0,0`).
    ///
    /// The official TH-D75 grammar defines modes 0 through 2. Firmware
    /// 1.03.AZM adds exact mode 3 for MMDVM/Reflector Terminal operation;
    /// its second field remains the typed TNC data band.
    GetTncMode,
    /// Set TNC mode (TN write).
    ///
    /// Wire format: `TN mode,data_band\r`.
    ///
    /// The official TH-D75 grammar defines modes 0 through 2. Firmware
    /// 1.03.AZM adds exact mode 3 for MMDVM/Reflector Terminal operation;
    /// its second field remains the typed TNC data band.
    SetTncMode {
        /// TNC operating mode (APRS/NAVITRA/KISS/MMDVM).
        mode: TncMode,
        /// TNC data band.
        data_band: TncDataBand,
    },
    /// Get D-STAR callsign data for a slot (DC read).
    ///
    /// Hardware-verified: `DC slot\r` where slot is 1-6.
    /// `DC 0` returns `N` (not available). Slots 1-6 return callsign
    /// data in format `DC slot,callsign,suffix`.
    ///
    /// Hardware readback confirms that DC addresses D-STAR callsign slots.
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
        /// Validated callsign, encoded as exactly eight space-padded bytes.
        callsign: DstarCallsign,
        /// Validated suffix, encoded as exactly four space-padded bytes.
        suffix: DstarSuffix,
    },
    /// Get real-time clock (RT bare read).
    ///
    /// Hardware-verified: bare `RT\r` returns `RT YYMMDDHHmmss`.
    /// Band-indexed `RT band\r` returns `?` (rejected).
    ///
    /// Hardware readback confirms that RT returns the radio's real-time clock.
    GetRealTimeClock,

    // === Scan (SF, BS) ===
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
    GetAntennaInput,
    /// Select the MW/SW receive antenna (BS write).
    SetAntennaInput {
        /// Antenna input to select.
        input: AntennaInput,
    },

    // === APRS (AS, AE, BE, PT, MS, CS) ===
    /// Get the packet-data rate (AS read).
    ///
    /// Returns 0 = 1200 bps, 1 = 9600 bps.
    GetPacketDataRate,
    /// Set the packet-data rate (AS write).
    ///
    /// Values: 0 = 1200 bps, 1 = 9600 bps.
    SetPacketDataRate {
        /// Packet-data rate.
        data_rate: PacketDataRate,
    },

    // === Serial Info (AE) ===
    /// Get serial number and model code (AE read).
    ///
    /// Despite the AE mnemonic (historically "APRS Extended"), this command
    /// returns the radio's serial number and model code.
    GetSerialInfo,
    /// Get beacon TX control mode (PT read).
    GetBeaconMode,
    /// Set beacon TX control mode (PT write).
    SetBeaconMode {
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
    /// Set the APRS My Callsign value (CS write, canonical AX.25 identity).
    SetAprsCallsign {
        /// Validated APRS callsign, optionally including an SSID.
        callsign: AprsCallsign,
    },
    /// Trigger one APRS beacon transmission (BE action).
    ///
    /// This command can transmit over amateur-radio spectrum. The radio
    /// returns `N` when its TNC is not ready for an APRS beacon.
    TransmitAprsBeacon,

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
    /// Get GPS settings (GP read).
    ///
    /// Returns GPS enabled and PC output enabled flags.
    GetGpsSettings,
    /// Set GPS settings (GP write).
    ///
    /// Sets GPS enabled and PC output enabled flags.
    SetGpsSettings {
        /// Complete two-field GPS receiver settings.
        settings: GpsSettings,
    },
    /// Get GPS/Radio mode status (GM bare read).
    ///
    /// # Warning
    /// Only use bare `GM\r` (no parameter). Sending `GM 1\r` **reboots the
    /// radio** into GPS-only mode. This command only supports the bare read.
    GetGpsMode,
    /// Get GPS NMEA sentence enable flags (GS read).
    ///
    /// Returns the validated selection of GGA, GLL, GSA, GSV, RMC, and VTG.
    GetGpsSentences,
    /// Set GPS NMEA sentence enable flags (GS write).
    ///
    /// Sets the validated, nonempty NMEA sentence selection.
    SetGpsSentences {
        /// NMEA sentence types to output.
        sentences: NmeaSentences,
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
    /// Query SD-card presence (SD read).
    ///
    /// MCP programming mode is entered by the distinct private
    /// `0M PROGRAM` command, not by `SD PROGRAM`.
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

    // === Radio type (TY) ===
    /// Get radio type/region code (TY read).
    ///
    /// Returns a typed region and opaque hardware-variant nibble (e.g.,
    /// `TY K,2` for the United States region, variant 2).
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
        /// Exact bounded firmware identity.
        version: FirmwareIdentity,
    },
    /// Power status response (PS).
    PowerStatus {
        /// Whether the radio is on.
        on: bool,
    },
    /// Radio model ID response (ID).
    RadioId {
        /// Exact TH-D75 model identity.
        model: RadioModel,
    },
    /// Power level response (PC).
    PowerLevel {
        /// Band the level is for.
        band: Band,
        /// Current power level.
        level: PowerLevel,
    },
    /// Band response (BC read).
    Band {
        /// Current active band.
        band: Band,
    },
    /// Tuning-mode response (VM).
    ///
    /// Tuning mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    TuningMode {
        /// Band the mode is for.
        band: Band,
        /// Tuning mode.
        mode: TuningMode,
    },
    /// FM radio on/off response (FR).
    FmRadio {
        /// Whether FM radio is enabled.
        enabled: bool,
    },

    // === VFO ===
    /// AF gain response (AG).
    ///
    /// Firmware validation and hardware reads establish the global range
    /// `000` through `200`.
    AfGain {
        /// Gain level (0-200). Global, not per-band.
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
    OperatingMode {
        /// Band the mode is for.
        band: Band,
        /// Current operating mode.
        mode: OperatingMode,
    },
    /// Fine step response (FS).
    ///
    /// Firmware-verified: bare `FS\r` returns `FS value` (single value, no band).
    FineStep {
        /// Current fine step setting.
        step: FineStep,
    },
    /// Fine Tune state response (FT).
    FineTune {
        /// Whether Fine Tune is enabled.
        enabled: bool,
    },
    /// Filter width response (SH).
    FilterWidth {
        /// Mode-qualified filter width index.
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
    /// Single-band or dual-band selection response (DL).
    BandMode {
        /// Current band presentation.
        mode: BandMode,
    },
    /// Frequency down acknowledgement (DW).
    FrequencyDownAck,
    /// Frequency up acknowledgement (UP).
    FrequencyUpAck,
    /// Receive-mode action acknowledgement (RX).
    ReceiveAck,
    /// Transmit-mode action acknowledgement (TX).
    TransmitAck,
    /// LCD backlight control mode response (LC).
    BacklightControl {
        /// Current backlight mode.
        mode: BacklightControl,
    },
    /// USB audio output selection response (IO).
    UsbAudioOutput {
        /// Current USB output selection.
        output: UsbAudioOutput,
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
        /// Current VOX delay selected from the radio's seven discrete durations.
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
        /// Exact memory-channel address returned by the radio.
        selector: MemoryChannelAddress,
        /// Lossless ME record, including split and scan-lockout state.
        record: CatMemoryChannelRecord,
    },
    /// Memory recall echo response (MR write acknowledgment).
    ///
    /// When `MR band,channel` is sent as a write/recall, the radio echoes
    /// back the band and channel as acknowledgment.
    MemoryRecallAck {
        /// Target band.
        band: Band,
        /// Exact memory-channel address echoed by the radio.
        selector: MemoryChannelAddress,
    },
    /// Current memory selector response (MR read).
    ///
    /// Hardware-verified: an `MR band\r` request returns only the selected
    /// memory (`MR 021`, `MR L00`, `MR Pri`, and so on). The request band is
    /// not present in this frame and must not be inferred from the selector.
    CurrentChannel {
        /// Selector returned by the bandless MR response.
        selector: CurrentMemorySelector,
    },
    // === TNC / D-STAR / Clock ===
    /// TNC mode response (TN).
    ///
    /// Hardware-verified: bare `TN\r` returns `TN mode,data_band`.
    /// Example: `TN 0,0`.
    ///
    /// The official TH-D75 grammar defines modes 0 through 2. Firmware
    /// 1.03.AZM adds exact mode 3 for MMDVM/Reflector Terminal operation;
    /// its second field remains the typed TNC data band.
    TncMode {
        /// TNC operating mode.
        mode: TncMode,
        /// TNC data band.
        data_band: TncDataBand,
    },
    /// D-STAR callsign data response (DC).
    ///
    /// Hardware-verified: `DC slot\r` returns `DC slot,callsign,suffix`.
    /// Example: `DC 1,KQ4NIT  ,D75A`.
    DstarCallsign {
        /// Callsign slot (1-6).
        slot: DstarSlot,
        /// Validated callsign with wire padding removed.
        callsign: DstarCallsign,
        /// Validated callsign suffix with wire padding removed.
        suffix: DstarSuffix,
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
    AntennaInput {
        /// Current receive antenna input.
        input: AntennaInput,
    },

    // === APRS and A-prefixed identity ===
    /// Packet-data rate response (AS).
    ///
    /// Values: 0 = 1200 bps, 1 = 9600 bps.
    PacketDataRate {
        /// Current packet-data rate.
        data_rate: PacketDataRate,
    },
    /// Serial number and model code response (AE).
    ///
    /// Despite the AE mnemonic (historically "APRS Extended"), this command
    /// returns the radio's serial number and model code.
    /// Format: `serial,model_code` (e.g., `C3C10368,K01`).
    SerialInformation(SerialInformation),
    /// Beacon TX control mode response (PT).
    BeaconMode {
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
        /// Current APRS station callsign, or `None` when the slot is empty.
        callsign: Option<AprsCallsign>,
    },
    /// Bare acknowledgement for an accepted APRS beacon transmit action (BE).
    AprsBeaconTransmitAck,

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
    /// GPS settings response (GP).
    ///
    /// Two boolean fields: GPS enabled and PC output enabled.
    /// Format: `gps_enabled,pc_output` (e.g., `0,0`).
    GpsSettings {
        /// Complete two-field GPS receiver settings.
        settings: GpsSettings,
    },
    /// GPS/Radio mode status response (GM).
    ///
    /// 0 = Normal transceiver mode, 1 = GPS receiver mode.
    /// Only the wire values 0 and 1 are valid.
    GpsMode {
        /// GPS/Radio operating mode.
        mode: GpsRadioMode,
    },
    /// GPS NMEA sentence enable flags response (GS).
    ///
    /// Validated, nonempty set of NMEA sentence types to output.
    GpsSentences {
        /// NMEA sentence types selected by the radio.
        sentences: NmeaSentences,
    },

    // === Bluetooth ===
    /// Bluetooth state response (BT).
    Bluetooth {
        /// Whether Bluetooth is enabled.
        enabled: bool,
    },

    // === SD ===
    /// SD-card presence response (SD).
    ///
    /// MCP programming mode uses the distinct private `0M PROGRAM` command.
    SdCard {
        /// Whether an SD card is present.
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
        /// The bounded, nonempty decoded bytes.
        bytes: memread::MemoryReadData,
    },

    // === Extra (TY) ===
    /// Radio type/region code response (TY).
    ///
    /// Returns the radio's typed region and opaque hardware-variant nibble.
    /// Example: `TY K,2` (K = United States region, variant 2).
    RadioType(RadioType),

    // === Special ===
    /// Error response (`?\r`).
    Error,
    /// Not available response (`N\r`): command not supported in current mode.
    NotAvailableInCurrentMode,
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
        Command::GetTuningMode { .. } | Command::SetTuningMode { .. } => "VM",
        Command::GetFmRadio => "FR",
        Command::GetAfGain | Command::SetAfGain { .. } => "AG",
        Command::GetSquelch { .. } | Command::SetSquelch { .. } => "SQ",
        Command::GetSmeter { .. } => "SM",
        Command::GetOperatingMode { .. } | Command::SetOperatingMode { .. } => "MD",
        Command::GetFineStep => "FS",
        Command::GetFineTune | Command::SetFineTune { .. } => "FT",
        Command::GetFilterWidth { .. } | Command::SetFilterWidth { .. } => "SH",
        Command::FrequencyUp => "UP",
        Command::FrequencyDown => "DW",
        Command::GetAttenuator { .. } | Command::SetAttenuator { .. } => "RA",
        Command::GetAutoInfo | Command::SetAutoInfo { .. } => "AI",
        Command::GetBusy { .. } => "BY",
        Command::GetBandMode | Command::SetBandMode { .. } => "DL",
        Command::Receive => "RX",
        Command::Transmit => "TX",
        Command::GetBacklightControl | Command::SetBacklightControl { .. } => "LC",
        Command::GetUsbAudioOutput | Command::SetUsbAudioOutput { .. } => "IO",
        Command::GetBatteryLevel => "BL",
        Command::GetVoxDelay | Command::SetVoxDelay { .. } => "VD",
        Command::GetVoxGain | Command::SetVoxGain { .. } => "VG",
        Command::GetVox | Command::SetVox { .. } => "VX",
        Command::GetCurrentChannel { .. } | Command::RecallMemoryChannel { .. } => "MR",
        Command::GetMemoryChannel { .. } => "ME",
        Command::GetTncMode | Command::SetTncMode { .. } => "TN",
        Command::GetDstarCallsign { .. } | Command::SetDstarCallsign { .. } => "DC",
        Command::GetRealTimeClock => "RT",
        Command::GetStepSize { .. } | Command::SetStepSize { .. } => "SF",
        Command::GetAntennaInput | Command::SetAntennaInput { .. } => "BS",
        Command::GetPacketDataRate | Command::SetPacketDataRate { .. } => "AS",
        Command::GetSerialInfo => "AE",
        Command::GetBeaconMode | Command::SetBeaconMode { .. } => "PT",
        Command::GetMyPositionSelection | Command::SetMyPositionSelection { .. } => "MS",
        Command::GetAprsCallsign | Command::SetAprsCallsign { .. } => "CS",
        Command::TransmitAprsBeacon => "BE",
        Command::GetDstarSlot | Command::SetDstarSlot { .. } => "DS",
        Command::GetGateway => "GW",
        Command::GetGpsSettings | Command::SetGpsSettings { .. } => "GP",
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
        Command::GetTuningMode { band } => format!("VM {}", u8::from(*band)),
        Command::SetTuningMode { band, mode } => {
            format!("VM {},{}", u8::from(*band), u8::from(*mode))
        }
        Command::GetFmRadio => "FR".to_owned(),

        // VFO
        Command::GetAfGain => "AG".to_owned(),
        Command::SetAfGain { level } => {
            // The radio expects bare `AG AAA\r`. Band-indexed
            // `AG band,level` is rejected with `?`; the exact global domain
            // is 000-200.
            format!("AG {:03}", level.as_raw())
        }
        Command::GetSquelch { band } => format!("SQ {}", u8::from(*band)),
        Command::SetSquelch { band, level } => {
            format!("SQ {},{}", u8::from(*band), level.as_raw())
        }
        Command::GetSmeter { band } => format!("SM {}", u8::from(*band)),
        Command::GetOperatingMode { band } => format!("MD {}", u8::from(*band)),
        Command::SetOperatingMode { band, mode } => {
            format!("MD {},{}", u8::from(*band), u8::from(*mode))
        }
        Command::GetFineStep => "FS".to_owned(),
        Command::GetFineTune => "FT".to_owned(),
        Command::SetFineTune { enabled } => format!("FT {}", u8::from(*enabled)),
        Command::GetFilterWidth { mode } => format!("SH {}", u8::from(*mode)),
        Command::SetFilterWidth { width } => {
            format!("SH {},{}", u8::from(width.mode()), width.as_raw())
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
        Command::GetBandMode => "DL".to_owned(),
        Command::SetBandMode { mode } => format!("DL {}", mode.as_raw()),
        Command::Receive => "RX".to_owned(),
        Command::Transmit => "TX".to_owned(),
        Command::GetBacklightControl => "LC".to_owned(),
        Command::SetBacklightControl { mode } => format!("LC {}", u8::from(*mode)),
        Command::GetUsbAudioOutput => "IO".to_owned(),
        Command::SetUsbAudioOutput { output } => format!("IO {}", u8::from(*output)),
        Command::GetBatteryLevel => "BL".to_owned(),
        Command::GetVoxDelay => "VD".to_owned(),
        Command::SetVoxDelay { delay } => format!("VD {}", delay.as_raw()),
        Command::GetVoxGain => "VG".to_owned(),
        Command::SetVoxGain { gain } => format!("VG {}", gain.as_raw()),
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
        // TNC / D-STAR / Clock
        Command::GetTncMode => "TN".to_owned(),
        Command::SetTncMode { mode, data_band } => {
            format!("TN {},{}", u8::from(*mode), u8::from(*data_band))
        }
        Command::GetDstarCallsign { slot } => format!("DC {}", slot.as_raw()),
        Command::SetDstarCallsign {
            slot,
            callsign,
            suffix,
        } => format!(
            "DC {},{:<8},{:<4}",
            slot.as_raw(),
            callsign.as_str(),
            suffix.as_str()
        ),
        Command::GetRealTimeClock => "RT".to_owned(),

        // Scan
        Command::GetStepSize { band } => format!("SF {}", u8::from(*band)),
        Command::SetStepSize { band, step } => {
            format!("SF {},{:X}", u8::from(*band), u8::from(*step))
        }
        Command::GetAntennaInput => "BS".to_owned(),
        Command::SetAntennaInput { input } => {
            format!("BS {}", input.as_raw())
        }

        // APRS
        Command::GetPacketDataRate => "AS".to_owned(),
        Command::SetPacketDataRate { data_rate } => format!("AS {}", u8::from(*data_rate)),
        Command::GetSerialInfo => "AE".to_owned(),
        Command::GetBeaconMode => "PT".to_owned(),
        Command::SetBeaconMode { mode } => format!("PT {}", u8::from(*mode)),
        Command::GetMyPositionSelection => "MS".to_owned(),
        Command::SetMyPositionSelection { selection } => {
            format!("MS {}", u8::from(*selection))
        }
        Command::GetAprsCallsign => "CS".to_owned(),
        Command::SetAprsCallsign { callsign } => format!("CS {callsign}"),
        Command::TransmitAprsBeacon => "BE".to_owned(),

        // D-STAR
        Command::GetDstarSlot => "DS".to_owned(),
        Command::SetDstarSlot { slot } => format!("DS {}", slot.as_raw()),
        Command::GetGateway => "GW".to_owned(),

        // GPS
        Command::GetGpsSettings => "GP".to_owned(),
        Command::SetGpsSettings { settings } => format!(
            "GP {},{}",
            u8::from(settings.enabled()),
            u8::from(settings.pc_output())
        ),
        Command::GetGpsMode => "GM".to_owned(),
        Command::GetGpsSentences => "GS".to_owned(),
        Command::SetGpsSentences { sentences } => format!(
            "GS {},{},{},{},{},{}",
            u8::from(sentences.contains(NmeaSentence::Gga)),
            u8::from(sentences.contains(NmeaSentence::Gll)),
            u8::from(sentences.contains(NmeaSentence::Gsa)),
            u8::from(sentences.contains(NmeaSentence::Gsv)),
            u8::from(sentences.contains(NmeaSentence::Rmc)),
            u8::from(sentences.contains(NmeaSentence::Vtg))
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

    tracing::trace!(wire = %body, "serialized wire format");
    let mut bytes = body.into_bytes();
    bytes.push(b'\r');
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
    if frame.len() > Codec::MAX_BUFFERED_BYTES {
        return Err(ProtocolError::FrameTooLong {
            maximum: Codec::MAX_BUFFERED_BYTES,
            buffered: 0,
            incoming: frame.len(),
        });
    }

    // Error response
    if frame == b"?" {
        tracing::debug!(mnemonic = "?", "parsing error response");
        return Ok(Response::Error);
    }

    // Not-available response
    if frame == b"N" {
        tracing::debug!(mnemonic = "N", "parsing not-available response");
        return Ok(Response::NotAvailableInCurrentMode);
    }

    if frame.iter().any(|byte| !(b' '..=b'~').contains(byte)) {
        tracing::warn!(
            ?frame,
            "CAT frame contains a non-printable or non-ASCII byte"
        );
        return Err(ProtocolError::MalformedFrame(frame.to_vec()));
    }

    let frame_str = std::str::from_utf8(frame).map_err(|_| {
        tracing::warn!("failed to parse frame as UTF-8");
        ProtocolError::MalformedFrame(frame.to_vec())
    })?;

    // Extract the two-character mnemonic.
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
    if !mnemonic
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        tracing::warn!(frame = %frame_str, "frame mnemonic contains an invalid byte");
        return Err(ProtocolError::MalformedFrame(frame.to_vec()));
    }
    tracing::debug!(mnemonic = %mnemonic, "parsing response");

    // A payload is separated from its two-byte mnemonic by exactly one ASCII
    // space. Do not normalize missing separators, tabs, or repeated spaces:
    // doing so can turn malformed wire data into a different valid response.
    let payload = match frame.get(2..) {
        Some([]) => "",
        Some([b' ', payload @ ..]) => std::str::from_utf8(payload)
            .map_err(|_| ProtocolError::MalformedFrame(frame.to_vec()))?,
        Some(_) | None => return Err(ProtocolError::MalformedFrame(frame.to_vec())),
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
        let Response::RadioType(radio_type) = r else {
            return Err(format!("expected RadioType, got {r:?}").into());
        };
        assert_eq!(radio_type.region(), crate::types::RadioRegion::UnitedStates);
        assert_eq!(radio_type.hardware_variant().as_raw(), 2);
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
            callsign: DstarCallsign::new("KQ4NIT")?,
            suffix: DstarSuffix::new("D75A")?,
        });
        assert_eq!(bytes, b"DC 1,KQ4NIT  ,D75A\r");
        Ok(())
    }

    #[test]
    fn serialize_set_antenna_input() {
        let bytes = serialize(&Command::SetAntennaInput {
            input: AntennaInput::InternalBar,
        });
        assert_eq!(bytes, b"BS 1\r");
    }

    #[test]
    fn serialize_set_tnc_mode() {
        let bytes = serialize(&Command::SetTncMode {
            mode: TncMode::Mmdvm,
            data_band: TncDataBand::A,
        });
        assert_eq!(bytes, b"TN 3,0\r");
    }

    #[test]
    fn all_mnemonics_recognized() -> TestResult {
        // All standard mnemonics whose response semantics are identified.
        let mnemonics = [
            "AI", "AG", "BC", "BY", "DL", "DW", "ME", "MR", "PC", "RX", "SQ", "SH", "TX", "UP",
            "VM", "FQ", "FO", "PS", "FV", "ID", "CS", "TN", "BL", "GP", "GM", "SM", "RA", "BT",
            "FS", "FT", "MD", "SF", "VD", "VG", "VX", "IO", "BS", "LC", "GS", "MS", "PT", "AS",
            "DC", "DS", "RT", "FR", "GW", "SD", "AE", "BE",
            // Extra mnemonics not in main dispatch table
            "TY",
        ];
        assert_eq!(mnemonics.len(), 51);
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
