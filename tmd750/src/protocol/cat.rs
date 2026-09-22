//! CAT commands and reply lines.
//!
//! A request is the mnemonic, an optional space and argument list, and a
//! carriage return; a reply is the mnemonic, a space and the payload,
//! terminated the same way. A lone `?` rejects the command and a lone `N`
//! reports that it is unavailable in the current state. Every command here
//! was sent to firmware 1.02 over the panel USB connector and its reply read;
//! a setter's reply echoes the accepted setting in the read form.

use crate::error::ProtocolError;
use crate::types::{
    AmHighCut, BacklightControl, Band, BandControl, BandDisplay, BeaconMethod, CHANNEL_FIELD_COUNT,
    CatChannelRecord, CatMemoryChannelRecord, CurrentMemorySelector, DstarCallsignEntry, DstarSlot,
    DvGatewayMode, FirmwareIdentity, Frequency, GpsSettings, MemoryChannelAddress,
    MyPositionSelection, NmeaSentences, OperatingMode, PacketDataRate, PowerLevel, RadioModel,
    RadioType, RealTimeClock, SMeterReading, SelectableMode, SerialInformation, SquelchLevel,
    StepSize, TncMode, TuningMode, VoxDelay, VoxGain, VoxMode,
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
}

impl Command {
    /// The two-letter mnemonic.
    #[must_use]
    pub const fn mnemonic(&self) -> &'static str {
        match *self {
            Self::Identify => "ID",
            Self::FirmwareVersion => "FV",
            Self::RadioType => "TY",
            Self::SerialInformation => "AE",
            Self::PowerStatus => "PS",
            Self::RealTimeClock => "RT",
            Self::GetOperatingMode { .. } | Self::SetOperatingMode { .. } => "MD",
            Self::GetGatewayMode => "GW",
            Self::GetFrequency { .. } | Self::SetFrequency { .. } => "FQ",
            Self::GetChannelRecord { .. } => "FO",
            Self::GetPowerLevel { .. } | Self::SetPowerLevel { .. } => "PC",
            Self::GetTuningMode { .. } | Self::SetTuningMode { .. } => "VM",
            Self::GetSquelch { .. } | Self::SetSquelch { .. } => "SQ",
            Self::GetSmeter { .. } => "SM",
            Self::GetBusy { .. } => "BY",
            Self::GetAttenuator { .. } | Self::SetAttenuator { .. } => "RA",
            Self::GetStepSize { .. } | Self::SetStepSize { .. } => "SF",
            Self::GetAmHighCut | Self::SetAmHighCut { .. } => "SH",
            Self::FrequencyUp => "UP",
            Self::FrequencyDown => "DW",
            Self::GetCurrentChannel { .. } | Self::RecallMemoryChannel { .. } => "MR",
            Self::GetMemoryChannel { .. }
            | Self::WriteMemoryChannel { .. }
            | Self::ClearMemoryChannel { .. } => "ME",
            Self::GetDstarCallsign { .. } => "DC",
            Self::GetBandControl | Self::SetBandControl { .. } => "BC",
            Self::GetBandDisplay | Self::SetBandDisplay { .. } => "DL",
            Self::GetDstarSlot | Self::SetDstarSlot { .. } => "DS",
            Self::GetBacklightControl | Self::SetBacklightControl { .. } => "LC",
            Self::GetMyPositionSelection | Self::SetMyPositionSelection { .. } => "MS",
            Self::GetPacketDataRate | Self::SetPacketDataRate { .. } => "AS",
            Self::GetBeaconMethod | Self::SetBeaconMethod { .. } => "PT",
            Self::GetTncMode => "TN",
            Self::GetVoxDelay | Self::SetVoxDelay { .. } => "VD",
            Self::GetVoxGain | Self::SetVoxGain { .. } => "VG",
            Self::GetVox => "VX",
            Self::GetGpsSettings | Self::SetGpsSettings { .. } => "GP",
            Self::GetGpsSentences | Self::SetGpsSentences { .. } => "GS",
            Self::GetBluetooth | Self::SetBluetooth { .. } => "BT",
        }
    }

    /// Whether the command changes radio state.
    #[must_use]
    pub const fn is_write(&self) -> bool {
        matches!(
            self,
            Self::SetOperatingMode { .. }
                | Self::SetFrequency { .. }
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
            | Self::GetBluetooth => None,
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
            | Self::SetBluetooth { .. } => self.memory_and_global_argument(),
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
            Self::GetAmHighCut => Some("0".to_owned()),
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
            Self::SetBluetooth { enabled } => Some(u8::from(enabled).to_string()),
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
            Self::PowerStatus => matches!(response, Response::PowerStatus { .. }),
            Self::RealTimeClock => matches!(response, Response::RealTimeClock(_)),
            Self::GetOperatingMode { band } | Self::SetOperatingMode { band, .. } => {
                matches!(response, Response::OperatingMode { band: reply, .. } if *reply == band)
            }
            Self::GetGatewayMode => matches!(response, Response::GatewayMode(_)),
            Self::GetFrequency { band } | Self::SetFrequency { band, .. } => {
                matches!(response, Response::Frequency { band: reply, .. } if *reply == band)
            }
            Self::GetChannelRecord { band } => {
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
            | Self::GetVoxDelay
            | Self::SetVoxDelay { .. }
            | Self::GetVoxGain
            | Self::SetVoxGain { .. }
            | Self::GetVox
            | Self::GetGpsSettings
            | Self::SetGpsSettings { .. }
            | Self::GetGpsSentences
            | Self::SetGpsSentences { .. }
            | Self::GetBluetooth
            | Self::SetBluetooth { .. } => self.correlates_global(response),
        }
    }

    /// Correlation of the global commands; `false` for the rest.
    fn correlates_global(&self, response: &Response) -> bool {
        match *self {
            Self::GetDstarCallsign { slot } => {
                matches!(response, Response::DstarCallsign(entry) if entry.slot == slot)
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
            Self::GetTncMode => matches!(response, Response::TncMode { .. }),
            Self::GetVoxDelay | Self::SetVoxDelay { .. } => {
                matches!(response, Response::VoxDelay(_))
            }
            Self::GetVoxGain | Self::SetVoxGain { .. } => matches!(response, Response::VoxGain(_)),
            Self::GetVox => matches!(response, Response::Vox(_)),
            Self::GetGpsSettings | Self::SetGpsSettings { .. } => {
                matches!(response, Response::GpsSettings(_))
            }
            Self::GetGpsSentences | Self::SetGpsSentences { .. } => {
                matches!(response, Response::GpsSentences(_))
            }
            Self::GetBluetooth | Self::SetBluetooth { .. } => {
                matches!(response, Response::Bluetooth { .. })
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

/// Parse one reply line (with or without its terminator).
///
/// # Errors
///
/// Returns [`ProtocolError::NonAsciiLine`] for non-ASCII bytes,
/// [`ProtocolError::EmptyLine`] for an empty line,
/// [`ProtocolError::UnexpectedIdentity`] for an `ID` payload other than
/// `TM-D750`, [`ProtocolError::FieldCount`] for the wrong number of fields,
/// and [`ProtocolError::FieldParse`] for a malformed typed payload.
pub fn parse_line(line: &[u8]) -> Result<Response, ProtocolError> {
    let line = line.strip_suffix(&[LINE_TERMINATOR]).unwrap_or(line);
    if !line.is_ascii() {
        return Err(ProtocolError::NonAsciiLine {
            line: line.to_vec(),
        });
    }
    let text = String::from_utf8_lossy(line);
    let text = text.as_ref();
    match text {
        "?" => return Ok(Response::Rejected),
        "N" => return Ok(Response::NotAvailable),
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
    Ok(Response::Other {
        mnemonic: mnemonic.to_owned(),
        payload: payload.to_owned(),
    })
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
            .map_err(|error| field_error("FV", "version", error.to_string()))?,
        "TY" => RadioType::new(payload)
            .map(Response::RadioType)
            .map_err(|error| field_error("TY", "radio_type", error.to_string()))?,
        "AE" => {
            let [serial, code] = split_exact::<2>(payload, "AE")?;
            SerialInformation::new(serial, code)
                .map(Response::SerialInformation)
                .map_err(|error| field_error("AE", "serial_information", error.to_string()))?
        }
        "PS" => Response::PowerStatus {
            on: boolean(payload, "PS", "power")?,
        },
        "RT" => RealTimeClock::from_wire_str(payload)
            .map(Response::RealTimeClock)
            .map_err(|error| field_error("RT", "clock", error.to_string()))?,
        "GW" => Response::GatewayMode(DvGatewayMode::from(decimal_u8(payload, "GW", "mode")?)),
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_band_indexed(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "MD" => {
            let [band_field, mode] = split_exact::<2>(payload, "MD")?;
            Response::OperatingMode {
                band: band(band_field, "MD")?,
                mode: OperatingMode::from(decimal_u8(mode, "MD", "mode")?),
            }
        }
        "FQ" => {
            let [band_field, hz] = split_exact::<2>(payload, "FQ")?;
            Response::Frequency {
                band: band(band_field, "FQ")?,
                frequency: Frequency::from_wire_str(hz)
                    .map_err(|error| field_error("FQ", "frequency", error.to_string()))?,
            }
        }
        "FO" => {
            let (band_field, rest) = payload.split_once(',').ok_or(ProtocolError::FieldCount {
                command: "FO",
                expected: CHANNEL_FIELD_COUNT + 1,
                actual: 1,
            })?;
            let fields =
                split_exact::<CHANNEL_FIELD_COUNT>(rest, "FO").map_err(|error| match error {
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
                })?;
            Response::ChannelRecord {
                band: band(band_field, "FO")?,
                record: parse_channel_fields(&fields, "FO")?,
            }
        }
        "PC" => {
            let [band_field, level] = split_exact::<2>(payload, "PC")?;
            Response::PowerLevel {
                band: band(band_field, "PC")?,
                level: PowerLevel::try_from(decimal_u8(level, "PC", "level")?)
                    .map_err(|error| field_error("PC", "level", error.to_string()))?,
            }
        }
        "VM" => {
            let [band_field, mode] = split_exact::<2>(payload, "VM")?;
            Response::TuningMode {
                band: band(band_field, "VM")?,
                mode: TuningMode::try_from(decimal_u8(mode, "VM", "mode")?)
                    .map_err(|error| field_error("VM", "mode", error.to_string()))?,
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_band_levels(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "SQ" => {
            let [band_field, level] = split_exact::<2>(payload, "SQ")?;
            Response::Squelch {
                band: band(band_field, "SQ")?,
                level: SquelchLevel::new(decimal_u8(level, "SQ", "level")?)
                    .map_err(|error| field_error("SQ", "level", error.to_string()))?,
            }
        }
        "SM" => {
            let [band_field, reading] = split_exact::<2>(payload, "SM")?;
            Response::Smeter {
                band: band(band_field, "SM")?,
                reading: SMeterReading::new(decimal_u8(reading, "SM", "reading")?),
            }
        }
        "BY" => {
            let [band_field, busy] = split_exact::<2>(payload, "BY")?;
            Response::Busy {
                band: band(band_field, "BY")?,
                busy: boolean(busy, "BY", "busy")?,
            }
        }
        "RA" => {
            let [band_field, enabled] = split_exact::<2>(payload, "RA")?;
            Response::Attenuator {
                band: band(band_field, "RA")?,
                enabled: boolean(enabled, "RA", "enabled")?,
            }
        }
        "SF" => {
            let [band_field, step] = split_exact::<2>(payload, "SF")?;
            Response::StepSize {
                band: band(band_field, "SF")?,
                step: StepSize::from_wire_str(step)
                    .map_err(|error| field_error("SF", "step", error.to_string()))?,
            }
        }
        "SH" => {
            let [filter, cut] = split_exact::<2>(payload, "SH")?;
            if filter != "0" {
                return Err(field_error(
                    "SH",
                    "filter",
                    format!("expected filter index 0, got {filter:?}"),
                ));
            }
            Response::AmHighCut(
                AmHighCut::try_from(decimal_u8(cut, "SH", "cut")?)
                    .map_err(|error| field_error("SH", "cut", error.to_string()))?,
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
                mnemonic_static(mnemonic),
                "payload",
                format!("expected a bare acknowledgement, got {payload:?}"),
            ));
        }
        "UP" => Response::FrequencyUpAck,
        "DW" => Response::FrequencyDownAck,
        "MR" => match payload.split_once(',') {
            Some((band_field, address)) => Response::MemoryRecallAck {
                band: band(band_field, "MR")?,
                address: MemoryChannelAddress::from_wire_str(address)
                    .map_err(|error| field_error("MR", "address", error.to_string()))?,
            },
            None => Response::CurrentChannel(
                CurrentMemorySelector::from_wire_str(payload)
                    .map_err(|error| field_error("MR", "selector", error.to_string()))?,
            ),
        },
        "ME" if !payload.contains(',') => Response::MemoryChannelCleared {
            address: MemoryChannelAddress::from_wire_str(payload)
                .map_err(|error| field_error("ME", "address", error.to_string()))?,
        },
        "ME" => {
            let (address, record) = parse_memory_channel(payload)?;
            Response::MemoryChannel { address, record }
        }
        "DC" => {
            let [slot, callsign, memo] = split_exact::<3>(payload, "DC")?;
            let slot = DstarSlot::new(decimal_u8(slot, "DC", "slot")?)
                .map_err(|error| field_error("DC", "slot", error.to_string()))?;
            Response::DstarCallsign(
                DstarCallsignEntry::new(slot, callsign, memo)
                    .map_err(|error| field_error("DC", "callsign", error.to_string()))?,
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}

fn parse_global(mnemonic: &str, payload: &str) -> Result<Option<Response>, ProtocolError> {
    let response = match mnemonic {
        "BC" => {
            let [control, ptt] = split_exact::<2>(payload, "BC")?;
            Response::BandControl(BandControl {
                control: band(control, "BC")?,
                ptt: band(ptt, "BC")?,
            })
        }
        "DL" => Response::BandDisplay(
            BandDisplay::try_from(decimal_u8(payload, "DL", "display")?)
                .map_err(|error| field_error("DL", "display", error.to_string()))?,
        ),
        "DS" => Response::DstarSlot(
            DstarSlot::new(decimal_u8(payload, "DS", "slot")?)
                .map_err(|error| field_error("DS", "slot", error.to_string()))?,
        ),
        "LC" => Response::BacklightControl(
            BacklightControl::new(decimal_u8(payload, "LC", "control")?)
                .map_err(|error| field_error("LC", "control", error.to_string()))?,
        ),
        "MS" => Response::MyPositionSelection(
            MyPositionSelection::new(decimal_u8(payload, "MS", "selection")?)
                .map_err(|error| field_error("MS", "selection", error.to_string()))?,
        ),
        "AS" => Response::PacketDataRate(
            PacketDataRate::try_from(decimal_u8(payload, "AS", "rate")?)
                .map_err(|error| field_error("AS", "rate", error.to_string()))?,
        ),
        "PT" => Response::BeaconMethod(
            BeaconMethod::try_from(decimal_u8(payload, "PT", "method")?)
                .map_err(|error| field_error("PT", "method", error.to_string()))?,
        ),
        "TN" => {
            let [mode, data_band] = split_exact::<2>(payload, "TN")?;
            Response::TncMode {
                mode: TncMode::from(decimal_u8(mode, "TN", "mode")?),
                data_band: band(data_band, "TN")?,
            }
        }
        "VD" => Response::VoxDelay(
            VoxDelay::new(decimal_u8(payload, "VD", "delay")?)
                .map_err(|error| field_error("VD", "delay", error.to_string()))?,
        ),
        "VG" => Response::VoxGain(
            VoxGain::new(decimal_u8(payload, "VG", "gain")?)
                .map_err(|error| field_error("VG", "gain", error.to_string()))?,
        ),
        "VX" => Response::Vox(VoxMode::from(decimal_u8(payload, "VX", "state")?)),
        "GP" => {
            let [gps, pc] = split_exact::<2>(payload, "GP")?;
            Response::GpsSettings(GpsSettings {
                gps_enabled: boolean(gps, "GP", "gps")?,
                pc_output: boolean(pc, "GP", "pc_output")?,
            })
        }
        "GS" => {
            let [gga, gll, gsa, gsv, rmc, vtg] = split_exact::<6>(payload, "GS")?;
            Response::GpsSentences(NmeaSentences::from_flags([
                boolean(gga, "GS", "gga")?,
                boolean(gll, "GS", "gll")?,
                boolean(gsa, "GS", "gsa")?,
                boolean(gsv, "GS", "gsv")?,
                boolean(rmc, "GS", "rmc")?,
                boolean(vtg, "GS", "vtg")?,
            ]))
        }
        "BT" => Response::Bluetooth {
            enabled: boolean(payload, "BT", "enabled")?,
        },
        _ => return Ok(None),
    };
    Ok(Some(response))
}

const fn mnemonic_static(mnemonic: &str) -> &'static str {
    match mnemonic.as_bytes() {
        b"UP" => "UP",
        _ => "DW",
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
    fn identity_replies_parse() -> TestResult {
        assert_eq!(
            parse_line(b"ID TM-D750\r")?,
            Response::Identity {
                model: RadioModel::TmD750
            }
        );
        assert_eq!(
            parse_line(b"FV 1.00")?,
            Response::FirmwareVersion {
                version: FirmwareIdentity::new("1.00")?
            }
        );
        assert_eq!(
            parse_line(b"TY K,2,1")?,
            Response::RadioType(RadioType::new("K,2,1")?)
        );
        assert_eq!(
            parse_line(b"AE C6210439,K01")?,
            Response::SerialInformation(SerialInformation::new("C6210439", "K01")?)
        );
        assert_eq!(parse_line(b"PS 1")?, Response::PowerStatus { on: true });
        assert_eq!(
            parse_line(b"RT 260921003607")?,
            Response::RealTimeClock(RealTimeClock::from_wire_str("260921003607")?)
        );
        assert_eq!(
            parse_line(b"MD 0,1")?,
            Response::OperatingMode {
                band: Band::A,
                mode: OperatingMode::Dv,
            }
        );
        assert_eq!(
            parse_line(b"GW 2")?,
            Response::GatewayMode(DvGatewayMode::Terminal),
            "the observed Terminal reply must parse as a named read value"
        );
        assert_eq!(
            parse_line(b"GW 1")?,
            Response::GatewayMode(DvGatewayMode::Unqualified(1)),
            "unobserved GW 1 must remain losslessly unqualified"
        );
        assert_eq!(parse_line(b"?")?, Response::Rejected);
        assert_eq!(parse_line(b"N")?, Response::NotAvailable);
        assert_eq!(
            parse_line(b"XX 1")?,
            Response::Other {
                mnemonic: "XX".to_owned(),
                payload: "1".to_owned()
            }
        );
        Ok(())
    }

    #[test]
    fn band_indexed_replies_parse() -> TestResult {
        assert_eq!(
            parse_line(b"FQ 0,0145190000")?,
            Response::Frequency {
                band: Band::A,
                frequency: Frequency::new(145_190_000)?
            }
        );
        let Response::ChannelRecord { band, record } =
            parse_line(b"FO 1,0121500000,0000000000,6,6,2,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00")?
        else {
            return Err("FO must parse as a channel record".into());
        };
        assert_eq!(band, Band::B);
        assert_eq!(record.mode, OperatingMode::Am);
        assert_eq!(
            parse_line(b"PC 1,1")?,
            Response::PowerLevel {
                band: Band::B,
                level: PowerLevel::Medium
            }
        );
        assert_eq!(
            parse_line(b"VM 0,3")?,
            Response::TuningMode {
                band: Band::A,
                mode: TuningMode::DStarRepeater
            }
        );
        assert_eq!(
            parse_line(b"SQ 1,10")?,
            Response::Squelch {
                band: Band::B,
                level: SquelchLevel::new(10)?
            }
        );
        assert_eq!(
            parse_line(b"SM 0,9")?,
            Response::Smeter {
                band: Band::A,
                reading: SMeterReading::new(9)
            }
        );
        assert_eq!(
            parse_line(b"BY 0,1")?,
            Response::Busy {
                band: Band::A,
                busy: true
            }
        );
        assert_eq!(
            parse_line(b"RA 0,0")?,
            Response::Attenuator {
                band: Band::A,
                enabled: false
            }
        );
        assert_eq!(
            parse_line(b"SF 1,8")?,
            Response::StepSize {
                band: Band::B,
                step: StepSize::Hz20000
            }
        );
        assert_eq!(
            parse_line(b"SH 0,2")?,
            Response::AmHighCut(AmHighCut::Hz6000)
        );
        Ok(())
    }

    #[test]
    fn memory_and_global_replies_parse() -> TestResult {
        assert_eq!(parse_line(b"UP")?, Response::FrequencyUpAck);
        assert_eq!(parse_line(b"DW")?, Response::FrequencyDownAck);
        assert_eq!(
            parse_line(b"MR AP ")?,
            Response::CurrentChannel(CurrentMemorySelector::Aprs)
        );
        assert_eq!(
            parse_line(b"MR 0,021")?,
            Response::MemoryRecallAck {
                band: Band::A,
                address: MemoryChannelAddress::regular(21)?
            }
        );
        let Response::MemoryChannel { address, record } = parse_line(
            b"ME 021,0446000000,0000000000,2,2,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0",
        )?
        else {
            return Err("ME must parse as a memory channel".into());
        };
        assert_eq!(address, MemoryChannelAddress::regular(21)?);
        assert!(!record.split);
        assert_eq!(
            parse_line(b"DC 1,,")?,
            Response::DstarCallsign(DstarCallsignEntry::new(DstarSlot::new(1)?, "", "")?)
        );
        assert_eq!(
            parse_line(b"BC 1,0")?,
            Response::BandControl(BandControl {
                control: Band::B,
                ptt: Band::A
            })
        );
        assert_eq!(
            parse_line(b"DL 0")?,
            Response::BandDisplay(BandDisplay::Dual)
        );
        assert_eq!(
            parse_line(b"DS 6")?,
            Response::DstarSlot(DstarSlot::new(6)?)
        );
        assert_eq!(
            parse_line(b"LC 3")?,
            Response::BacklightControl(BacklightControl::new(3)?)
        );
        assert_eq!(
            parse_line(b"MS 0")?,
            Response::MyPositionSelection(MyPositionSelection::GPS)
        );
        assert_eq!(
            parse_line(b"AS 0")?,
            Response::PacketDataRate(PacketDataRate::Bps1200)
        );
        assert_eq!(
            parse_line(b"PT 2")?,
            Response::BeaconMethod(BeaconMethod::Auto)
        );
        assert_eq!(
            parse_line(b"TN 0,0")?,
            Response::TncMode {
                mode: TncMode::Off,
                data_band: Band::A
            }
        );
        assert_eq!(parse_line(b"VD 1")?, Response::VoxDelay(VoxDelay::new(1)?));
        assert_eq!(parse_line(b"VG 4")?, Response::VoxGain(VoxGain::new(4)?));
        assert_eq!(parse_line(b"VX 0")?, Response::Vox(VoxMode::Off));
        assert_eq!(
            parse_line(b"GP 1,0")?,
            Response::GpsSettings(GpsSettings {
                gps_enabled: true,
                pc_output: false
            })
        );
        assert_eq!(
            parse_line(b"GS 1,0,0,0,1,0")?,
            Response::GpsSentences(NmeaSentences::from_flags([
                true, false, false, false, true, false
            ]))
        );
        assert_eq!(parse_line(b"BT 1")?, Response::Bluetooth { enabled: true });
        Ok(())
    }

    #[test]
    fn foreign_identities_and_bad_bytes_are_errors() {
        let d75 = parse_line(b"ID TH-D75");
        assert!(
            matches!(d75, Err(ProtocolError::UnexpectedIdentity { ref reply }) if reply == "TH-D75"),
            "{d75:?}"
        );
        let empty = parse_line(b"");
        assert!(
            matches!(empty, Err(ProtocolError::EmptyLine { .. })),
            "{empty:?}"
        );
        let non_ascii = parse_line(&[b'I', b'D', b' ', 0xFF]);
        assert!(
            matches!(non_ascii, Err(ProtocolError::NonAsciiLine { .. })),
            "{non_ascii:?}"
        );
        let bad_type = parse_line(b"TY ");
        assert!(
            matches!(
                bad_type,
                Err(ProtocolError::FieldParse { command: "TY", .. })
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
            b"AE C6210439",
        ] {
            let result = parse_line(line);
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
            parse_line(b"MD 0,7"),
            Ok(Response::OperatingMode {
                band: Band::A,
                mode: OperatingMode::Unqualified(7),
            })
        );
        assert_eq!(
            parse_line(b"GW 3"),
            Ok(Response::GatewayMode(DvGatewayMode::Unqualified(3)))
        );
        let short_record = parse_line(b"FO 0,0145190000");
        assert!(
            matches!(
                short_record,
                Err(ProtocolError::FieldCount {
                    command: "FO",
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
            parse_line(format!("ME {payload}").as_bytes())?
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
            parse_line(b"ME 998")?,
            Response::MemoryChannelCleared { address }
        );
        assert!(clear.correlates(&Response::MemoryChannelCleared { address }));
        assert!(!clear.correlates(&Response::MemoryChannel { address, record }));
        assert!(
            parse_line(b"ME 99").is_err(),
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
            slot.correlates(&Response::DstarCallsign(DstarCallsignEntry::new(
                DstarSlot::new(2)?,
                "",
                ""
            )?))
        );
        assert!(
            !slot.correlates(&Response::DstarCallsign(DstarCallsignEntry::new(
                DstarSlot::new(3)?,
                "",
                ""
            )?))
        );
        assert!(
            Command::SetBluetooth { enabled: true }
                .correlates(&Response::Bluetooth { enabled: true })
        );
        Ok(())
    }
}
