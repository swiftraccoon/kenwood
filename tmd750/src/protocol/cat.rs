//! CAT commands and reply lines.
//!
//! A request is the mnemonic, an optional space and argument list, and a
//! carriage return; a reply is the mnemonic, a space and the payload,
//! terminated the same way. A lone `?` rejects the command and a lone `N`
//! reports that it is unavailable in the current state. Every command here
//! except [`Command::PowerOff`] was sent to firmware 1.02 over the panel USB
//! connector and its reply read; a setter's reply echoes the accepted setting
//! in the read form.

use crate::error::{CommandName, ProtocolError, ReplyField, ReplyFieldError};
use crate::types::{
    AmHighCut, AprsCallsign, BacklightControl, Band, BandControl, BandDisplay, BeaconMethod,
    CHANNEL_FIELD_COUNT, CatChannelRecord, CatMemoryChannelRecord, CurrentMemorySelector,
    DstarCallsignEntry, DstarCallsignText, DstarSlot, DvGatewayMode, FirmwareIdentity, Frequency,
    GpsSettings, MemoryChannelAddress, MyPositionSelection, NmeaSentences, OperatingMode,
    PacketDataRate, PowerLevel, RadioModel, RadioType, RealTimeClock, SMeterReading,
    SelectableMode, SelectableTncMode, SerialInformation, SquelchLevel, StepSize, TncMode,
    TuningMode, VoxDelay, VoxGain, VoxMode,
};

use super::channel::{parse_channel_fields, parse_memory_channel};
use super::fields::{band, boolean, decimal_u8, field_error, split_exact};

/// Byte that terminates every CAT line.
pub const LINE_TERMINATOR: u8 = b'\r';

/// Typed CAT commands this crate sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `ID`: model string.
    Identify,
    /// `FV`: firmware identity.
    FirmwareVersion,
    /// `TY`: complete opaque radio-type payload.
    RadioType,
    /// `AE`: serial number and model code.
    SerialInformation,
    /// `PS`: power status.
    PowerStatus,
    /// `RT`: the radio's clock.
    RealTimeClock,
    /// `MD <band>`: read the demodulation mode.
    GetOperatingMode {
        /// Band to query.
        band: Band,
    },
    /// `MD <band>,<mode>`: select a demodulation mode.
    SetOperatingMode {
        /// Band to change.
        band: Band,
        /// Mode to select.
        mode: SelectableMode,
    },
    /// `GW`: read the persistent DV Gateway mode.
    GetGatewayMode,
    /// `FQ <band>`: read the frequency.
    GetFrequency {
        /// Band to query.
        band: Band,
    },
    /// `FQ <band>,<hz>`: tune the VFO.
    SetFrequency {
        /// Band to tune.
        band: Band,
        /// Frequency to select.
        frequency: Frequency,
    },
    /// `FO <band>`: read the band's complete channel record.
    GetChannelRecord {
        /// Band to query.
        band: Band,
    },
    /// `FO <band>,<18 fields>`: replace the band's complete channel record.
    SetChannelRecord {
        /// Band to change.
        band: Band,
        /// Record to apply.
        record: CatChannelRecord,
    },
    /// `PC <band>`: read the transmit power.
    GetPowerLevel {
        /// Band to query.
        band: Band,
    },
    /// `PC <band>,<level>`: select the transmit power.
    SetPowerLevel {
        /// Band to change.
        band: Band,
        /// Level to select.
        level: PowerLevel,
    },
    /// `VM <band>`: read the tuning mode.
    GetTuningMode {
        /// Band to query.
        band: Band,
    },
    /// `VM <band>,<mode>`: select the tuning mode.
    SetTuningMode {
        /// Band to change.
        band: Band,
        /// Mode to select.
        mode: TuningMode,
    },
    /// `SQ <band>`: read the squelch level.
    GetSquelch {
        /// Band to query.
        band: Band,
    },
    /// `SQ <band>,<level>`: set the squelch level.
    SetSquelch {
        /// Band to change.
        band: Band,
        /// Level to set.
        level: SquelchLevel,
    },
    /// `SM <band>`: read the signal strength.
    GetSmeter {
        /// Band to query.
        band: Band,
    },
    /// `BY <band>`: read whether the squelch is open.
    GetBusy {
        /// Band to query.
        band: Band,
    },
    /// `RA <band>`: read the attenuator.
    GetAttenuator {
        /// Band to query.
        band: Band,
    },
    /// `RA <band>,<0|1>`: switch the attenuator.
    SetAttenuator {
        /// Band to change.
        band: Band,
        /// Whether the attenuator is on.
        enabled: bool,
    },
    /// `SF <band>`: read the tuning step.
    GetStepSize {
        /// Band to query.
        band: Band,
    },
    /// `SF <band>,<step>`: select the tuning step.
    SetStepSize {
        /// Band to change.
        band: Band,
        /// Step to select.
        step: StepSize,
    },
    /// `SH 0`: read the AM high-cut filter.
    GetAmHighCut,
    /// `SH 0,<cut>`: select the AM high-cut filter.
    SetAmHighCut {
        /// Cutoff to select.
        cut: AmHighCut,
    },
    /// `UP`: step the control band up.
    FrequencyUp,
    /// `DW`: step the control band down.
    FrequencyDown,
    /// `MR <band>`: read the selected memory channel.
    GetCurrentChannel {
        /// Band to query.
        band: Band,
    },
    /// `MR <band>,<address>`: recall a memory channel.
    RecallMemoryChannel {
        /// Band to change.
        band: Band,
        /// Channel to recall.
        address: MemoryChannelAddress,
    },
    /// `ME <address>`: read a stored memory channel.
    GetMemoryChannel {
        /// Channel to read.
        address: MemoryChannelAddress,
    },
    /// `ME <address>,<20 fields>`: store a memory channel.
    WriteMemoryChannel {
        /// Channel to store.
        address: MemoryChannelAddress,
        /// Record to store.
        record: CatMemoryChannelRecord,
    },
    /// `ME <address>,`: clear a memory channel.
    ClearMemoryChannel {
        /// Channel to clear.
        address: MemoryChannelAddress,
    },
    /// `DC <slot>`: read a D-STAR MY callsign slot.
    GetDstarCallsign {
        /// Slot to read.
        slot: DstarSlot,
    },
    /// `DC <slot>,<callsign>,<memo>`: store a D-STAR MY callsign slot.
    ///
    /// Empty callsign and memo clear the slot.
    SetDstarCallsign {
        /// Slot to store and the callsign and memo to write into it.
        entry: DstarCallsignEntry,
    },
    /// `BC`: read the control and PTT bands.
    GetBandControl,
    /// `BC <control>,<ptt>`: select the control and PTT bands.
    SetBandControl {
        /// Roles to select.
        roles: BandControl,
    },
    /// `DL`: read the dual or single band display.
    GetBandDisplay,
    /// `DL <0|1>`: select the dual or single band display.
    SetBandDisplay {
        /// Display to select.
        display: BandDisplay,
    },
    /// `DS`: read the selected D-STAR MY callsign slot.
    GetDstarSlot,
    /// `DS <slot>`: select a D-STAR MY callsign slot.
    SetDstarSlot {
        /// Slot to select.
        slot: DstarSlot,
    },
    /// `LC`: read the panel lighting setting.
    GetBacklightControl,
    /// `LC <value>`: select the panel lighting setting.
    SetBacklightControl {
        /// Value to select.
        control: BacklightControl,
    },
    /// `MS`: read the APRS position source.
    GetMyPositionSelection,
    /// `MS <selection>`: select the APRS position source.
    SetMyPositionSelection {
        /// Source to select.
        selection: MyPositionSelection,
    },
    /// `AS`: read the packet data speed.
    GetPacketDataRate,
    /// `AS <rate>`: select the packet data speed.
    SetPacketDataRate {
        /// Speed to select.
        rate: PacketDataRate,
    },
    /// `PT`: read the APRS beacon method.
    GetBeaconMethod,
    /// `PT <method>`: select the APRS beacon method.
    SetBeaconMethod {
        /// Method to select.
        method: BeaconMethod,
    },
    /// `TN`: read the TNC mode and data band.
    GetTncMode,
    /// `TN <mode>,<band>`: select the TNC mode and its data band.
    SetTncMode {
        /// Mode to select.
        mode: SelectableTncMode,
        /// Band the TNC sends and receives on.
        data_band: Band,
    },
    /// `VD`: read the VOX delay.
    GetVoxDelay,
    /// `VD <index>`: select the VOX delay.
    SetVoxDelay {
        /// Delay to select.
        delay: VoxDelay,
    },
    /// `VG`: read the VOX gain.
    GetVoxGain,
    /// `VG <gain>`: select the VOX gain.
    SetVoxGain {
        /// Gain to select.
        gain: VoxGain,
    },
    /// `VX`: read the VOX state.
    GetVox,
    /// `VX <0|1>`: switch VOX.
    SetVox {
        /// Whether VOX is on.
        enabled: bool,
    },
    /// `GP`: read the GPS receiver settings.
    GetGpsSettings,
    /// `GP <gps>,<pc>`: select the GPS receiver settings.
    SetGpsSettings {
        /// Settings to select.
        settings: GpsSettings,
    },
    /// `GS`: read the NMEA sentence selection.
    GetGpsSentences,
    /// `GS <six flags>`: select the NMEA sentences.
    SetGpsSentences {
        /// Sentences to select.
        sentences: NmeaSentences,
    },
    /// `BT`: read whether Bluetooth is on.
    GetBluetooth,
    /// `BT <0|1>`: switch Bluetooth.
    SetBluetooth {
        /// Whether Bluetooth is on.
        enabled: bool,
    },
    /// `CS`: read the APRS My Callsign.
    GetAprsCallsign,
    /// `CS <callsign>`: set the APRS My Callsign.
    SetAprsCallsign {
        /// Callsign to write.
        callsign: AprsCallsign,
    },
    /// `TX`: key the PTT band's transmitter.
    Transmit,
    /// `RX`: release the transmitter.
    Receive,
    /// `BE`: transmit one APRS beacon.
    SendAprsBeacon,
    /// `PS 0`: switch the radio off.
    PowerOff,
}

impl Command {
    /// The two-letter mnemonic.
    #[must_use]
    pub const fn mnemonic(&self) -> &'static str {
        self.name().as_str()
    }

    /// Typed command label used by reply and exchange failures.
    #[must_use]
    pub const fn name(&self) -> CommandName {
        match *self {
            Self::Identify => CommandName::Id,
            Self::FirmwareVersion => CommandName::Fv,
            Self::RadioType => CommandName::Ty,
            Self::SerialInformation => CommandName::Ae,
            Self::PowerStatus | Self::PowerOff => CommandName::Ps,
            Self::RealTimeClock => CommandName::Rt,
            Self::GetOperatingMode { .. } | Self::SetOperatingMode { .. } => CommandName::Md,
            Self::GetGatewayMode => CommandName::Gw,
            Self::GetFrequency { .. } | Self::SetFrequency { .. } => CommandName::Fq,
            Self::GetChannelRecord { .. } | Self::SetChannelRecord { .. } => CommandName::Fo,
            Self::GetPowerLevel { .. } | Self::SetPowerLevel { .. } => CommandName::Pc,
            Self::GetTuningMode { .. } | Self::SetTuningMode { .. } => CommandName::Vm,
            Self::GetSquelch { .. } | Self::SetSquelch { .. } => CommandName::Sq,
            Self::GetSmeter { .. } => CommandName::Sm,
            Self::GetBusy { .. } => CommandName::By,
            Self::GetAttenuator { .. } | Self::SetAttenuator { .. } => CommandName::Ra,
            Self::GetStepSize { .. } | Self::SetStepSize { .. } => CommandName::Sf,
            Self::GetAmHighCut | Self::SetAmHighCut { .. } => CommandName::Sh,
            Self::FrequencyUp => CommandName::Up,
            Self::FrequencyDown => CommandName::Dw,
            Self::GetCurrentChannel { .. } | Self::RecallMemoryChannel { .. } => CommandName::Mr,
            Self::GetMemoryChannel { .. }
            | Self::WriteMemoryChannel { .. }
            | Self::ClearMemoryChannel { .. } => CommandName::Me,
            Self::GetDstarCallsign { .. } | Self::SetDstarCallsign { .. } => CommandName::Dc,
            Self::GetBandControl | Self::SetBandControl { .. } => CommandName::Bc,
            Self::GetBandDisplay | Self::SetBandDisplay { .. } => CommandName::Dl,
            Self::GetDstarSlot | Self::SetDstarSlot { .. } => CommandName::Ds,
            Self::GetBacklightControl | Self::SetBacklightControl { .. } => CommandName::Lc,
            Self::GetMyPositionSelection | Self::SetMyPositionSelection { .. } => CommandName::Ms,
            Self::GetPacketDataRate | Self::SetPacketDataRate { .. } => CommandName::As,
            Self::GetBeaconMethod | Self::SetBeaconMethod { .. } => CommandName::Pt,
            Self::GetTncMode | Self::SetTncMode { .. } => CommandName::Tn,
            Self::GetVoxDelay | Self::SetVoxDelay { .. } => CommandName::Vd,
            Self::GetVoxGain | Self::SetVoxGain { .. } => CommandName::Vg,
            Self::GetVox | Self::SetVox { .. } => CommandName::Vx,
            Self::GetGpsSettings | Self::SetGpsSettings { .. } => CommandName::Gp,
            Self::GetGpsSentences | Self::SetGpsSentences { .. } => CommandName::Gs,
            Self::GetBluetooth | Self::SetBluetooth { .. } => CommandName::Bt,
            Self::GetAprsCallsign | Self::SetAprsCallsign { .. } => CommandName::Cs,
            Self::Transmit => CommandName::Tx,
            Self::Receive => CommandName::Rx,
            Self::SendAprsBeacon => CommandName::Be,
        }
    }

    /// Whether the command changes radio state.
    #[must_use]
    pub const fn is_write(&self) -> bool {
        matches!(
            self,
            Self::SetOperatingMode { .. }
                | Self::SetFrequency { .. }
                | Self::SetChannelRecord { .. }
                | Self::SetPowerLevel { .. }
                | Self::SetTuningMode { .. }
                | Self::SetSquelch { .. }
                | Self::SetAttenuator { .. }
                | Self::SetStepSize { .. }
                | Self::SetAmHighCut { .. }
                | Self::FrequencyUp
                | Self::FrequencyDown
                | Self::RecallMemoryChannel { .. }
                | Self::WriteMemoryChannel { .. }
                | Self::ClearMemoryChannel { .. }
                | Self::SetBandControl { .. }
                | Self::SetBandDisplay { .. }
                | Self::SetDstarCallsign { .. }
                | Self::SetDstarSlot { .. }
                | Self::SetBacklightControl { .. }
                | Self::SetMyPositionSelection { .. }
                | Self::SetPacketDataRate { .. }
                | Self::SetBeaconMethod { .. }
                | Self::SetVoxDelay { .. }
                | Self::SetVoxGain { .. }
                | Self::SetGpsSettings { .. }
                | Self::SetGpsSentences { .. }
                | Self::SetBluetooth { .. }
                | Self::SetAprsCallsign { .. }
                | Self::SetTncMode { .. }
                | Self::SetVox { .. }
                | Self::Transmit
                | Self::Receive
                | Self::SendAprsBeacon
                | Self::PowerOff
        )
    }

    /// The bytes to send.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mnemonic = self.mnemonic();
        let argument = match *self {
            Self::Identify
            | Self::FirmwareVersion
            | Self::RadioType
            | Self::SerialInformation
            | Self::PowerStatus
            | Self::RealTimeClock
            | Self::GetGatewayMode
            | Self::FrequencyUp
            | Self::FrequencyDown
            | Self::GetBandControl
            | Self::GetBandDisplay
            | Self::GetDstarSlot
            | Self::GetBacklightControl
            | Self::GetMyPositionSelection
            | Self::GetPacketDataRate
            | Self::GetBeaconMethod
            | Self::GetTncMode
            | Self::GetVoxDelay
            | Self::GetVoxGain
            | Self::GetVox
            | Self::GetGpsSettings
            | Self::GetGpsSentences
            | Self::GetBluetooth
            | Self::GetAprsCallsign
            | Self::Transmit
            | Self::Receive
            | Self::SendAprsBeacon => None,
            Self::GetOperatingMode { band }
            | Self::GetFrequency { band }
            | Self::GetChannelRecord { band }
            | Self::GetPowerLevel { band }
            | Self::GetTuningMode { band }
            | Self::GetSquelch { band }
            | Self::GetSmeter { band }
            | Self::GetBusy { band }
            | Self::GetAttenuator { band }
            | Self::GetStepSize { band }
            | Self::GetCurrentChannel { band } => Some(u8::from(band).to_string()),
            Self::SetOperatingMode { band, mode } => Some(format!(
                "{},{}",
                u8::from(band),
                u8::from(OperatingMode::from(mode))
            )),
            Self::SetFrequency { band, frequency } => {
                Some(format!("{},{}", u8::from(band), frequency.to_wire_string()))
            }
            Self::SetChannelRecord { band, ref record } => {
                Some(format!("{},{}", u8::from(band), record.to_wire_fields()))
            }
            Self::SetPowerLevel { band, level } => {
                Some(format!("{},{}", u8::from(band), level.as_raw()))
            }
            Self::SetTuningMode { band, mode } => {
                Some(format!("{},{}", u8::from(band), mode.as_raw()))
            }
            Self::SetSquelch { band, level } => {
                Some(format!("{},{}", u8::from(band), level.as_raw()))
            }
            Self::SetAttenuator { band, enabled } => {
                Some(format!("{},{}", u8::from(band), u8::from(enabled)))
            }
            Self::SetStepSize { band, step } => {
                Some(format!("{},{}", u8::from(band), step.wire_char()))
            }
            Self::GetAmHighCut
            | Self::SetAmHighCut { .. }
            | Self::RecallMemoryChannel { .. }
            | Self::GetMemoryChannel { .. }
            | Self::WriteMemoryChannel { .. }
            | Self::ClearMemoryChannel { .. }
            | Self::GetDstarCallsign { .. }
            | Self::SetDstarCallsign { .. }
            | Self::SetDstarSlot { .. }
            | Self::SetBandControl { .. }
            | Self::SetBandDisplay { .. }
            | Self::SetBacklightControl { .. }
            | Self::SetMyPositionSelection { .. }
            | Self::SetPacketDataRate { .. }
            | Self::SetBeaconMethod { .. }
            | Self::SetVoxDelay { .. }
            | Self::SetVoxGain { .. }
            | Self::SetGpsSettings { .. }
            | Self::SetGpsSentences { .. }
            | Self::SetBluetooth { .. }
            | Self::SetAprsCallsign { .. }
            | Self::SetTncMode { .. }
            | Self::SetVox { .. }
            | Self::PowerOff => self.memory_and_global_argument(),
        };
        let mut bytes = argument.map_or_else(
            || mnemonic.as_bytes().to_vec(),
            |argument| format!("{mnemonic} {argument}").into_bytes(),
        );
        bytes.push(LINE_TERMINATOR);
        bytes
    }

    /// Argument text of the memory and global commands; `None` for the rest.
    fn memory_and_global_argument(&self) -> Option<String> {
        match *self {
            Self::GetAmHighCut | Self::PowerOff => Some("0".to_owned()),
            Self::SetAmHighCut { cut } => Some(format!("0,{}", cut.as_raw())),
            Self::RecallMemoryChannel { band, address } => {
                Some(format!("{},{}", u8::from(band), address.to_wire_string()))
            }
            Self::GetMemoryChannel { address } => Some(address.to_wire_string()),
            Self::WriteMemoryChannel {
                address,
                ref record,
            } => Some(format!(
                "{},{}",
                address.to_wire_string(),
                record.to_wire_fields()
            )),
            Self::ClearMemoryChannel { address } => Some(format!("{},", address.to_wire_string())),
            Self::GetDstarCallsign { slot } | Self::SetDstarSlot { slot } => {
                Some(slot.as_raw().to_string())
            }
            Self::SetDstarCallsign { ref entry } => Some(format!(
                "{},{},{}",
                entry.slot.as_raw(),
                entry.callsign(),
                entry.memo()
            )),
            Self::SetBandControl { roles } => Some(format!(
                "{},{}",
                u8::from(roles.control),
                u8::from(roles.ptt)
            )),
            Self::SetBandDisplay { display } => Some(display.as_raw().to_string()),
            Self::SetBacklightControl { control } => Some(control.as_raw().to_string()),
            Self::SetMyPositionSelection { selection } => Some(selection.as_raw().to_string()),
            Self::SetPacketDataRate { rate } => Some(rate.as_raw().to_string()),
            Self::SetBeaconMethod { method } => Some(method.as_raw().to_string()),
            Self::SetVoxDelay { delay } => Some(delay.as_raw().to_string()),
            Self::SetVoxGain { gain } => Some(gain.as_raw().to_string()),
            Self::SetGpsSettings { settings } => Some(format!(
                "{},{}",
                u8::from(settings.gps_enabled),
                u8::from(settings.pc_output)
            )),
            Self::SetGpsSentences { sentences } => Some(
                sentences
                    .to_flags()
                    .map(|flag| u8::from(flag).to_string())
                    .join(","),
            ),
            Self::SetBluetooth { enabled } | Self::SetVox { enabled } => {
                Some(u8::from(enabled).to_string())
            }
            Self::SetAprsCallsign { ref callsign } => Some(callsign.as_str().to_owned()),
            Self::SetTncMode { mode, data_band } => {
                Some(format!("{},{}", mode.as_raw(), u8::from(data_band)))
            }
            _ => None,
        }
    }

    /// Whether `response` answers this command.
    ///
    /// `?` and `N` answer every command. A typed reply answers the command
    /// with its mnemonic when every identifying field (the band, the memory
    /// address or the callsign slot) agrees; a reply that carries another
    /// band or address is a stale line from an earlier exchange.
    #[must_use]
    pub fn correlates(&self, response: &Response) -> bool {
        if matches!(response, Response::Rejected | Response::NotAvailable) {
            return true;
        }
        match *self {
            Self::Identify => matches!(response, Response::Identity { .. }),
            Self::FirmwareVersion => matches!(response, Response::FirmwareVersion { .. }),
            Self::RadioType => matches!(response, Response::RadioType(_)),
            Self::SerialInformation => matches!(response, Response::SerialInformation(_)),
            Self::PowerStatus | Self::PowerOff => {
                matches!(response, Response::PowerStatus { .. })
            }
            Self::Transmit => matches!(response, Response::TransmitAck),
            Self::Receive => matches!(response, Response::ReceiveAck),
            Self::SendAprsBeacon => matches!(response, Response::AprsBeaconAck),
            Self::RealTimeClock => matches!(response, Response::RealTimeClock(_)),
            Self::GetOperatingMode { band } | Self::SetOperatingMode { band, .. } => {
                matches!(response, Response::OperatingMode { band: reply, .. } if *reply == band)
            }
            Self::GetGatewayMode => matches!(response, Response::GatewayMode(_)),
            Self::GetFrequency { band } | Self::SetFrequency { band, .. } => {
                matches!(response, Response::Frequency { band: reply, .. } if *reply == band)
            }
            Self::GetChannelRecord { band } | Self::SetChannelRecord { band, .. } => {
                matches!(response, Response::ChannelRecord { band: reply, .. } if *reply == band)
            }
            Self::GetPowerLevel { band } | Self::SetPowerLevel { band, .. } => {
                matches!(response, Response::PowerLevel { band: reply, .. } if *reply == band)
            }
            Self::GetTuningMode { band } | Self::SetTuningMode { band, .. } => {
                matches!(response, Response::TuningMode { band: reply, .. } if *reply == band)
            }
            Self::GetSquelch { band } | Self::SetSquelch { band, .. } => {
                matches!(response, Response::Squelch { band: reply, .. } if *reply == band)
            }
            Self::GetSmeter { band } => {
                matches!(response, Response::Smeter { band: reply, .. } if *reply == band)
            }
            Self::GetBusy { band } => {
                matches!(response, Response::Busy { band: reply, .. } if *reply == band)
            }
            Self::GetAttenuator { band } | Self::SetAttenuator { band, .. } => {
                matches!(response, Response::Attenuator { band: reply, .. } if *reply == band)
            }
            Self::GetStepSize { band } | Self::SetStepSize { band, .. } => {
                matches!(response, Response::StepSize { band: reply, .. } if *reply == band)
            }
            Self::GetAmHighCut | Self::SetAmHighCut { .. } => {
                matches!(response, Response::AmHighCut(_))
            }
            Self::FrequencyUp => matches!(response, Response::FrequencyUpAck),
            Self::FrequencyDown => matches!(response, Response::FrequencyDownAck),
            Self::GetCurrentChannel { .. } => matches!(response, Response::CurrentChannel(_)),
            Self::RecallMemoryChannel { band, address } => matches!(
                response,
                Response::MemoryRecallAck { band: reply_band, address: reply_address }
                    if *reply_band == band && *reply_address == address
            ),
            Self::GetMemoryChannel { address } | Self::WriteMemoryChannel { address, .. } => {
                matches!(
                    response,
                    Response::MemoryChannel { address: reply, .. } if *reply == address
                )
            }
            Self::ClearMemoryChannel { address } => matches!(
                response,
                Response::MemoryChannelCleared { address: reply } if *reply == address
            ),
            Self::GetDstarCallsign { .. }
            | Self::SetDstarCallsign { .. }
            | Self::GetBandControl
            | Self::SetBandControl { .. }
            | Self::GetBandDisplay
            | Self::SetBandDisplay { .. }
            | Self::GetDstarSlot
            | Self::SetDstarSlot { .. }
            | Self::GetBacklightControl
            | Self::SetBacklightControl { .. }
            | Self::GetMyPositionSelection
            | Self::SetMyPositionSelection { .. }
            | Self::GetPacketDataRate
            | Self::SetPacketDataRate { .. }
            | Self::GetBeaconMethod
            | Self::SetBeaconMethod { .. }
            | Self::GetTncMode
            | Self::SetTncMode { .. }
            | Self::GetVoxDelay
            | Self::SetVoxDelay { .. }
            | Self::GetVoxGain
            | Self::SetVoxGain { .. }
            | Self::GetVox
            | Self::SetVox { .. }
            | Self::GetGpsSettings
            | Self::SetGpsSettings { .. }
            | Self::GetGpsSentences
            | Self::SetGpsSentences { .. }
            | Self::GetBluetooth
            | Self::SetBluetooth { .. }
            | Self::GetAprsCallsign
            | Self::SetAprsCallsign { .. } => self.correlates_global(response),
        }
    }

    /// Correlation of the global commands; `false` for the rest.
    fn correlates_global(&self, response: &Response) -> bool {
        match *self {
            Self::GetDstarCallsign { slot } => {
                matches!(response, Response::DstarCallsign(entry) if entry.slot == slot)
            }
            Self::SetDstarCallsign { ref entry } => {
                matches!(response, Response::DstarCallsign(reply) if reply.slot == entry.slot)
            }
            Self::GetBandControl | Self::SetBandControl { .. } => {
                matches!(response, Response::BandControl(_))
            }
            Self::GetBandDisplay | Self::SetBandDisplay { .. } => {
                matches!(response, Response::BandDisplay(_))
            }
            Self::GetDstarSlot | Self::SetDstarSlot { .. } => {
                matches!(response, Response::DstarSlot(_))
            }
            Self::GetBacklightControl | Self::SetBacklightControl { .. } => {
                matches!(response, Response::BacklightControl(_))
            }
            Self::GetMyPositionSelection | Self::SetMyPositionSelection { .. } => {
                matches!(response, Response::MyPositionSelection(_))
            }
            Self::GetPacketDataRate | Self::SetPacketDataRate { .. } => {
                matches!(response, Response::PacketDataRate(_))
            }
            Self::GetBeaconMethod | Self::SetBeaconMethod { .. } => {
                matches!(response, Response::BeaconMethod(_))
            }
            Self::GetTncMode | Self::SetTncMode { .. } => {
                matches!(response, Response::TncMode { .. })
            }
            Self::GetVoxDelay | Self::SetVoxDelay { .. } => {
                matches!(response, Response::VoxDelay(_))
            }
            Self::GetVoxGain | Self::SetVoxGain { .. } => matches!(response, Response::VoxGain(_)),
            Self::GetVox | Self::SetVox { .. } => matches!(response, Response::Vox(_)),
            Self::GetGpsSettings | Self::SetGpsSettings { .. } => {
                matches!(response, Response::GpsSettings(_))
            }
            Self::GetGpsSentences | Self::SetGpsSentences { .. } => {
                matches!(response, Response::GpsSentences(_))
            }
            Self::GetBluetooth | Self::SetBluetooth { .. } => {
                matches!(response, Response::Bluetooth { .. })
            }
            Self::GetAprsCallsign | Self::SetAprsCallsign { .. } => {
                matches!(response, Response::AprsCallsign(_))
            }
            _ => false,
        }
    }
}

/// A parsed reply line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// `ID TM-D750`.
    Identity {
        /// The model.
        model: RadioModel,
    },
    /// `FV <identity>`.
    FirmwareVersion {
        /// The firmware identity.
        version: FirmwareIdentity,
    },
    /// `TY <opaque graphic-ASCII payload>`; spaces are not accepted in the payload.
    RadioType(RadioType),
    /// `AE <serial>,<code>`.
    SerialInformation(SerialInformation),
    /// `PS <0|1>`.
    PowerStatus {
        /// Whether the radio reports itself on.
        on: bool,
    },
    /// `RT <YYMMDDHHMMSS>`.
    RealTimeClock(RealTimeClock),
    /// `MD <band>,<mode>`.
    OperatingMode {
        /// Band reported by the radio.
        band: Band,
        /// Current demodulation mode.
        mode: OperatingMode,
    },
    /// `GW <mode>`.
    GatewayMode(DvGatewayMode),
    /// `FQ <band>,<hz>`.
    Frequency {
        /// Band reported by the radio.
        band: Band,
        /// Current frequency.
        frequency: Frequency,
    },
    /// `FO <band>,<18 fields>`.
    ChannelRecord {
        /// Band reported by the radio.
        band: Band,
        /// The band's current channel settings.
        record: CatChannelRecord,
    },
    /// `PC <band>,<level>`.
    PowerLevel {
        /// Band reported by the radio.
        band: Band,
        /// Current transmit power.
        level: PowerLevel,
    },
    /// `VM <band>,<mode>`.
    TuningMode {
        /// Band reported by the radio.
        band: Band,
        /// Current tuning mode.
        mode: TuningMode,
    },
    /// `SQ <band>,<level>`.
    Squelch {
        /// Band reported by the radio.
        band: Band,
        /// Current squelch level.
        level: SquelchLevel,
    },
    /// `SM <band>,<reading>`.
    Smeter {
        /// Band reported by the radio.
        band: Band,
        /// Current reading.
        reading: SMeterReading,
    },
    /// `BY <band>,<0|1>`.
    Busy {
        /// Band reported by the radio.
        band: Band,
        /// Whether the squelch is open.
        busy: bool,
    },
    /// `RA <band>,<0|1>`.
    Attenuator {
        /// Band reported by the radio.
        band: Band,
        /// Whether the attenuator is on.
        enabled: bool,
    },
    /// `SF <band>,<step>`.
    StepSize {
        /// Band reported by the radio.
        band: Band,
        /// Current tuning step.
        step: StepSize,
    },
    /// `SH 0,<cut>`.
    AmHighCut(AmHighCut),
    /// `UP`.
    FrequencyUpAck,
    /// `DW`.
    FrequencyDownAck,
    /// `MR <selector>`: the selected memory channel, without a band field.
    CurrentChannel(CurrentMemorySelector),
    /// `MR <band>,<address>`: a recall echo.
    MemoryRecallAck {
        /// Band reported by the radio.
        band: Band,
        /// Recalled channel.
        address: MemoryChannelAddress,
    },
    /// `ME <address>,<record>`.
    MemoryChannel {
        /// Address reported by the radio.
        address: MemoryChannelAddress,
        /// The stored channel.
        record: CatMemoryChannelRecord,
    },
    /// `ME <address>` alone: the acknowledgement of a clear.
    MemoryChannelCleared {
        /// Address reported by the radio.
        address: MemoryChannelAddress,
    },
    /// `DC <slot>,<callsign>,<memo>`.
    DstarCallsign(DstarCallsignEntry),
    /// `BC <control>,<ptt>`.
    BandControl(BandControl),
    /// `DL <0|1>`.
    BandDisplay(BandDisplay),
    /// `DS <slot>`.
    DstarSlot(DstarSlot),
    /// `LC <value>`.
    BacklightControl(BacklightControl),
    /// `MS <selection>`.
    MyPositionSelection(MyPositionSelection),
    /// `AS <rate>`.
    PacketDataRate(PacketDataRate),
    /// `PT <method>`.
    BeaconMethod(BeaconMethod),
    /// `TN <mode>,<data band>`.
    TncMode {
        /// Current TNC mode.
        mode: TncMode,
        /// Band the TNC uses.
        data_band: Band,
    },
    /// `VD <index>`.
    VoxDelay(VoxDelay),
    /// `VG <gain>`.
    VoxGain(VoxGain),
    /// `VX <state>`.
    Vox(VoxMode),
    /// `GP <gps>,<pc>`.
    GpsSettings(GpsSettings),
    /// `GS <six flags>`.
    GpsSentences(NmeaSentences),
    /// `BT <0|1>`.
    Bluetooth {
        /// Whether Bluetooth is on.
        enabled: bool,
    },
    /// `CS <callsign>`.
    AprsCallsign(AprsCallsign),
    /// `TX 1`: the radio accepted the transmit request, whichever band is the
    /// PTT band.
    TransmitAck,
    /// `RX`: the radio accepted the receive request.
    ReceiveAck,
    /// `BE`: the radio accepted the beacon request.
    AprsBeaconAck,
    /// `?`: the radio rejected the command.
    Rejected,
    /// `N`: the command is unavailable in the current state.
    NotAvailable,
    /// Any other line, kept verbatim.
    Other {
        /// Mnemonic.
        mnemonic: String,
        /// Payload after the space (empty when absent).
        payload: String,
    },
}

impl Response {
    /// Parse one reply line (with or without its terminator).
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::NonAsciiLine`] for non-ASCII bytes,
    /// [`ProtocolError::EmptyLine`] for an empty line,
    /// [`ProtocolError::UnexpectedIdentity`] for an `ID` payload other than
    /// `TM-D750`, [`ProtocolError::FieldCount`] for the wrong number of fields,
    /// and [`ProtocolError::FieldParse`] for a malformed typed payload.
    pub fn from_bytes(line: &[u8]) -> Result<Self, ProtocolError> {
        let line = line.strip_suffix(&[LINE_TERMINATOR]).unwrap_or(line);
        if !line.is_ascii() {
            return Err(ProtocolError::NonAsciiLine {
                line: line.to_vec(),
            });
        }
        let text = String::from_utf8_lossy(line);
        let text = text.as_ref();
        match text {
            "?" => return Ok(Self::Rejected),
            "N" => return Ok(Self::NotAvailable),
            "" => {
                return Err(ProtocolError::EmptyLine {
                    line: String::new(),
                });
            }
            _ => {}
        }
        let (mnemonic, payload) = text.split_once(' ').unwrap_or((text, ""));
        if let Some(response) = parse_identity(mnemonic, payload)? {
            return Ok(response);
        }
        if let Some(response) = parse_band_indexed(mnemonic, payload)? {
            return Ok(response);
        }
        if let Some(response) = parse_band_levels(mnemonic, payload)? {
            return Ok(response);
        }
        if let Some(response) = parse_memory(mnemonic, payload)? {
            return Ok(response);
        }
        if let Some(response) = parse_global(mnemonic, payload)? {
            return Ok(response);
        }
        if let Some(response) = parse_transmit(mnemonic, payload)? {
            return Ok(response);
        }
        Ok(Self::Other {
            mnemonic: mnemonic.to_owned(),
            payload: payload.to_owned(),
        })
    }
}

impl TryFrom<&[u8]> for Response {
    type Error = ProtocolError;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(value)
    }
}

impl std::str::FromStr for Response {
    type Err = ProtocolError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_bytes(value.as_bytes())
    }
}

fn parse_identity(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "ID" => RadioModel::try_from(payload)
            .map(|model| Response::Identity { model })
            .map_err(|_| ProtocolError::UnexpectedIdentity {
                reply: payload.to_owned(),
            })?,
        "FV" => FirmwareIdentity::new(payload)
            .map(|version| Response::FirmwareVersion { version })
            .map_err(|error| field_error(CommandName::Fv, ReplyField::Version, error))?,
        "TY" => RadioType::new(payload)
            .map(Response::RadioType)
            .map_err(|error| field_error(CommandName::Ty, ReplyField::RadioType, error))?,
        "AE" => {
            let [serial, code] = split_exact::<2>(payload, CommandName::Ae)?;
            SerialInformation::new(serial, code)
                .map(Response::SerialInformation)
                .map_err(|error| {
                    field_error(CommandName::Ae, ReplyField::SerialInformation, error)
                })?
        }
        "PS" => Response::PowerStatus {
            on: boolean(payload, CommandName::Ps, ReplyField::Power)?,
        },
        "RT" => RealTimeClock::from_wire_str(payload)
            .map(Response::RealTimeClock)
            .map_err(|error| field_error(CommandName::Rt, ReplyField::Clock, error))?,
        "GW" => Response::GatewayMode(DvGatewayMode::from(decimal_u8(
            payload,
            CommandName::Gw,
            ReplyField::Mode,
        )?)),
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_band_indexed(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "MD" => {
            let [band_field, mode] = split_exact::<2>(payload, CommandName::Md)?;
            Response::OperatingMode {
                band: band(band_field, CommandName::Md)?,
                mode: OperatingMode::from(decimal_u8(mode, CommandName::Md, ReplyField::Mode)?),
            }
        }
        "FQ" => {
            let [band_field, hz] = split_exact::<2>(payload, CommandName::Fq)?;
            Response::Frequency {
                band: band(band_field, CommandName::Fq)?,
                frequency: Frequency::from_wire_str(hz)
                    .map_err(|error| field_error(CommandName::Fq, ReplyField::Frequency, error))?,
            }
        }
        "FO" => {
            let (band_field, rest) = payload.split_once(',').ok_or(ProtocolError::FieldCount {
                command: CommandName::Fo,
                expected: CHANNEL_FIELD_COUNT + 1,
                actual: 1,
            })?;
            let fields =
                split_exact::<CHANNEL_FIELD_COUNT>(rest, CommandName::Fo).map_err(|error| {
                    match error {
                        ProtocolError::FieldCount {
                            command,
                            expected,
                            actual,
                        } => ProtocolError::FieldCount {
                            command,
                            expected: expected + 1,
                            actual: actual + 1,
                        },
                        other => other,
                    }
                })?;
            Response::ChannelRecord {
                band: band(band_field, CommandName::Fo)?,
                record: parse_channel_fields(&fields, CommandName::Fo)?,
            }
        }
        "PC" => {
            let [band_field, level] = split_exact::<2>(payload, CommandName::Pc)?;
            Response::PowerLevel {
                band: band(band_field, CommandName::Pc)?,
                level: PowerLevel::try_from(decimal_u8(level, CommandName::Pc, ReplyField::Level)?)
                    .map_err(|error| field_error(CommandName::Pc, ReplyField::Level, error))?,
            }
        }
        "VM" => {
            let [band_field, mode] = split_exact::<2>(payload, CommandName::Vm)?;
            Response::TuningMode {
                band: band(band_field, CommandName::Vm)?,
                mode: TuningMode::try_from(decimal_u8(mode, CommandName::Vm, ReplyField::Mode)?)
                    .map_err(|error| field_error(CommandName::Vm, ReplyField::Mode, error))?,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_band_levels(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "SQ" => {
            let [band_field, level] = split_exact::<2>(payload, CommandName::Sq)?;
            Response::Squelch {
                band: band(band_field, CommandName::Sq)?,
                level: SquelchLevel::new(decimal_u8(level, CommandName::Sq, ReplyField::Level)?)
                    .map_err(|error| field_error(CommandName::Sq, ReplyField::Level, error))?,
            }
        }
        "SM" => {
            let [band_field, reading] = split_exact::<2>(payload, CommandName::Sm)?;
            Response::Smeter {
                band: band(band_field, CommandName::Sm)?,
                reading: SMeterReading::new(decimal_u8(
                    reading,
                    CommandName::Sm,
                    ReplyField::Reading,
                )?),
            }
        }
        "BY" => {
            let [band_field, busy] = split_exact::<2>(payload, CommandName::By)?;
            Response::Busy {
                band: band(band_field, CommandName::By)?,
                busy: boolean(busy, CommandName::By, ReplyField::Busy)?,
            }
        }
        "RA" => {
            let [band_field, enabled] = split_exact::<2>(payload, CommandName::Ra)?;
            Response::Attenuator {
                band: band(band_field, CommandName::Ra)?,
                enabled: boolean(enabled, CommandName::Ra, ReplyField::Enabled)?,
            }
        }
        "SF" => {
            let [band_field, step] = split_exact::<2>(payload, CommandName::Sf)?;
            Response::StepSize {
                band: band(band_field, CommandName::Sf)?,
                step: StepSize::from_wire_str(step)
                    .map_err(|error| field_error(CommandName::Sf, ReplyField::Step, error))?,
            }
        }
        "SH" => {
            let [filter, cut] = split_exact::<2>(payload, CommandName::Sh)?;
            if filter != "0" {
                return Err(field_error(
                    CommandName::Sh,
                    ReplyField::Filter,
                    ReplyFieldError::FilterIndex {
                        value: filter.to_owned(),
                    },
                ));
            }
            Response::AmHighCut(
                AmHighCut::try_from(decimal_u8(cut, CommandName::Sh, ReplyField::Cut)?)
                    .map_err(|error| field_error(CommandName::Sh, ReplyField::Cut, error))?,
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_memory(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "UP" | "DW" if !payload.is_empty() => {
            return Err(field_error(
                if mnemonic == "UP" {
                    CommandName::Up
                } else {
                    CommandName::Dw
                },
                ReplyField::Payload,
                ReplyFieldError::UnexpectedPayload {
                    value: payload.to_owned(),
                },
            ));
        }
        "UP" => Response::FrequencyUpAck,
        "DW" => Response::FrequencyDownAck,
        "MR" => match payload.split_once(',') {
            Some((band_field, address)) => Response::MemoryRecallAck {
                band: band(band_field, CommandName::Mr)?,
                address: MemoryChannelAddress::from_wire_str(address)
                    .map_err(|error| field_error(CommandName::Mr, ReplyField::Address, error))?,
            },
            None => Response::CurrentChannel(
                CurrentMemorySelector::from_wire_str(payload)
                    .map_err(|error| field_error(CommandName::Mr, ReplyField::Selector, error))?,
            ),
        },
        "ME" if !payload.contains(',') => Response::MemoryChannelCleared {
            address: MemoryChannelAddress::from_wire_str(payload)
                .map_err(|error| field_error(CommandName::Me, ReplyField::Address, error))?,
        },
        "ME" => {
            let (address, record) = parse_memory_channel(payload)?;
            Response::MemoryChannel { address, record }
        }
        "DC" => {
            let [slot, callsign, memo] = split_exact::<3>(payload, CommandName::Dc)?;
            let slot = DstarSlot::new(decimal_u8(slot, CommandName::Dc, ReplyField::Slot)?)
                .map_err(|error| field_error(CommandName::Dc, ReplyField::Slot, error))?;
            Response::DstarCallsign(
                DstarCallsignEntry::new(
                    slot,
                    DstarCallsignText {
                        callsign: callsign.into(),
                        memo: memo.into(),
                    },
                )
                .map_err(|error| field_error(CommandName::Dc, ReplyField::Callsign, error))?,
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_global(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "BC" => {
            let [control, ptt] = split_exact::<2>(payload, CommandName::Bc)?;
            Response::BandControl(BandControl {
                control: band(control, CommandName::Bc)?,
                ptt: band(ptt, CommandName::Bc)?,
            })
        }
        "DL" => Response::BandDisplay(
            BandDisplay::try_from(decimal_u8(payload, CommandName::Dl, ReplyField::Display)?)
                .map_err(|error| field_error(CommandName::Dl, ReplyField::Display, error))?,
        ),
        "DS" => Response::DstarSlot(
            DstarSlot::new(decimal_u8(payload, CommandName::Ds, ReplyField::Slot)?)
                .map_err(|error| field_error(CommandName::Ds, ReplyField::Slot, error))?,
        ),
        "LC" => Response::BacklightControl(
            BacklightControl::new(decimal_u8(payload, CommandName::Lc, ReplyField::Control)?)
                .map_err(|error| field_error(CommandName::Lc, ReplyField::Control, error))?,
        ),
        "MS" => Response::MyPositionSelection(
            MyPositionSelection::new(decimal_u8(payload, CommandName::Ms, ReplyField::Selection)?)
                .map_err(|error| field_error(CommandName::Ms, ReplyField::Selection, error))?,
        ),
        "AS" => Response::PacketDataRate(
            PacketDataRate::try_from(decimal_u8(payload, CommandName::As, ReplyField::Rate)?)
                .map_err(|error| field_error(CommandName::As, ReplyField::Rate, error))?,
        ),
        "PT" => Response::BeaconMethod(
            BeaconMethod::try_from(decimal_u8(payload, CommandName::Pt, ReplyField::Method)?)
                .map_err(|error| field_error(CommandName::Pt, ReplyField::Method, error))?,
        ),
        "TN" => {
            let [mode, data_band] = split_exact::<2>(payload, CommandName::Tn)?;
            Response::TncMode {
                mode: TncMode::from(decimal_u8(mode, CommandName::Tn, ReplyField::Mode)?),
                data_band: band(data_band, CommandName::Tn)?,
            }
        }
        "VD" => Response::VoxDelay(
            VoxDelay::new(decimal_u8(payload, CommandName::Vd, ReplyField::Delay)?)
                .map_err(|error| field_error(CommandName::Vd, ReplyField::Delay, error))?,
        ),
        "VG" => Response::VoxGain(
            VoxGain::new(decimal_u8(payload, CommandName::Vg, ReplyField::Gain)?)
                .map_err(|error| field_error(CommandName::Vg, ReplyField::Gain, error))?,
        ),
        "VX" => Response::Vox(VoxMode::from(decimal_u8(
            payload,
            CommandName::Vx,
            ReplyField::State,
        )?)),
        "GP" => {
            let [gps, pc] = split_exact::<2>(payload, CommandName::Gp)?;
            Response::GpsSettings(GpsSettings {
                gps_enabled: boolean(gps, CommandName::Gp, ReplyField::Gps)?,
                pc_output: boolean(pc, CommandName::Gp, ReplyField::PcOutput)?,
            })
        }
        "GS" => {
            let [gga, gll, gsa, gsv, rmc, vtg] = split_exact::<6>(payload, CommandName::Gs)?;
            Response::GpsSentences(NmeaSentences::from_flags([
                boolean(gga, CommandName::Gs, ReplyField::Gga)?,
                boolean(gll, CommandName::Gs, ReplyField::Gll)?,
                boolean(gsa, CommandName::Gs, ReplyField::Gsa)?,
                boolean(gsv, CommandName::Gs, ReplyField::Gsv)?,
                boolean(rmc, CommandName::Gs, ReplyField::Rmc)?,
                boolean(vtg, CommandName::Gs, ReplyField::Vtg)?,
            ]))
        }
        "BT" => Response::Bluetooth {
            enabled: boolean(payload, CommandName::Bt, ReplyField::Enabled)?,
        },
        "CS" => Response::AprsCallsign(
            AprsCallsign::new(payload)
                .map_err(|error| field_error(CommandName::Cs, ReplyField::Callsign, error))?,
        ),
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_transmit(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let (command, response, expected) = match mnemonic {
        "TX" => (CommandName::Tx, Response::TransmitAck, "1"),
        "RX" => (CommandName::Rx, Response::ReceiveAck, ""),
        "BE" => (CommandName::Be, Response::AprsBeaconAck, ""),
        _ => return Ok(None),
    };
    if payload == expected {
        Ok(Some(response))
    } else {
        Err(field_error(
            command,
            ReplyField::Payload,
            if command == CommandName::Tx {
                ReplyFieldError::TransmitPayload {
                    value: payload.to_owned(),
                }
            } else {
                ReplyFieldError::UnexpectedPayload {
                    value: payload.to_owned(),
                }
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn assert_encodings(cases: Vec<(Command, &[u8])>) {
        for (command, expected) in cases {
            assert_eq!(command.encode(), expected, "{command:?}");
            assert_eq!(
                Some(command.mnemonic().as_bytes()),
                expected.get(..2),
                "{command:?}"
            );
        }
    }

    #[test]
    fn read_commands_encode_with_a_carriage_return() -> TestResult {
        assert_encodings(vec![
            (Command::Identify, b"ID\r"),
            (Command::FirmwareVersion, b"FV\r"),
            (Command::RadioType, b"TY\r"),
            (Command::SerialInformation, b"AE\r"),
            (Command::PowerStatus, b"PS\r"),
            (Command::RealTimeClock, b"RT\r"),
            (Command::GetOperatingMode { band: Band::A }, b"MD 0\r"),
            (
                Command::SetOperatingMode {
                    band: Band::B,
                    mode: SelectableMode::Dv,
                },
                b"MD 1,1\r",
            ),
            (
                Command::SetOperatingMode {
                    band: Band::A,
                    mode: SelectableMode::Nfm,
                },
                b"MD 0,3\r",
            ),
            (Command::GetGatewayMode, b"GW\r"),
            (Command::GetFrequency { band: Band::B }, b"FQ 1\r"),
            (Command::GetChannelRecord { band: Band::A }, b"FO 0\r"),
            (Command::GetSmeter { band: Band::A }, b"SM 0\r"),
            (Command::GetBusy { band: Band::B }, b"BY 1\r"),
            (Command::GetAmHighCut, b"SH 0\r"),
            (Command::FrequencyUp, b"UP\r"),
            (Command::FrequencyDown, b"DW\r"),
            (Command::GetCurrentChannel { band: Band::A }, b"MR 0\r"),
            (
                Command::GetMemoryChannel {
                    address: MemoryChannelAddress::Priority,
                },
                b"ME Pri\r",
            ),
            (
                Command::GetDstarCallsign {
                    slot: DstarSlot::new(6)?,
                },
                b"DC 6\r",
            ),
            (Command::GetBandControl, b"BC\r"),
            (Command::GetTncMode, b"TN\r"),
            (Command::GetVox, b"VX\r"),
        ]);
        assert!(!Command::GetBluetooth.is_write());
        Ok(())
    }

    #[test]
    fn setter_commands_encode_their_arguments() -> TestResult {
        assert_encodings(vec![
            (
                Command::SetFrequency {
                    band: Band::A,
                    frequency: Frequency::new(145_190_000)?,
                },
                b"FQ 0,0145190000\r",
            ),
            (
                Command::SetPowerLevel {
                    band: Band::A,
                    level: PowerLevel::Low,
                },
                b"PC 0,2\r",
            ),
            (
                Command::SetTuningMode {
                    band: Band::A,
                    mode: TuningMode::DStarRepeater,
                },
                b"VM 0,3\r",
            ),
            (
                Command::SetSquelch {
                    band: Band::A,
                    level: SquelchLevel::new(31)?,
                },
                b"SQ 0,31\r",
            ),
            (
                Command::SetAttenuator {
                    band: Band::A,
                    enabled: true,
                },
                b"RA 0,1\r",
            ),
            (
                Command::SetStepSize {
                    band: Band::A,
                    step: StepSize::Hz100000,
                },
                b"SF 0,C\r",
            ),
            (
                Command::SetAmHighCut {
                    cut: AmHighCut::Hz7500,
                },
                b"SH 0,3\r",
            ),
        ]);
        Ok(())
    }

    #[test]
    fn global_setter_commands_encode_their_arguments() -> TestResult {
        assert_encodings(vec![
            (
                Command::RecallMemoryChannel {
                    band: Band::A,
                    address: MemoryChannelAddress::regular(21)?,
                },
                b"MR 0,021\r",
            ),
            (
                Command::SetBandControl {
                    roles: BandControl {
                        control: Band::B,
                        ptt: Band::A,
                    },
                },
                b"BC 1,0\r",
            ),
            (
                Command::SetBandDisplay {
                    display: BandDisplay::Single,
                },
                b"DL 1\r",
            ),
            (
                Command::SetDstarSlot {
                    slot: DstarSlot::new(2)?,
                },
                b"DS 2\r",
            ),
            (
                Command::SetBacklightControl {
                    control: BacklightControl::new(3)?,
                },
                b"LC 3\r",
            ),
            (
                Command::SetMyPositionSelection {
                    selection: MyPositionSelection::new(5)?,
                },
                b"MS 5\r",
            ),
            (
                Command::SetPacketDataRate {
                    rate: PacketDataRate::Bps9600,
                },
                b"AS 1\r",
            ),
            (
                Command::SetBeaconMethod {
                    method: BeaconMethod::Manual,
                },
                b"PT 0\r",
            ),
            (
                Command::SetVoxDelay {
                    delay: VoxDelay::new(6)?,
                },
                b"VD 6\r",
            ),
            (
                Command::SetVoxGain {
                    gain: VoxGain::new(0)?,
                },
                b"VG 0\r",
            ),
            (
                Command::SetGpsSettings {
                    settings: GpsSettings {
                        gps_enabled: true,
                        pc_output: false,
                    },
                },
                b"GP 1,0\r",
            ),
            (
                Command::SetGpsSentences {
                    sentences: NmeaSentences::from_flags([true, false, false, false, true, false]),
                },
                b"GS 1,0,0,0,1,0\r",
            ),
            (Command::SetBluetooth { enabled: false }, b"BT 0\r"),
        ]);
        assert!(Command::SetBluetooth { enabled: true }.is_write());
        Ok(())
    }

    #[test]
    fn dstar_callsign_writes_encode_and_report_as_writes() -> TestResult {
        assert_encodings(vec![
            (
                Command::SetDstarCallsign {
                    entry: DstarCallsignEntry::new(
                        DstarSlot::new(6)?,
                        DstarCallsignText {
                            callsign: "KQ4NIT".into(),
                            memo: "RIG".into(),
                        },
                    )?,
                },
                b"DC 6,KQ4NIT,RIG\r",
            ),
            (
                Command::SetDstarCallsign {
                    entry: DstarCallsignEntry::empty(DstarSlot::new(6)?),
                },
                b"DC 6,,\r",
            ),
        ]);
        assert!(
            Command::SetDstarCallsign {
                entry: DstarCallsignEntry::empty(DstarSlot::new(6)?),
            }
            .is_write()
        );
        assert!(
            !Command::GetDstarCallsign {
                slot: DstarSlot::new(6)?,
            }
            .is_write()
        );
        Ok(())
    }

    #[test]
    fn aprs_callsign_encodes_parses_and_correlates() -> TestResult {
        assert_encodings(vec![
            (Command::GetAprsCallsign, b"CS\r"),
            (
                Command::SetAprsCallsign {
                    callsign: AprsCallsign::new("KQ4NIT-9")?,
                },
                b"CS KQ4NIT-9\r",
            ),
        ]);
        assert!(
            Command::SetAprsCallsign {
                callsign: AprsCallsign::new("KQ4NIT")?,
            }
            .is_write()
        );
        assert!(!Command::GetAprsCallsign.is_write());
        assert_eq!(
            Response::from_bytes(b"CS NOCALL\r")?,
            Response::AprsCallsign(AprsCallsign::new("NOCALL")?)
        );
        let get = Command::GetAprsCallsign;
        assert!(get.correlates(&Response::AprsCallsign(AprsCallsign::new("NOCALL")?)));
        assert!(!get.correlates(&Response::Bluetooth { enabled: true }));
        let set = Command::SetAprsCallsign {
            callsign: AprsCallsign::new("KQ4NIT-9")?,
        };
        assert!(set.correlates(&Response::AprsCallsign(AprsCallsign::new("KQ4NIT-9")?)));
        assert!(!set.correlates(&Response::Bluetooth { enabled: true }));
        Ok(())
    }

    #[test]
    fn identity_replies_parse() -> TestResult {
        assert_eq!(
            Response::from_bytes(b"ID TM-D750\r")?,
            Response::Identity {
                model: RadioModel::TmD750
            }
        );
        assert_eq!(
            Response::from_bytes(b"FV 1.00")?,
            Response::FirmwareVersion {
                version: FirmwareIdentity::new("1.00")?
            }
        );
        assert_eq!(
            Response::from_bytes(b"TY K,2,1")?,
            Response::RadioType(RadioType::new("K,2,1")?)
        );
        assert_eq!(
            Response::from_bytes(b"AE A1234567,K01")?,
            Response::SerialInformation(SerialInformation::new("A1234567", "K01")?)
        );
        assert_eq!(
            Response::from_bytes(b"PS 1")?,
            Response::PowerStatus { on: true }
        );
        assert_eq!(
            Response::from_bytes(b"RT 260921003607")?,
            Response::RealTimeClock(RealTimeClock::from_wire_str("260921003607")?)
        );
        assert_eq!(
            Response::from_bytes(b"MD 0,1")?,
            Response::OperatingMode {
                band: Band::A,
                mode: OperatingMode::Dv,
            }
        );
        assert_eq!(
            Response::from_bytes(b"GW 2")?,
            Response::GatewayMode(DvGatewayMode::Terminal),
            "the observed Terminal reply must parse as a named read value"
        );
        assert_eq!(
            Response::from_bytes(b"GW 1")?,
            Response::GatewayMode(DvGatewayMode::from(1)),
            "unobserved GW 1 must remain losslessly unnamed"
        );
        assert_eq!(Response::from_bytes(b"?")?, Response::Rejected);
        assert_eq!(Response::from_bytes(b"N")?, Response::NotAvailable);
        assert_eq!(
            Response::from_bytes(b"XX 1")?,
            Response::Other {
                mnemonic: "XX".to_owned(),
                payload: "1".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn a_channel_record_write_encodes_its_fields_and_correlates_with_its_band() -> TestResult {
        const LINE: &str = "FO 1,0223020000,0001600000,8,8,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00";
        let Response::ChannelRecord { band, record } = Response::from_bytes(LINE.as_bytes())?
        else {
            return Err("FO must parse as a channel record".into());
        };
        let write = Command::SetChannelRecord {
            band,
            record: record.clone(),
        };
        assert_eq!(write.encode(), format!("{LINE}\r").into_bytes());
        assert_eq!(write.mnemonic(), "FO");
        assert!(write.is_write());
        assert!(write.correlates(&Response::ChannelRecord {
            band: Band::B,
            record: record.clone(),
        }));
        assert!(
            !write.correlates(&Response::ChannelRecord {
                band: Band::A,
                record,
            }),
            "a reply for the other band is a stale line"
        );
        Ok(())
    }

    #[test]
    fn band_indexed_replies_parse() -> TestResult {
        assert_eq!(
            Response::from_bytes(b"FQ 0,0145190000")?,
            Response::Frequency {
                band: Band::A,
                frequency: Frequency::new(145_190_000)?
            }
        );
        let Response::ChannelRecord { band, record } = Response::from_bytes(
            b"FO 1,0121500000,0000000000,6,6,2,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
        )?
        else {
            return Err("FO must parse as a channel record".into());
        };
        assert_eq!(band, Band::B);
        assert_eq!(record.mode, OperatingMode::Am);
        assert_eq!(
            Response::from_bytes(b"PC 1,1")?,
            Response::PowerLevel {
                band: Band::B,
                level: PowerLevel::Medium
            }
        );
        assert_eq!(
            Response::from_bytes(b"VM 0,3")?,
            Response::TuningMode {
                band: Band::A,
                mode: TuningMode::DStarRepeater
            }
        );
        assert_eq!(
            Response::from_bytes(b"SQ 1,10")?,
            Response::Squelch {
                band: Band::B,
                level: SquelchLevel::new(10)?
            }
        );
        assert_eq!(
            Response::from_bytes(b"SM 0,9")?,
            Response::Smeter {
                band: Band::A,
                reading: SMeterReading::new(9)
            }
        );
        assert_eq!(
            Response::from_bytes(b"BY 0,1")?,
            Response::Busy {
                band: Band::A,
                busy: true
            }
        );
        assert_eq!(
            Response::from_bytes(b"RA 0,0")?,
            Response::Attenuator {
                band: Band::A,
                enabled: false
            }
        );
        assert_eq!(
            Response::from_bytes(b"SF 1,8")?,
            Response::StepSize {
                band: Band::B,
                step: StepSize::Hz20000
            }
        );
        assert_eq!(
            Response::from_bytes(b"SH 0,2")?,
            Response::AmHighCut(AmHighCut::Hz6000)
        );
        Ok(())
    }

    #[test]
    fn memory_and_global_replies_parse() -> TestResult {
        assert_eq!(Response::from_bytes(b"UP")?, Response::FrequencyUpAck);
        assert_eq!(Response::from_bytes(b"DW")?, Response::FrequencyDownAck);
        assert_eq!(
            Response::from_bytes(b"MR AP ")?,
            Response::CurrentChannel(CurrentMemorySelector::Aprs)
        );
        assert_eq!(
            Response::from_bytes(b"MR 0,021")?,
            Response::MemoryRecallAck {
                band: Band::A,
                address: MemoryChannelAddress::regular(21)?
            }
        );
        let Response::MemoryChannel { address, record } = Response::from_bytes(
            b"ME 021,0446000000,0000000000,2,2,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0",
        )?
        else {
            return Err("ME must parse as a memory channel".into());
        };
        assert_eq!(address, MemoryChannelAddress::regular(21)?);
        assert!(!record.split());
        assert_eq!(
            Response::from_bytes(b"DC 1,,")?,
            Response::DstarCallsign(DstarCallsignEntry::empty(DstarSlot::new(1)?))
        );
        assert_eq!(
            Response::from_bytes(b"BC 1,0")?,
            Response::BandControl(BandControl {
                control: Band::B,
                ptt: Band::A
            })
        );
        assert_eq!(
            Response::from_bytes(b"DL 0")?,
            Response::BandDisplay(BandDisplay::Dual)
        );
        assert_eq!(
            Response::from_bytes(b"DS 6")?,
            Response::DstarSlot(DstarSlot::new(6)?)
        );
        assert_eq!(
            Response::from_bytes(b"LC 3")?,
            Response::BacklightControl(BacklightControl::new(3)?)
        );
        assert_eq!(
            Response::from_bytes(b"MS 0")?,
            Response::MyPositionSelection(MyPositionSelection::GPS)
        );
        assert_eq!(
            Response::from_bytes(b"AS 0")?,
            Response::PacketDataRate(PacketDataRate::Bps1200)
        );
        assert_eq!(
            Response::from_bytes(b"PT 2")?,
            Response::BeaconMethod(BeaconMethod::Auto)
        );
        assert_eq!(
            Response::from_bytes(b"TN 0,0")?,
            Response::TncMode {
                mode: TncMode::Off,
                data_band: Band::A
            }
        );
        assert_eq!(
            Response::from_bytes(b"VD 1")?,
            Response::VoxDelay(VoxDelay::new(1)?)
        );
        assert_eq!(
            Response::from_bytes(b"VG 4")?,
            Response::VoxGain(VoxGain::new(4)?)
        );
        assert_eq!(Response::from_bytes(b"VX 0")?, Response::Vox(VoxMode::Off));
        assert_eq!(
            Response::from_bytes(b"GP 1,0")?,
            Response::GpsSettings(GpsSettings {
                gps_enabled: true,
                pc_output: false
            })
        );
        assert_eq!(
            Response::from_bytes(b"GS 1,0,0,0,1,0")?,
            Response::GpsSentences(NmeaSentences::from_flags([
                true, false, false, false, true, false
            ]))
        );
        assert_eq!(
            Response::from_bytes(b"BT 1")?,
            Response::Bluetooth { enabled: true }
        );
        Ok(())
    }

    #[test]
    fn foreign_identities_and_bad_bytes_are_errors() {
        let d75 = Response::from_bytes(b"ID TH-D75");
        assert!(
            matches!(d75, Err(ProtocolError::UnexpectedIdentity { ref reply }) if reply == "TH-D75"),
            "{d75:?}"
        );
        let empty = Response::from_bytes(b"");
        assert!(
            matches!(empty, Err(ProtocolError::EmptyLine { .. })),
            "{empty:?}"
        );
        let non_ascii = Response::from_bytes(&[b'I', b'D', b' ', 0xFF]);
        assert!(
            matches!(non_ascii, Err(ProtocolError::NonAsciiLine { .. })),
            "{non_ascii:?}"
        );
        let bad_type = Response::from_bytes(b"TY ");
        assert!(
            matches!(
                bad_type,
                Err(ProtocolError::FieldParse {
                    command: CommandName::Ty,
                    ..
                })
            ),
            "{bad_type:?}"
        );
    }

    #[test]
    fn malformed_typed_replies_are_errors() {
        for line in [
            b"MD 0".as_slice(),
            b"MD 2,1",
            b"MD 0,x",
            b"MD 00,1",
            b"MD +0,+1",
            b"GW x",
            b"GW 00",
            b"FQ 0,145190000",
            b"PC 0,3",
            b"VM 0,4",
            b"SQ 0,32",
            b"SQ 0,03",
            b"BY 0,2",
            b"SF 0,D",
            b"SH 1,2",
            b"SH 0,4",
            b"UP 0",
            b"MR 0,AP ",
            b"MR 2,021",
            b"DC 7,,",
            b"DC 1,",
            b"BC 0",
            b"DL 2",
            b"DS 0",
            b"LC 4",
            b"MS 6",
            b"AS 2",
            b"PT 4",
            b"TN 0",
            b"VD 7",
            b"VG 10",
            b"GP 1",
            b"GS 1,0,0,0,1",
            b"BT 2",
            b"RT 26092100360",
            b"AE A1234567",
        ] {
            let result = Response::from_bytes(line);
            assert!(
                matches!(
                    result,
                    Err(ProtocolError::FieldParse { .. } | ProtocolError::FieldCount { .. })
                ),
                "{}: {result:?}",
                String::from_utf8_lossy(line)
            );
        }
        assert_eq!(
            Response::from_bytes(b"MD 0,7"),
            Ok(Response::OperatingMode {
                band: Band::A,
                mode: OperatingMode::from(7),
            })
        );
        assert_eq!(
            Response::from_bytes(b"GW 3"),
            Ok(Response::GatewayMode(DvGatewayMode::from(3)))
        );
        let short_record = Response::from_bytes(b"FO 0,0145190000");
        assert!(
            matches!(
                short_record,
                Err(ProtocolError::FieldCount {
                    command: CommandName::Fo,
                    expected: 19,
                    actual: 2
                })
            ),
            "{short_record:?}"
        );
    }

    #[test]
    fn memory_channel_writes_encode_parse_and_correlate() -> TestResult {
        let payload = "998,0146520000,0146540000,2,2,0,1,0,0,0,0,1,0,08,08,000,0,CQCQCQ,0,00,1";
        let Response::MemoryChannel { address, record } =
            Response::from_bytes(format!("ME {payload}").as_bytes())?
        else {
            return Err("ME must parse as a memory channel".into());
        };
        let write = Command::WriteMemoryChannel {
            address,
            record: record.clone(),
        };
        assert_eq!(write.encode(), format!("ME {payload}\r").into_bytes());
        assert!(write.is_write());
        assert!(write.correlates(&Response::MemoryChannel {
            address,
            record: record.clone()
        }));
        let clear = Command::ClearMemoryChannel { address };
        assert_eq!(clear.encode(), b"ME 998,\r");
        assert_eq!(
            Response::from_bytes(b"ME 998")?,
            Response::MemoryChannelCleared { address }
        );
        assert!(clear.correlates(&Response::MemoryChannelCleared { address }));
        assert!(!clear.correlates(&Response::MemoryChannel { address, record }));
        assert!(
            Response::from_bytes(b"ME 99").is_err(),
            "a cleared address keeps the three-character form"
        );
        Ok(())
    }

    #[test]
    fn correlation_requires_matching_identifying_fields() -> TestResult {
        let frequency_a = Response::Frequency {
            band: Band::A,
            frequency: Frequency::new(145_190_000)?,
        };
        let frequency_b = Response::Frequency {
            band: Band::B,
            frequency: Frequency::new(223_000_000)?,
        };
        assert!(Command::GetFrequency { band: Band::A }.correlates(&frequency_a));
        assert!(!Command::GetFrequency { band: Band::A }.correlates(&frequency_b));
        assert!(!Command::GetFrequency { band: Band::A }.correlates(&Response::FrequencyUpAck));
        assert!(Command::GetFrequency { band: Band::A }.correlates(&Response::Rejected));
        assert!(Command::GetFrequency { band: Band::A }.correlates(&Response::NotAvailable));
        assert!(
            !Command::GetFrequency { band: Band::A }.correlates(&Response::Other {
                mnemonic: "FQ".to_owned(),
                payload: String::new()
            })
        );
        let recall = Command::RecallMemoryChannel {
            band: Band::A,
            address: MemoryChannelAddress::regular(21)?,
        };
        assert!(recall.correlates(&Response::MemoryRecallAck {
            band: Band::A,
            address: MemoryChannelAddress::regular(21)?
        }));
        assert!(!recall.correlates(&Response::MemoryRecallAck {
            band: Band::A,
            address: MemoryChannelAddress::regular(22)?
        }));
        let slot = Command::GetDstarCallsign {
            slot: DstarSlot::new(2)?,
        };
        assert!(
            slot.correlates(&Response::DstarCallsign(DstarCallsignEntry::empty(
                DstarSlot::new(2)?
            )))
        );
        assert!(
            !slot.correlates(&Response::DstarCallsign(DstarCallsignEntry::empty(
                DstarSlot::new(3)?
            )))
        );
        let write = Command::SetDstarCallsign {
            entry: DstarCallsignEntry::new(
                DstarSlot::new(6)?,
                DstarCallsignText {
                    callsign: "KQ4NIT".into(),
                    memo: "RIG".into(),
                },
            )?,
        };
        assert!(
            write.correlates(&Response::DstarCallsign(DstarCallsignEntry::new(
                DstarSlot::new(6)?,
                DstarCallsignText {
                    callsign: "KQ4NIT".into(),
                    memo: "RIG".into()
                },
            )?))
        );
        assert!(
            !write.correlates(&Response::DstarCallsign(DstarCallsignEntry::new(
                DstarSlot::new(1)?,
                DstarCallsignText {
                    callsign: "KQ4NIT".into(),
                    memo: "RIG".into()
                },
            )?))
        );
        assert!(
            Command::SetBluetooth { enabled: true }
                .correlates(&Response::Bluetooth { enabled: true })
        );
        Ok(())
    }

    #[test]
    fn transmit_capable_commands_encode_report_as_writes_and_correlate() -> TestResult {
        assert_encodings(vec![
            (Command::Transmit, b"TX\r"),
            (Command::Receive, b"RX\r"),
            (Command::SendAprsBeacon, b"BE\r"),
            (
                Command::SetTncMode {
                    mode: SelectableTncMode::Kiss,
                    data_band: Band::B,
                },
                b"TN 2,1\r",
            ),
            (
                Command::SetTncMode {
                    mode: SelectableTncMode::Aprs,
                    data_band: Band::A,
                },
                b"TN 1,0\r",
            ),
            (
                Command::SetTncMode {
                    mode: SelectableTncMode::Off,
                    data_band: Band::A,
                },
                b"TN 0,0\r",
            ),
            (Command::SetVox { enabled: true }, b"VX 1\r"),
            (Command::SetVox { enabled: false }, b"VX 0\r"),
            (Command::PowerOff, b"PS 0\r"),
        ]);
        for command in [
            Command::Transmit,
            Command::Receive,
            Command::SendAprsBeacon,
            Command::SetVox { enabled: false },
            Command::PowerOff,
        ] {
            assert!(command.is_write(), "{command:?}");
        }
        assert_eq!(Response::from_bytes(b"TX 1")?, Response::TransmitAck);
        assert_eq!(Response::from_bytes(b"RX")?, Response::ReceiveAck);
        assert_eq!(Response::from_bytes(b"BE")?, Response::AprsBeaconAck);
        assert!(Command::Transmit.correlates(&Response::TransmitAck));
        assert!(!Command::Transmit.correlates(&Response::ReceiveAck));
        assert!(Command::Receive.correlates(&Response::ReceiveAck));
        assert!(Command::SendAprsBeacon.correlates(&Response::AprsBeaconAck));
        assert_eq!(
            Response::from_bytes(b"TN 2,1")?,
            Response::TncMode {
                mode: TncMode::Kiss,
                data_band: Band::B
            }
        );
        assert_eq!(
            Response::from_bytes(b"TN 1,0")?,
            Response::TncMode {
                mode: TncMode::Aprs,
                data_band: Band::A
            }
        );
        assert!(
            Command::SetTncMode {
                mode: SelectableTncMode::Kiss,
                data_band: Band::B
            }
            .correlates(&Response::TncMode {
                mode: TncMode::Kiss,
                data_band: Band::B
            })
        );
        assert_eq!(Response::from_bytes(b"VX 1")?, Response::Vox(VoxMode::On));
        assert!(Command::SetVox { enabled: true }.correlates(&Response::Vox(VoxMode::On)));
        assert!(Command::PowerOff.correlates(&Response::PowerStatus { on: false }));
        for malformed in [b"TX".as_slice(), b"TX 0", b"TX 2", b"RX 0", b"BE 1"] {
            assert!(
                Response::from_bytes(malformed).is_err(),
                "{}",
                String::from_utf8_lossy(malformed)
            );
        }
        Ok(())
    }
}
