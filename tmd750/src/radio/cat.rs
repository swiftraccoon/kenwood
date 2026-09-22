//! Typed CAT reads and verified writes beyond identity and mode.
//!
//! Every read sends one query without an implicit identity check. Every write
//! runs the firmware `1.02` / `K,2,1` gate, requires the echo to equal the
//! request, and reads the setting back; a failed echo or readback can follow
//! an applied change, and nothing is rolled back. All methods follow
//! [`Radio`]'s deadline, cancellation and post-error ownership contract and
//! return transport, timeout, parse, rejection, unexpected-response or
//! session-state errors; `?` is [`ProtocolError::Rejected`] and `N` is
//! [`ProtocolError::NotAvailable`].

use std::time::Duration;

use crate::error::{Error, ProtocolError};
use crate::protocol::cat::{Command, Response};
use crate::types::{
    AmHighCut, BacklightControl, Band, BandControl, BandDisplay, BeaconMethod, CatChannelRecord,
    CatMemoryChannelRecord, CurrentMemorySelector, DstarCallsignEntry, DstarSlot, Frequency,
    GpsSettings, MemoryChannelAddress, MyPositionSelection, NmeaSentences, PacketDataRate,
    PowerLevel, RealTimeClock, SMeterReading, SerialInformation, SquelchLevel, StepSize, TncMode,
    TuningMode, UrCallsign, VoxDelay, VoxGain, VoxMode,
};
use kenwood_transport::Transport;

use super::Radio;

/// Frequency readbacks attempted after an acknowledged `UP` or `DW`.
///
/// On firmware 1.02 the first readback, 3 ms after the acknowledgement,
/// already showed the new frequency.
pub const STEP_READBACK_ATTEMPTS: u8 = 20;

/// Pause between two frequency readbacks after an acknowledged `UP` or `DW`.
pub const STEP_READBACK_INTERVAL: Duration = Duration::from_millis(10);

/// Time after a step acknowledgement before the step methods return.
///
/// On firmware 1.02 a setting write sent up to 16 ms after the
/// acknowledgement was answered `N`; from 21 ms on it was accepted.
pub const STEP_SETTLE: Duration = Duration::from_millis(40);

/// Minimum time between two `UP`/`DW` commands.
///
/// On firmware 1.02 a second step 5 to 15 ms after the first was acknowledged
/// and dropped; 20 ms apart both applied.
pub const STEP_SPACING: Duration = Duration::from_millis(50);

impl<T: Transport> Radio<T> {
    /// Read the serial number and model code (`AE`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_serial_information(&mut self) -> Result<SerialInformation, Error> {
        self.query(
            Command::SerialInformation,
            "SerialInformation",
            |response| match response {
                Response::SerialInformation(information) => Some(information.clone()),
                _ => None,
            },
        )
        .await
    }

    /// Read whether the radio reports itself powered on (`PS`).
    ///
    /// The crate sends no `PS` write.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_power_status(&mut self) -> Result<bool, Error> {
        self.query(
            Command::PowerStatus,
            "PowerStatus",
            |response| match response {
                Response::PowerStatus { on } => Some(*on),
                _ => None,
            },
        )
        .await
    }

    /// Read the radio's clock (`RT`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_real_time_clock(&mut self) -> Result<RealTimeClock, Error> {
        self.query(
            Command::RealTimeClock,
            "RealTimeClock",
            |response| match response {
                Response::RealTimeClock(clock) => Some(*clock),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's frequency (`FQ`).
    ///
    /// In Memory, CALL and DR tuning modes the reply is the selected
    /// channel's receive frequency.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_frequency(&mut self, band: Band) -> Result<Frequency, Error> {
        self.query(
            Command::GetFrequency { band },
            "Frequency",
            |response| match response {
                Response::Frequency { frequency, .. } => Some(*frequency),
                _ => None,
            },
        )
        .await
    }

    /// Tune one band's VFO (`FQ band,hz`) with echo and readback.
    ///
    /// On firmware 1.02, tuning Band B from 223.000 MHz to 121.500 MHz switched
    /// its mode to AM, and tuning back to 223.000 MHz reported FM again.
    ///
    /// # Errors
    ///
    /// See the module contract; a frequency outside the band's range is
    /// answered `N`.
    pub async fn set_frequency(&mut self, band: Band, frequency: Frequency) -> Result<(), Error> {
        self.apply(
            Command::SetFrequency { band, frequency },
            Command::GetFrequency { band },
            "Frequency",
            frequency,
            |response| match response {
                Response::Frequency { frequency, .. } => Some(*frequency),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's complete channel record (`FO`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_channel_record(&mut self, band: Band) -> Result<CatChannelRecord, Error> {
        self.query(
            Command::GetChannelRecord { band },
            "ChannelRecord",
            |response| match response {
                Response::ChannelRecord { record, .. } => Some(record.clone()),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's transmit power (`PC`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_power_level(&mut self, band: Band) -> Result<PowerLevel, Error> {
        self.query(
            Command::GetPowerLevel { band },
            "PowerLevel",
            |response| match response {
                Response::PowerLevel { level, .. } => Some(*level),
                _ => None,
            },
        )
        .await
    }

    /// Select one band's transmit power (`PC band,level`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_power_level(&mut self, band: Band, level: PowerLevel) -> Result<(), Error> {
        self.apply(
            Command::SetPowerLevel { band, level },
            Command::GetPowerLevel { band },
            "PowerLevel",
            level,
            |response| match response {
                Response::PowerLevel { level, .. } => Some(*level),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's tuning mode (`VM`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_tuning_mode(&mut self, band: Band) -> Result<TuningMode, Error> {
        self.query(
            Command::GetTuningMode { band },
            "TuningMode",
            |response| match response {
                Response::TuningMode { mode, .. } => Some(*mode),
                _ => None,
            },
        )
        .await
    }

    /// Select one band's tuning mode (`VM band,mode`) with echo and readback.
    ///
    /// Leaving DR for VFO reported the band's previous VFO frequency and mode.
    /// Leaving Memory or CALL mode returns to the VFO of the frequency range
    /// the channel was in: Band B, whose VFO was 223.000 MHz, selected the
    /// 144.390 MHz APRS channel and then the 146.520 MHz CALL channel, and
    /// returning to VFO reported 144.000 MHz. Read the frequency afterwards
    /// and tune it back when the previous VFO matters.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_tuning_mode(&mut self, band: Band, mode: TuningMode) -> Result<(), Error> {
        self.apply(
            Command::SetTuningMode { band, mode },
            Command::GetTuningMode { band },
            "TuningMode",
            mode,
            |response| match response {
                Response::TuningMode { mode, .. } => Some(*mode),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's squelch level (`SQ`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_squelch(&mut self, band: Band) -> Result<SquelchLevel, Error> {
        self.query(
            Command::GetSquelch { band },
            "Squelch",
            |response| match response {
                Response::Squelch { level, .. } => Some(*level),
                _ => None,
            },
        )
        .await
    }

    /// Set one band's squelch level (`SQ band,level`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_squelch(&mut self, band: Band, level: SquelchLevel) -> Result<(), Error> {
        self.apply(
            Command::SetSquelch { band, level },
            Command::GetSquelch { band },
            "Squelch",
            level,
            |response| match response {
                Response::Squelch { level, .. } => Some(*level),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's signal strength (`SM`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_smeter(&mut self, band: Band) -> Result<SMeterReading, Error> {
        self.query(
            Command::GetSmeter { band },
            "Smeter",
            |response| match response {
                Response::Smeter { reading, .. } => Some(*reading),
                _ => None,
            },
        )
        .await
    }

    /// Read whether one band's squelch is open (`BY`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_busy(&mut self, band: Band) -> Result<bool, Error> {
        self.query(
            Command::GetBusy { band },
            "Busy",
            |response| match response {
                Response::Busy { busy, .. } => Some(*busy),
                _ => None,
            },
        )
        .await
    }

    /// Read whether one band's attenuator is on (`RA`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_attenuator(&mut self, band: Band) -> Result<bool, Error> {
        self.query(
            Command::GetAttenuator { band },
            "Attenuator",
            |response| match response {
                Response::Attenuator { enabled, .. } => Some(*enabled),
                _ => None,
            },
        )
        .await
    }

    /// Switch one band's attenuator (`RA band,0|1`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_attenuator(&mut self, band: Band, enabled: bool) -> Result<(), Error> {
        self.apply(
            Command::SetAttenuator { band, enabled },
            Command::GetAttenuator { band },
            "Attenuator",
            enabled,
            |response| match response {
                Response::Attenuator { enabled, .. } => Some(*enabled),
                _ => None,
            },
        )
        .await
    }

    /// Read one band's tuning step (`SF`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_step_size(&mut self, band: Band) -> Result<StepSize, Error> {
        self.query(
            Command::GetStepSize { band },
            "StepSize",
            |response| match response {
                Response::StepSize { step, .. } => Some(*step),
                _ => None,
            },
        )
        .await
    }

    /// Select one band's tuning step (`SF band,step`) with echo and readback.
    ///
    /// Selecting a step moves the VFO down to a multiple of it; read the
    /// frequency afterwards. 8.33 kHz is answered `N` outside the 118 MHz band.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_step_size(&mut self, band: Band, step: StepSize) -> Result<(), Error> {
        self.apply(
            Command::SetStepSize { band, step },
            Command::GetStepSize { band },
            "StepSize",
            step,
            |response| match response {
                Response::StepSize { step, .. } => Some(*step),
                _ => None,
            },
        )
        .await
    }

    /// Read the AM high-cut filter (`SH 0`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_am_high_cut(&mut self) -> Result<AmHighCut, Error> {
        self.query(
            Command::GetAmHighCut,
            "AmHighCut",
            |response| match response {
                Response::AmHighCut(cut) => Some(*cut),
                _ => None,
            },
        )
        .await
    }

    /// Select the AM high-cut filter (`SH 0,cut`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_am_high_cut(&mut self, cut: AmHighCut) -> Result<(), Error> {
        self.apply(
            Command::SetAmHighCut { cut },
            Command::GetAmHighCut,
            "AmHighCut",
            cut,
            |response| match response {
                Response::AmHighCut(cut) => Some(*cut),
                _ => None,
            },
        )
        .await
    }

    /// Step the control band up by its tuning step (`UP`) and read the new frequency.
    ///
    /// `UP` acts on the control band only, so the band roles are read first
    /// and `band` must be the control band. The bare command is the only
    /// accepted form; `UP 0` is rejected. The radio acknowledges the step
    /// within about 3 ms, drops a second step that follows within about
    /// 15 ms, and answers a setting write `N` for about 20 ms. This method
    /// therefore keeps [`STEP_SPACING`] since the previous step, reads the
    /// frequency before the step, reads it again up to
    /// [`STEP_READBACK_ATTEMPTS`] times [`STEP_READBACK_INTERVAL`] apart until
    /// it changes, and returns no sooner than [`STEP_SETTLE`] after the
    /// acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotControlBand`] when `band` is not the control band
    /// and [`Error::StepNotApplied`] when every readback reports the frequency
    /// read before the step; otherwise see the module contract.
    pub async fn frequency_up(&mut self, band: Band) -> Result<Frequency, Error> {
        self.step_control_band(band, Command::FrequencyUp).await
    }

    /// Step the control band down by its tuning step (`DW`) and read the new frequency.
    ///
    /// Same contract as [`Self::frequency_up`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotControlBand`] when `band` is not the control band
    /// and [`Error::StepNotApplied`] when every readback reports the frequency
    /// read before the step; otherwise see the module contract.
    pub async fn frequency_down(&mut self, band: Band) -> Result<Frequency, Error> {
        self.step_control_band(band, Command::FrequencyDown).await
    }

    async fn step_control_band(&mut self, band: Band, step: Command) -> Result<Frequency, Error> {
        self.require_qualified_write_target().await?;
        let roles = self.get_band_control().await?;
        if roles.control != band {
            return Err(Error::NotControlBand {
                band,
                control: roles.control,
            });
        }
        let before = self.get_frequency(band).await?;
        if let Some(previous) = self.last_step {
            tokio::time::sleep_until(previous + STEP_SPACING).await;
        }
        let mnemonic = step.mnemonic();
        self.query(step, "step acknowledgement", |response| match response {
            Response::FrequencyUpAck | Response::FrequencyDownAck => Some(()),
            _ => None,
        })
        .await?;
        let acknowledged = tokio::time::Instant::now();
        self.last_step = Some(acknowledged);
        for attempt in 1..=STEP_READBACK_ATTEMPTS {
            let after = self.get_frequency(band).await?;
            if after != before {
                tokio::time::sleep_until(acknowledged + STEP_SETTLE).await;
                return Ok(after);
            }
            if attempt < STEP_READBACK_ATTEMPTS {
                tokio::time::sleep(STEP_READBACK_INTERVAL).await;
            }
        }
        Err(Error::StepNotApplied {
            band,
            frequency: before,
            step: mnemonic,
        })
    }

    /// Read the memory channel one band has selected (`MR band`).
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::NotAvailable`] unless the band is in Memory
    /// tuning mode, otherwise see the module contract.
    pub async fn get_current_channel(
        &mut self,
        band: Band,
    ) -> Result<CurrentMemorySelector, Error> {
        self.query(
            Command::GetCurrentChannel { band },
            "CurrentChannel",
            |response| match response {
                Response::CurrentChannel(selector) => Some(*selector),
                _ => None,
            },
        )
        .await
    }

    /// Recall a stored memory channel on one band (`MR band,address`) and read
    /// the selection back.
    ///
    /// The band must be in Memory tuning mode and the channel must be stored;
    /// an empty channel is answered `N`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::NotAvailable`] for an empty channel or a band
    /// outside Memory mode, otherwise see the module contract.
    pub async fn recall_memory_channel(
        &mut self,
        band: Band,
        address: MemoryChannelAddress,
    ) -> Result<(), Error> {
        self.require_qualified_write_target().await?;
        self.query(
            Command::RecallMemoryChannel { band, address },
            "MemoryRecallAck",
            |response| match response {
                Response::MemoryRecallAck { .. } => Some(()),
                _ => None,
            },
        )
        .await?;
        let selected = self.get_current_channel(band).await?;
        if selected.address() == Some(address) {
            Ok(())
        } else {
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "readback equal to the request",
                actual: format!("band {band} selected {selected}, requested {address}"),
            }))
        }
    }

    /// Read a stored memory channel (`ME address`); `None` when it is empty.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_memory_channel(
        &mut self,
        address: MemoryChannelAddress,
    ) -> Result<Option<CatMemoryChannelRecord>, Error> {
        let command = Command::GetMemoryChannel { address };
        match self.command(&command).await? {
            Response::NotAvailable => Ok(None),
            Response::Rejected => Err(ProtocolError::Rejected {
                command: command.mnemonic(),
            }
            .into()),
            Response::MemoryChannel { record, .. } => Ok(Some(record)),
            other => Err(super::unexpected("MemoryChannel", &other)),
        }
    }

    /// Store a memory channel (`ME address,<record>`) with echo and readback,
    /// returning the record as the radio reads it back.
    ///
    /// The radio echoes the request exactly. Its readback equals the request
    /// except that an empty URCALL reads back as `CQCQCQ`; the returned
    /// record is that readback. Regular and program scan channels accept the
    /// write; the priority channel answers `N`. The stored channel has no
    /// name, which only an MCP write sets. Writing over a stored channel
    /// replaces it.
    ///
    /// # Errors
    ///
    /// See the module contract; a readback that differs from the request in
    /// any other field is [`ProtocolError::UnexpectedResponse`].
    pub async fn write_memory_channel(
        &mut self,
        address: MemoryChannelAddress,
        record: CatMemoryChannelRecord,
    ) -> Result<CatMemoryChannelRecord, Error> {
        self.require_qualified_write_target().await?;
        let extract = |response: &Response| match response {
            Response::MemoryChannel { record, .. } => Some(record.clone()),
            _ => None,
        };
        let echoed = self
            .query(
                Command::WriteMemoryChannel {
                    address,
                    record: record.clone(),
                },
                "MemoryChannel",
                extract,
            )
            .await?;
        if echoed != record {
            return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "write echo equal to the request",
                actual: format!("ME {address} echoed {echoed:?}, requested {record:?}"),
            }));
        }
        let observed = self
            .query(
                Command::GetMemoryChannel { address },
                "MemoryChannel",
                extract,
            )
            .await?;
        let mut expected = record;
        if expected.channel.ur_call.as_str().is_empty() {
            expected.channel.ur_call = UrCallsign::new("CQCQCQ")?;
        }
        if observed == expected {
            Ok(observed)
        } else {
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "readback equal to the request",
                actual: format!("ME {address} read back {observed:?}, requested {expected:?}"),
            }))
        }
    }

    /// Clear a memory channel (`ME address,`) and confirm it reads back empty.
    ///
    /// The radio acknowledges with the bare address and the channel then
    /// answers `N`. Clearing an empty channel is acknowledged the same way.
    /// The priority channel answers `N` to the clear itself.
    ///
    /// # Errors
    ///
    /// See the module contract; a channel that still reads back after the
    /// acknowledgement is [`ProtocolError::UnexpectedResponse`].
    pub async fn clear_memory_channel(
        &mut self,
        address: MemoryChannelAddress,
    ) -> Result<(), Error> {
        self.require_qualified_write_target().await?;
        self.query(
            Command::ClearMemoryChannel { address },
            "MemoryChannelCleared",
            |response| match response {
                Response::MemoryChannelCleared { .. } => Some(()),
                _ => None,
            },
        )
        .await?;
        let readback = Command::GetMemoryChannel { address };
        match self.command(&readback).await? {
            Response::NotAvailable => Ok(()),
            Response::Rejected => Err(ProtocolError::Rejected {
                command: readback.mnemonic(),
            }
            .into()),
            other => Err(super::unexpected("empty channel readback", &other)),
        }
    }

    /// Read one D-STAR MY callsign slot (`DC slot`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_dstar_callsign(
        &mut self,
        slot: DstarSlot,
    ) -> Result<DstarCallsignEntry, Error> {
        self.query(
            Command::GetDstarCallsign { slot },
            "DstarCallsign",
            |response| match response {
                Response::DstarCallsign(entry) => Some(entry.clone()),
                _ => None,
            },
        )
        .await
    }

    /// Read the control and PTT bands (`BC`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_band_control(&mut self) -> Result<BandControl, Error> {
        self.query(
            Command::GetBandControl,
            "BandControl",
            |response| match response {
                Response::BandControl(roles) => Some(*roles),
                _ => None,
            },
        )
        .await
    }

    /// Select the control and PTT bands (`BC control,ptt`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_band_control(&mut self, roles: BandControl) -> Result<(), Error> {
        self.apply(
            Command::SetBandControl { roles },
            Command::GetBandControl,
            "BandControl",
            roles,
            |response| match response {
                Response::BandControl(roles) => Some(*roles),
                _ => None,
            },
        )
        .await
    }

    /// Read the dual or single band display (`DL`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_band_display(&mut self) -> Result<BandDisplay, Error> {
        self.query(
            Command::GetBandDisplay,
            "BandDisplay",
            |response| match response {
                Response::BandDisplay(display) => Some(*display),
                _ => None,
            },
        )
        .await
    }

    /// Select the dual or single band display (`DL 0|1`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_band_display(&mut self, display: BandDisplay) -> Result<(), Error> {
        self.apply(
            Command::SetBandDisplay { display },
            Command::GetBandDisplay,
            "BandDisplay",
            display,
            |response| match response {
                Response::BandDisplay(display) => Some(*display),
                _ => None,
            },
        )
        .await
    }

    /// Read the selected D-STAR MY callsign slot (`DS`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_dstar_slot(&mut self) -> Result<DstarSlot, Error> {
        self.query(
            Command::GetDstarSlot,
            "DstarSlot",
            |response| match response {
                Response::DstarSlot(slot) => Some(*slot),
                _ => None,
            },
        )
        .await
    }

    /// Select the D-STAR MY callsign slot (`DS slot`) with echo and readback.
    ///
    /// An empty slot is accepted.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_dstar_slot(&mut self, slot: DstarSlot) -> Result<(), Error> {
        self.apply(
            Command::SetDstarSlot { slot },
            Command::GetDstarSlot,
            "DstarSlot",
            slot,
            |response| match response {
                Response::DstarSlot(slot) => Some(*slot),
                _ => None,
            },
        )
        .await
    }

    /// Read the panel lighting setting (`LC`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_backlight_control(&mut self) -> Result<BacklightControl, Error> {
        self.query(
            Command::GetBacklightControl,
            "BacklightControl",
            |response| match response {
                Response::BacklightControl(control) => Some(*control),
                _ => None,
            },
        )
        .await
    }

    /// Select the panel lighting setting (`LC value`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_backlight_control(&mut self, control: BacklightControl) -> Result<(), Error> {
        self.apply(
            Command::SetBacklightControl { control },
            Command::GetBacklightControl,
            "BacklightControl",
            control,
            |response| match response {
                Response::BacklightControl(control) => Some(*control),
                _ => None,
            },
        )
        .await
    }

    /// Read the APRS position source (`MS`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_my_position_selection(&mut self) -> Result<MyPositionSelection, Error> {
        self.query(
            Command::GetMyPositionSelection,
            "MyPositionSelection",
            |response| match response {
                Response::MyPositionSelection(selection) => Some(*selection),
                _ => None,
            },
        )
        .await
    }

    /// Select the APRS position source (`MS selection`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_my_position_selection(
        &mut self,
        selection: MyPositionSelection,
    ) -> Result<(), Error> {
        self.apply(
            Command::SetMyPositionSelection { selection },
            Command::GetMyPositionSelection,
            "MyPositionSelection",
            selection,
            |response| match response {
                Response::MyPositionSelection(selection) => Some(*selection),
                _ => None,
            },
        )
        .await
    }

    /// Read the packet data speed (`AS`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_packet_data_rate(&mut self) -> Result<PacketDataRate, Error> {
        self.query(
            Command::GetPacketDataRate,
            "PacketDataRate",
            |response| match response {
                Response::PacketDataRate(rate) => Some(*rate),
                _ => None,
            },
        )
        .await
    }

    /// Select the packet data speed (`AS rate`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_packet_data_rate(&mut self, rate: PacketDataRate) -> Result<(), Error> {
        self.apply(
            Command::SetPacketDataRate { rate },
            Command::GetPacketDataRate,
            "PacketDataRate",
            rate,
            |response| match response {
                Response::PacketDataRate(rate) => Some(*rate),
                _ => None,
            },
        )
        .await
    }

    /// Read the APRS beacon method (`PT`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_beacon_method(&mut self) -> Result<BeaconMethod, Error> {
        self.query(
            Command::GetBeaconMethod,
            "BeaconMethod",
            |response| match response {
                Response::BeaconMethod(method) => Some(*method),
                _ => None,
            },
        )
        .await
    }

    /// Select the APRS beacon method (`PT method`) with echo and readback.
    ///
    /// With the TNC in APRS mode, `Auto` and `SmartBeaconing` transmit
    /// without further operator action.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_beacon_method(&mut self, method: BeaconMethod) -> Result<(), Error> {
        self.apply(
            Command::SetBeaconMethod { method },
            Command::GetBeaconMethod,
            "BeaconMethod",
            method,
            |response| match response {
                Response::BeaconMethod(method) => Some(*method),
                _ => None,
            },
        )
        .await
    }

    /// Read the TNC mode and its data band (`TN`).
    ///
    /// The crate sends no `TN` write.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_tnc_mode(&mut self) -> Result<(TncMode, Band), Error> {
        self.query(Command::GetTncMode, "TncMode", |response| match response {
            Response::TncMode { mode, data_band } => Some((*mode, *data_band)),
            _ => None,
        })
        .await
    }

    /// Read the VOX delay (`VD`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_vox_delay(&mut self) -> Result<VoxDelay, Error> {
        self.query(
            Command::GetVoxDelay,
            "VoxDelay",
            |response| match response {
                Response::VoxDelay(delay) => Some(*delay),
                _ => None,
            },
        )
        .await
    }

    /// Select the VOX delay (`VD index`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_vox_delay(&mut self, delay: VoxDelay) -> Result<(), Error> {
        self.apply(
            Command::SetVoxDelay { delay },
            Command::GetVoxDelay,
            "VoxDelay",
            delay,
            |response| match response {
                Response::VoxDelay(delay) => Some(*delay),
                _ => None,
            },
        )
        .await
    }

    /// Read the VOX gain (`VG`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_vox_gain(&mut self) -> Result<VoxGain, Error> {
        self.query(Command::GetVoxGain, "VoxGain", |response| match response {
            Response::VoxGain(gain) => Some(*gain),
            _ => None,
        })
        .await
    }

    /// Select the VOX gain (`VG gain`) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_vox_gain(&mut self, gain: VoxGain) -> Result<(), Error> {
        self.apply(
            Command::SetVoxGain { gain },
            Command::GetVoxGain,
            "VoxGain",
            gain,
            |response| match response {
                Response::VoxGain(gain) => Some(*gain),
                _ => None,
            },
        )
        .await
    }

    /// Read the VOX state (`VX`).
    ///
    /// The crate sends no `VX` write: with a microphone connected, VOX can key
    /// the transmitter.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_vox(&mut self) -> Result<VoxMode, Error> {
        self.query(Command::GetVox, "Vox", |response| match response {
            Response::Vox(mode) => Some(*mode),
            _ => None,
        })
        .await
    }

    /// Read the GPS receiver settings (`GP`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_gps_settings(&mut self) -> Result<GpsSettings, Error> {
        self.query(
            Command::GetGpsSettings,
            "GpsSettings",
            |response| match response {
                Response::GpsSettings(settings) => Some(*settings),
                _ => None,
            },
        )
        .await
    }

    /// Select the GPS receiver settings (`GP gps,pc`) with echo and readback.
    ///
    /// Switching the receiver off discards its fix.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_gps_settings(&mut self, settings: GpsSettings) -> Result<(), Error> {
        self.apply(
            Command::SetGpsSettings { settings },
            Command::GetGpsSettings,
            "GpsSettings",
            settings,
            |response| match response {
                Response::GpsSettings(settings) => Some(*settings),
                _ => None,
            },
        )
        .await
    }

    /// Read the NMEA sentence selection (`GS`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_gps_sentences(&mut self) -> Result<NmeaSentences, Error> {
        self.query(
            Command::GetGpsSentences,
            "GpsSentences",
            |response| match response {
                Response::GpsSentences(sentences) => Some(*sentences),
                _ => None,
            },
        )
        .await
    }

    /// Select the NMEA sentences (`GS` with six flags) with echo and readback.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_gps_sentences(&mut self, sentences: NmeaSentences) -> Result<(), Error> {
        self.apply(
            Command::SetGpsSentences { sentences },
            Command::GetGpsSentences,
            "GpsSentences",
            sentences,
            |response| match response {
                Response::GpsSentences(sentences) => Some(*sentences),
                _ => None,
            },
        )
        .await
    }

    /// Read whether Bluetooth is on (`BT`).
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn get_bluetooth(&mut self) -> Result<bool, Error> {
        self.query(
            Command::GetBluetooth,
            "Bluetooth",
            |response| match response {
                Response::Bluetooth { enabled } => Some(*enabled),
                _ => None,
            },
        )
        .await
    }

    /// Switch Bluetooth (`BT 0|1`) with echo and readback.
    ///
    /// Switching it on restarts the Bluetooth stack; the echo waits up to
    /// [`super::BLUETOOTH_WRITE_TIMEOUT`]. The write was exercised over USB;
    /// over a Bluetooth CAT connection, switching it off ends that connection.
    ///
    /// # Errors
    ///
    /// See the module contract.
    pub async fn set_bluetooth(&mut self, enabled: bool) -> Result<(), Error> {
        self.apply(
            Command::SetBluetooth { enabled },
            Command::GetBluetooth,
            "Bluetooth",
            enabled,
            |response| match response {
                Response::Bluetooth { enabled } => Some(*enabled),
                _ => None,
            },
        )
        .await
    }
}
