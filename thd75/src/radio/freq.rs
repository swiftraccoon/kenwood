//! Core radio methods: frequency, mode, power, squelch, S-meter, TX/RX, firmware, power status, ID,
//! band control, VFO/memory mode, FM radio, fine step, function type, and filter width.
//!
//! # Band capabilities (per Operating Tips §5.9, §5.10)
//!
//! - **Band A**: 144 / 220 (A only) / 430 MHz amateur operation
//! - **Band B**: 0.1-524 MHz wideband receive, all modes (FM, NFM, AM, LSB, USB, CW, DV, DR)
//! - **TH-D75A TX ranges**: 144-148 MHz, 222-225 MHz, 430-450 MHz
//! - **TH-D75E TX ranges**: 144-146 MHz, 430-440 MHz
//!
//! # IF signal output (per Operating Tips §5.10)
//!
//! Menu No. 102 enables IF (Intermediate Frequency) signal output via the USB
//! port: 12 kHz center frequency, 15 kHz bandwidth. This is intended for
//! SSB/CW/AM demodulation by a PC application. Single Band mode is required
//! for IF/Detect output. A band scope can be driven via a third-party PC
//! application using the BS command.
//!
//! # FQ vs FO
//!
//! The D75 has two frequency-related command pairs:
//!
//! - **FQ** (read-only): returns the current frequency and step size for a band. Writes are
//!   rejected by the firmware — use FO for frequency changes.
//! - **FO** (read/write): returns or sets the full channel configuration for a band, including
//!   frequency, offset, tone mode, CTCSS/DCS codes, shift direction, and more. This is the
//!   primary command for tuning the radio via CAT.
//!
//! # VFO mode requirement
//!
//! Most write commands in this module (FO write, MD write, SQ write, FS write, etc.) require the
//! target band to be in VFO mode. If the band is in Memory, Call, or WX mode, the radio returns
//! `?` and the write is silently rejected. Use [`set_vfo_memory_mode`](Radio::set_vfo_memory_mode)
//! to switch to VFO mode first, or use the safe `tune_frequency()` API which handles mode
//! management automatically.
//!
//! # Tone and offset configuration
//!
//! CTCSS tone, DCS code, tone mode, and repeater offset are not configured through dedicated
//! commands. Instead, they are fields within the [`ChannelMemory`] struct passed to
//! [`set_frequency_full`](Radio::set_frequency_full) (FO write). Read the current state with
//! [`get_frequency_full`](Radio::get_frequency_full), modify the desired fields, and write it back.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{
    Band, ChannelMemory, FilterMode, FilterWidthIndex, FineStep, Mode, PowerLevel, SMeterReading,
    SquelchLevel, VfoMemoryMode,
};

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
    /// Sends the full channel configuration (frequency, offset, tone mode, CTCSS/DCS codes,
    /// shift direction, and other fields) to the radio for the specified band.
    ///
    /// # VFO mode requirement
    ///
    /// The target band **must** be in VFO mode (`VM band,0`). If the band is in Memory, Call,
    /// or WX mode, the radio returns `?` and the write is silently rejected. Use
    /// [`set_vfo_memory_mode`](Self::set_vfo_memory_mode) to switch to VFO first, or prefer
    /// `tune_frequency()` which handles mode management safely.
    ///
    /// # Wire format
    ///
    /// `FO band,freq,step,shift,reverse,tone_status,ctcss_status,dcs_status,tone_freq,ctcss_freq,dcs_code,offset,...\r`
    ///
    /// The full FO command encodes all 21 fields of the [`ChannelMemory`] struct as
    /// comma-separated values.
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
    /// # Band restrictions
    ///
    /// SSB (LSB/USB), CW, and AM modes are only available on Band B. Attempting to set these
    /// modes on Band A will return `?`. FM, NFM, DV, and DR modes are available on both bands.
    ///
    /// See the [`Mode`] type for valid values. Note that the MD command uses a different
    /// encoding than FO/ME commands — the [`Mode`] type handles this mapping internally.
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
    pub async fn get_squelch(&mut self, band: Band) -> Result<SquelchLevel, Error> {
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
    /// # Valid range
    ///
    /// `level` must be 0 through 6 on the TH-D75. Values outside this range cause the radio
    /// to return `?` and the write is rejected. Level 0 means squelch is fully open (all signals
    /// pass); level 6 is the tightest squelch setting.
    ///
    /// # Wire format
    ///
    /// `SQ band,level\r` where band is 0 (A) or 1 (B) and level is a single digit 0-6.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_squelch(&mut self, band: Band, level: SquelchLevel) -> Result<(), Error> {
        tracing::debug!(?band, ?level, "setting squelch level");
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
    /// Returns an instantaneous signal strength measurement as a raw value 0-5. This is a
    /// read-only, point-in-time snapshot — the value changes continuously as signal conditions
    /// vary.
    ///
    /// # Value mapping
    ///
    /// The raw values map to approximate S-meter readings:
    ///
    /// | Raw | S-meter |
    /// |-----|---------|
    /// |  0  | S0 (no signal) |
    /// |  1  | S1 |
    /// |  2  | S3 |
    /// |  3  | S5 |
    /// |  4  | S7 |
    /// |  5  | S9 (full scale) |
    ///
    /// # Polling warning
    ///
    /// Do not poll SM continuously — the firmware returns spurious spikes on Band B. Instead,
    /// use AI mode ([`set_auto_info`](Self::set_auto_info)) with the BY (busy) signal as a
    /// gate: read SM once when squelch opens, and treat it as zero when squelch is closed.
    ///
    /// # Wire format
    ///
    /// `SM band\r` returns `SM band,level\r`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_smeter(&mut self, band: Band) -> Result<SMeterReading, Error> {
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
    /// "Busy" means the squelch is open — a signal strong enough to exceed the current squelch
    /// threshold is present on the channel. Returns `true` when the squelch is open (signal
    /// present), `false` when closed (no signal or signal below threshold).
    ///
    /// # Wire format
    ///
    /// `BY band\r` returns `BY band,state\r` where state is 0 (not busy / squelch closed) or
    /// 1 (busy / squelch open).
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
    /// # RF emission warning
    ///
    /// **This keys the transmitter and causes RF emission on the currently tuned frequency.**
    /// The radio will transmit continuously until [`receive`](Self::receive) is called. Ensure
    /// you are authorized to transmit on the current frequency before calling this method.
    /// Unauthorized transmission is a violation of radio regulations (e.g., FCC Part 97 in the
    /// US).
    ///
    /// Always call [`receive`](Self::receive) when done to return to receive mode. If your
    /// program panics or is interrupted while transmitting, the radio will continue to transmit
    /// until manually stopped or the timeout (if any) expires.
    ///
    /// # Wire format
    ///
    /// `TX band\r` where band is 0 (A) or 1 (B). Returns `OK\r` on success.
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
    /// Stops transmitting and returns the radio to receive mode. This is the counterpart to
    /// [`transmit`](Self::transmit) and **must** be called after transmitting to stop RF
    /// emission.
    ///
    /// # Wire format
    ///
    /// `RX band\r` where band is 0 (A) or 1 (B). Returns `OK\r` on success.
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
    pub async fn get_vfo_memory_mode(&mut self, band: Band) -> Result<VfoMemoryMode, Error> {
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
    pub async fn set_vfo_memory_mode(
        &mut self,
        band: Band,
        mode: VfoMemoryMode,
    ) -> Result<(), Error> {
        tracing::info!(?band, ?mode, "setting VFO/Memory mode");
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
    pub async fn frequency_up(&mut self, band: Band) -> Result<(), Error> {
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

    /// Get the FM broadcast radio on/off state (FR read).
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

    /// Set the FM broadcast radio on/off state (FR write).
    ///
    /// This controls the **broadcast FM receiver** (76-108 MHz), not amateur FM mode. This is
    /// the same as the "FM Radio" menu item on the radio — it tunes to commercial broadcast
    /// stations.
    ///
    /// # Side effects
    ///
    /// Enabling the FM broadcast receiver takes over the display and audio output. The radio's
    /// normal amateur band display is replaced with the broadcast FM frequency. Normal band
    /// receive audio is muted while the FM broadcast receiver is active. Disable it to return
    /// to normal amateur radio operation.
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

    /// Get the fine step setting (FS bare read).
    ///
    /// Firmware-verified: FS = Fine Step. Bare `FS\r` returns a single value (0-3).
    /// No band parameter — the radio returns a global fine step setting.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_fine_step(&mut self) -> Result<FineStep, Error> {
        tracing::debug!("reading fine step");
        let response = self.execute(Command::GetFineStep).await?;
        match response {
            Response::FineStep { step } => Ok(step),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FineStep".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the fine step for a band (FS write).
    ///
    /// Firmware-verified: `FS band,step\r` (band 0-1, step 0-3).
    ///
    /// # Firmware bug (v1.03)
    ///
    /// FS write is broken on firmware 1.03 — the radio returns `N`
    /// (not available) for all write attempts.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_fine_step(&mut self, band: Band, step: FineStep) -> Result<(), Error> {
        tracing::info!(?band, ?step, "setting fine step");
        let response = self.execute(Command::SetFineStep { band, step }).await?;
        match response {
            Response::FineStep { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FineStep".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the function type value (FT read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_function_type(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading function type (fine tune)");
        let response = self.execute(Command::GetFunctionType).await?;
        match response {
            Response::FunctionType { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FunctionType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set fine tune on/off for a band (FT write).
    ///
    /// Per Operating Tips section 5.10.6: Fine Tune only works with AM modulation
    /// and Band B. The write form takes a band parameter unlike the bare read.
    ///
    /// # Wire format
    ///
    /// `FT band,value\r` where band is 0 (A) or 1 (B) and value is 0 (off) or 1 (on).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_function_type(&mut self, band: Band, enabled: bool) -> Result<(), Error> {
        tracing::info!(?band, enabled, "setting fine tune (FT)");
        let response = self
            .execute(Command::SetFunctionType { band, enabled })
            .await?;
        match response {
            Response::FunctionType { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FunctionType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the S-meter value for a band (SM write) -- calibration/test interface.
    ///
    /// # Warning
    ///
    /// This is likely a calibration or test/debug interface. Setting the S-meter
    /// value directly may interfere with normal signal strength readings. The
    /// exact behavior and persistence of written values is undocumented.
    ///
    /// # Wire format
    ///
    /// `SM band,level\r` where band is 0 (A) or 1 (B) and level is a hex nibble value.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_smeter(&mut self, band: Band, level: SMeterReading) -> Result<(), Error> {
        tracing::info!(?band, ?level, "setting S-meter (SM write, calibration)");
        let response = self.execute(Command::SetSmeter { band, level }).await?;
        match response {
            Response::Smeter { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Smeter".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the busy/squelch state for a band (BY write) -- test/debug interface.
    ///
    /// # Warning
    ///
    /// This is likely a test or debug interface. Setting the busy state directly
    /// may interfere with normal squelch operation. Use with caution.
    ///
    /// # Wire format
    ///
    /// `BY band,state\r` where band is 0 (A) or 1 (B) and state is 0 (not busy)
    /// or 1 (busy).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_busy(&mut self, band: Band, busy: bool) -> Result<(), Error> {
        tracing::info!(?band, busy, "setting busy state (BY write, test/debug)");
        let response = self.execute(Command::SetBusy { band, busy }).await?;
        match response {
            Response::Busy { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Busy".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the firmware version string (FV write) -- factory programming command.
    ///
    /// # Safety
    ///
    /// **DANGEROUS FACTORY COMMAND.** This is intended for factory programming
    /// only. Writing an incorrect firmware version string may brick the radio,
    /// cause firmware validation failures, or void your warranty. **Do not use
    /// unless you fully understand the consequences.**
    ///
    /// # Wire format
    ///
    /// `FV version\r`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_firmware_version(&mut self, version: &str) -> Result<(), Error> {
        tracing::warn!(version, "setting firmware version (FACTORY COMMAND)");
        let response = self
            .execute(Command::SetFirmwareVersion {
                version: version.to_owned(),
            })
            .await?;
        match response {
            Response::FirmwareVersion { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FirmwareVersion".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the radio model identification string (ID write) -- factory programming command.
    ///
    /// # Safety
    ///
    /// **DANGEROUS FACTORY COMMAND.** This is intended for factory programming
    /// only. Writing an incorrect model ID may cause the radio to behave as a
    /// different model, disable features, or brick the device. **Do not use
    /// unless you fully understand the consequences.**
    ///
    /// # Wire format
    ///
    /// `ID model\r`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_radio_id(&mut self, model: &str) -> Result<(), Error> {
        tracing::warn!(model, "setting radio model ID (FACTORY COMMAND)");
        let response = self
            .execute(Command::SetRadioId {
                model: model.to_owned(),
            })
            .await?;
        match response {
            Response::RadioId { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RadioId".into(),
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
    pub async fn get_filter_width(&mut self, mode: FilterMode) -> Result<FilterWidthIndex, Error> {
        tracing::debug!(?mode, "reading filter width");
        let response = self.execute(Command::GetFilterWidth { mode }).await?;
        match response {
            Response::FilterWidth { width, .. } => Ok(width),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FilterWidth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the filter width for a given mode index (SH write).
    ///
    /// `mode_index`: 0 = SSB, 1 = CW, 2 = AM. The width value selects
    /// from the available filter options for that mode (per Operating
    /// Tips §5.10.1–§5.10.3).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_filter_width(
        &mut self,
        mode: FilterMode,
        width: FilterWidthIndex,
    ) -> Result<(), Error> {
        tracing::info!(?mode, ?width, "setting filter width");
        let response = self
            .execute(Command::SetFilterWidth { mode, width })
            .await?;
        match response {
            Response::FilterWidth { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FilterWidth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
