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
//! - **FQ** (read-only): returns exactly the band and its ten-digit frequency. It does not
//!   carry a step-size or full channel record.
//! - **FO** (read-only in this library): returns the full 20-field CAT channel record. The
//!   former writer was lossy and is quarantined until every field and its readback behavior
//!   have been qualified.
//!
//! # VFO mode requirement
//!
//! Band-indexed write commands generally require the target band to be in VFO mode. If the band
//! is in Memory, Call, or WX mode, the radio may reject the write. Use
//! [`set_vfo_memory_mode`](Radio::set_vfo_memory_mode) explicitly. Frequency tuning remains
//! unavailable while the complete FO write record is under qualification.
//!
//! # Tone and offset configuration
//!
//! CTCSS tone, DCS code, tone mode, and repeater offset are not configured through dedicated
//! commands. They are fields within the full FO record returned by
//! [`get_frequency_full`](Radio::get_frequency_full). Writing that record is intentionally
//! unavailable because a partial read-modify-write can alter unrelated radio state.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{
    Band, CatChannelRecord, ChannelMemory, FilterMode, FilterWidthIndex, FineStep, Frequency,
    MemorySelector, Mode, PowerLevel, SMeterReading, SquelchLevel, VfoMemoryMode,
};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read the current frequency data for the given band (FQ read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_frequency(&mut self, band: Band) -> Result<Frequency, Error> {
        tracing::debug!(?band, "reading frequency data");
        let response = self.execute(Command::GetFrequency { band }).await?;
        match response {
            Response::Frequency { frequency, .. } => Ok(frequency),
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
    pub async fn get_frequency_full(&mut self, band: Band) -> Result<CatChannelRecord, Error> {
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

    /// Attempt to write a full FO record.
    ///
    /// This writer is quarantined. The former codec emitted literal zeroes
    /// for transmit step, CAT mode, fine-enable, and fine-step. A seemingly
    /// harmless read-modify-write could therefore change DV to FM and clear
    /// fine tuning. It remains unavailable until every FO field is modeled
    /// and full-record readback is qualified on hardware.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    #[expect(
        clippy::unused_async,
        reason = "Compatibility quarantine: keep the existing async public API while returning \
                  before I/O until the full FO wire record is qualified"
    )]
    pub async fn set_frequency_full(
        &mut self,
        _band: Band,
        _channel: &ChannelMemory,
    ) -> Result<(), Error> {
        Err(Error::UnqualifiedCatWrite {
            command: "FO",
            reason: "the current channel model cannot preserve all 20 wire fields",
        })
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
    /// encoding than FO/ME commands; the [`Mode`] type handles this mapping internally.
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
    /// read-only, point-in-time snapshot; the value changes continuously as signal conditions
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
    /// Do not poll SM continuously: the firmware returns spurious spikes on Band B. Instead,
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
    /// "Busy" means the squelch is open: a signal strong enough to exceed the current squelch
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

    /// Switch the current operating context to transmit mode (bare TX action).
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
    /// `TX\r`. The command has no band parameter; select the active band
    /// separately before transmitting. Returns `OK\r` on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn transmit(&mut self) -> Result<(), Error> {
        tracing::info!("keying transmitter on the current operating context");
        let response = self.execute(Command::Transmit).await?;
        match response {
            Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Ok".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Switch the current operating context to receive mode (bare RX action).
    ///
    /// Stops transmitting and returns the radio to receive mode. This is the counterpart to
    /// [`transmit`](Self::transmit) and **must** be called after transmitting to stop RF
    /// emission.
    ///
    /// # Wire format
    ///
    /// `RX\r`. Returns `OK\r` on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn receive(&mut self) -> Result<(), Error> {
        tracing::info!("returning the current operating context to receive");
        let response = self.execute(Command::Receive).await?;
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

    /// Get the current memory selector for a band (MR read).
    ///
    /// Hardware-verified: the request is band-indexed, while the response is
    /// the selector alone (`MR 021`, `MR L00`, `MR Pri`, and so on). This is a
    /// read that queries which selector is active, not an action that changes
    /// the channel. A band with no active memory selector (for example, while
    /// operating in VFO mode) returns `N`, surfaced as [`Error::NotAvailable`].
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_current_channel(&mut self, band: Band) -> Result<MemorySelector, Error> {
        tracing::debug!(?band, "reading current memory selector");
        let response = self.execute(Command::GetCurrentChannel { band }).await?;
        match response {
            Response::CurrentChannel { selector } => Ok(selector),
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
        let selector = MemorySelector::try_from(channel)?;
        self.recall_memory(band, selector).await
    }

    /// Recall an exact memory selector on the given band (MR action).
    ///
    /// Supports ordinary channels, program-scan edges, regional selectors,
    /// and the priority channel without collapsing them to a fabricated
    /// numeric channel.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn recall_memory(
        &mut self,
        band: Band,
        selector: MemorySelector,
    ) -> Result<(), Error> {
        tracing::info!(?band, %selector, "recalling memory selector");
        let response = self
            .execute(Command::RecallMemoryChannel { band, selector })
            .await?;
        match response {
            Response::MemoryRecall {
                band: response_band,
                selector: response_selector,
            } if response_band == band && response_selector == selector => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MemoryRecall {{ band: {band:?}, selector: {selector} }}"),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Step the current operating context up by one increment (bare UP action).
    ///
    /// This is an ACTION command that changes the radio's active frequency.
    /// There is no undo; the previous frequency is not preserved.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn frequency_up(&mut self) -> Result<(), Error> {
        tracing::info!("stepping current operating context up");
        let response = self.execute(Command::FrequencyUp).await?;
        match response {
            Response::FrequencyUp | Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyUp".into(),
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
    /// the same as the "FM Radio" menu item on the radio: it tunes to commercial broadcast
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
    /// No band parameter: the radio returns the fine step for the current
    /// operating context.
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

    /// Set fine tune on/off (FT write).
    ///
    /// Per Operating Tips section 5.10.6: Fine Tune only works with AM modulation
    /// and Band B.
    ///
    /// # Wire format
    ///
    /// `FT value\r` where value is 0 (off) or 1 (on). This is a global toggle
    /// (no band parameter). Confirmed by ARFC-D75 decompilation and firmware
    /// handler analysis (accepts only 5-byte commands: `FT N\r`).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_function_type(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::info!(enabled, "setting fine tune (FT)");
        let response = self.execute(Command::SetFunctionType { enabled }).await?;
        match response {
            Response::FunctionType { .. } => Ok(()),
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
