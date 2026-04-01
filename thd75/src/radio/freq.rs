//! Core radio methods: frequency, mode, power, squelch, S-meter, TX/RX, firmware, power status, ID,
//! band control, VFO/memory mode, FM radio, frequency step, function type, and filter width.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{Band, ChannelMemory, Mode, PowerLevel, StepSize};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read the current frequency data for the given band (FQ read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_frequency(&mut self, band: Band) -> Result<ChannelMemory, Error> {
        tracing::debug!(?band, "reading frequency data");
        let response = self.execute(Command::GetFrequency { band }).await?;
        match response {
            Response::Frequency { channel, .. } => Ok(channel),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Frequency".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read the full frequency and settings for the given band (FO read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_frequency_full(&mut self, band: Band) -> Result<ChannelMemory, Error> {
        tracing::debug!(?band, "reading full frequency data");
        let response = self.execute(Command::GetFrequencyFull { band }).await?;
        match response {
            Response::FrequencyFull { channel, .. } => Ok(channel),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyFull".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write full frequency and settings for the given band (FO write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_frequency_full(
        &mut self,
        band: Band,
        channel: &ChannelMemory,
    ) -> Result<(), Error> {
        tracing::debug!(?band, "writing full frequency data");
        let response = self
            .execute(Command::SetFrequencyFull {
                band,
                channel: channel.clone(),
            })
            .await?;
        match response {
            Response::FrequencyFull { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyFull".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the operating mode for the given band (MD read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_mode(&mut self, band: Band) -> Result<Mode, Error> {
        tracing::debug!(?band, "reading operating mode");
        let response = self.execute(Command::GetMode { band }).await?;
        match response {
            Response::Mode { mode, .. } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Mode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the operating mode for the given band (MD write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_mode(&mut self, band: Band, mode: Mode) -> Result<(), Error> {
        tracing::debug!(?band, ?mode, "setting operating mode");
        let response = self.execute(Command::SetMode { band, mode }).await?;
        match response {
            Response::Mode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Mode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the power level for the given band (PC read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_power_level(&mut self, band: Band) -> Result<PowerLevel, Error> {
        tracing::debug!(?band, "reading power level");
        let response = self.execute(Command::GetPowerLevel { band }).await?;
        match response {
            Response::PowerLevel { level, .. } => Ok(level),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "PowerLevel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the power level for the given band (PC write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_power_level(&mut self, band: Band, level: PowerLevel) -> Result<(), Error> {
        tracing::debug!(?band, ?level, "setting power level");
        let response = self.execute(Command::SetPowerLevel { band, level }).await?;
        match response {
            Response::PowerLevel { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "PowerLevel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the squelch level for the given band (SQ read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_squelch(&mut self, band: Band) -> Result<u8, Error> {
        tracing::debug!(?band, "reading squelch level");
        let response = self.execute(Command::GetSquelch { band }).await?;
        match response {
            Response::Squelch { level, .. } => Ok(level),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Squelch".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the squelch level for the given band (SQ write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_squelch(&mut self, band: Band, level: u8) -> Result<(), Error> {
        tracing::debug!(?band, level, "setting squelch level");
        let response = self.execute(Command::SetSquelch { band, level }).await?;
        match response {
            Response::Squelch { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Squelch".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the S-meter reading for the given band (SM read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_smeter(&mut self, band: Band) -> Result<u8, Error> {
        tracing::debug!(?band, "reading S-meter");
        let response = self.execute(Command::GetSmeter { band }).await?;
        match response {
            Response::Smeter { level, .. } => Ok(level),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Smeter".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the busy state for the given band (BY read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_busy(&mut self, band: Band) -> Result<bool, Error> {
        tracing::debug!(?band, "reading busy state");
        let response = self.execute(Command::GetBusy { band }).await?;
        match response {
            Response::Busy { busy, .. } => Ok(busy),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Busy".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Switch the given band to transmit mode (TX action).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn transmit(&mut self, band: Band) -> Result<(), Error> {
        tracing::info!(?band, "keying transmitter");
        let response = self.execute(Command::Transmit { band }).await?;
        match response {
            Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Ok".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Switch the given band to receive mode (RX action).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn receive(&mut self, band: Band) -> Result<(), Error> {
        tracing::info!(?band, "returning to receive");
        let response = self.execute(Command::Receive { band }).await?;
        match response {
            Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Ok".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the firmware version string (FV read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_firmware_version(&mut self) -> Result<String, Error> {
        tracing::debug!("reading firmware version");
        let response = self.execute(Command::GetFirmwareVersion).await?;
        match response {
            Response::FirmwareVersion { version } => Ok(version),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FirmwareVersion".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the power on/off status (PS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_power_status(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading power status");
        let response = self.execute(Command::GetPowerStatus).await?;
        match response {
            Response::PowerStatus { on } => Ok(on),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "PowerStatus".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the radio model identification string (ID read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_radio_id(&mut self) -> Result<String, Error> {
        tracing::debug!("reading radio ID");
        let response = self.execute(Command::GetRadioId).await?;
        match response {
            Response::RadioId { model } => Ok(model),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RadioId".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the current active band (BC read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_band(&mut self) -> Result<Band, Error> {
        tracing::debug!("reading active band");
        let response = self.execute(Command::GetBand).await?;
        match response {
            Response::BandResponse { band } => Ok(band),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandResponse".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the active band (BC write).
    ///
    /// # Warning
    /// This is an ACTION command that switches the radio's active band.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_band(&mut self, band: Band) -> Result<(), Error> {
        tracing::info!(?band, "setting active band");
        let response = self.execute(Command::SetBand { band }).await?;
        match response {
            Response::BandResponse { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandResponse".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the VFO/Memory mode for a band (VM read).
    ///
    /// Returns a mode index: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_vfo_memory_mode(&mut self, band: Band) -> Result<u8, Error> {
        tracing::debug!(?band, "reading VFO/Memory mode");
        let response = self.execute(Command::GetVfoMemoryMode { band }).await?;
        match response {
            Response::VfoMemoryMode { mode, .. } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "VfoMemoryMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the VFO/Memory mode for a band (VM write).
    ///
    /// Mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_vfo_memory_mode(&mut self, band: Band, mode: u8) -> Result<(), Error> {
        tracing::info!(?band, mode, "setting VFO/Memory mode");
        let response = self
            .execute(Command::SetVfoMemoryMode { band, mode })
            .await?;
        match response {
            Response::VfoMemoryMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "VfoMemoryMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the current memory channel number for a band (MR read).
    ///
    /// Hardware-verified: `MR band\r` returns `MR bandCCC` where CCC is
    /// the channel number. This is a read that queries which channel is
    /// active, not an action that changes the channel.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_current_channel(&mut self, band: Band) -> Result<u16, Error> {
        tracing::debug!(?band, "reading current memory channel");
        let response = self.execute(Command::GetCurrentChannel { band }).await?;
        match response {
            Response::CurrentChannel { channel, .. } => Ok(channel),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "CurrentChannel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Recall a memory channel on the given band (MR action).
    ///
    /// This is an ACTION command that switches the radio's active channel.
    /// The previous channel selection is not preserved.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn recall_channel(&mut self, band: Band, channel: u16) -> Result<(), Error> {
        tracing::info!(?band, channel, "recalling memory channel");
        let response = self
            .execute(Command::RecallMemoryChannel { band, channel })
            .await?;
        match response {
            Response::MemoryRecall { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MemoryRecall".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Step the frequency up by one increment on the given band (UP action).
    ///
    /// This is an ACTION command that changes the radio's active frequency.
    /// There is no undo — the previous frequency is not preserved.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn freq_up(&mut self, band: Band) -> Result<(), Error> {
        tracing::info!(?band, "stepping frequency up");
        let response = self.execute(Command::FrequencyUp { band }).await?;
        match response {
            Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Ok".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the FM radio on/off state (FR read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_fm_radio(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading FM radio state");
        let response = self.execute(Command::GetFmRadio).await?;
        match response {
            Response::FmRadio { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FmRadio".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the FM radio on/off state (FR write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_fm_radio(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::info!(enabled, "setting FM radio state");
        let response = self.execute(Command::SetFmRadio { enabled }).await?;
        match response {
            Response::FmRadio { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FmRadio".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the frequency step size for a band (FS read).
    ///
    /// D75 RE: `FS x,y` (x: band, y: step index 0-11).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_frequency_step(&mut self, band: Band) -> Result<StepSize, Error> {
        tracing::debug!(?band, "reading frequency step");
        let response = self.execute(Command::GetFrequencyStep { band }).await?;
        match response {
            Response::FrequencyStep { step, .. } => Ok(step),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyStep".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the frequency step size for a band (FS write).
    ///
    /// D75 RE: `FS x,y` (x: band, y: step index 0-11).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_frequency_step(&mut self, band: Band, step: StepSize) -> Result<(), Error> {
        tracing::info!(?band, ?step, "setting frequency step");
        let response = self
            .execute(Command::SetFrequencyStep { band, step })
            .await?;
        match response {
            Response::FrequencyStep { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyStep".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the function type value (FT read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_function_type(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading function type");
        let response = self.execute(Command::GetFunctionType).await?;
        match response {
            Response::FunctionType { value } => Ok(value),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FunctionType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the filter width for a given mode index (SH read).
    ///
    /// `mode_index`: 0 = SSB, 1 = CW, 2 = AM.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_filter_width(&mut self, mode_index: u8) -> Result<u8, Error> {
        tracing::debug!(mode_index, "reading filter width");
        let response = self.execute(Command::GetFilterWidth { mode_index }).await?;
        match response {
            Response::FilterWidth { width, .. } => Ok(width),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FilterWidth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
