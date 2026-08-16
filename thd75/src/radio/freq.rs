//! Core radio methods: frequency, mode, power, squelch, S-meter, TX/RX, firmware, power status, ID,
//! band control, tuning mode, FM radio, fine step, Fine Tune, and filter width.
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
//! - **FO** (read-only in this library): returns the full 20-field CAT channel record. A write
//!   API remains unavailable until every field and its readback behavior have been qualified.
//!
//! # VFO mode requirement
//!
//! Band-indexed write commands generally require the target band to be in VFO mode. If the band
//! is in Memory, Call, or WX mode, the radio may reject the write. Use
//! [`set_tuning_mode`](Radio::set_tuning_mode) explicitly. Frequency tuning remains
//! unavailable while the complete FO write record is under qualification.
//!
//! # Tone and offset configuration
//!
//! CTCSS tone, DCS code, tone mode, and repeater offset are not configured through dedicated
//! commands. They are fields within the full FO record returned by
//! [`get_frequency_full`](Radio::get_frequency_full). Writing that record is intentionally
//! unavailable because a partial read-modify-write can alter unrelated radio state.

use crate::error::{Error, ProtocolError};
use crate::protocol::programming;
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{
    Band, CatChannelRecord, CurrentMemorySelector, FilterMode, FilterWidthIndex, FineStep,
    FirmwareIdentity, Frequency, MemoryChannelAddress, OperatingMode, PowerLevel, RegularChannel,
    SMeterReading, SquelchLevel, TuningMode,
};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read the current frequency data for the given band (FQ read).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::Band;
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"FQ 0\r", b"FQ 0,0145000000\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// let frequency = radio.get_frequency(Band::A).await?;
    /// assert_eq!(frequency.as_hz(), 145_000_000);
    /// # Ok(())
    /// # }
    /// ```
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

    /// Get the operating mode for the given band (MD read).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::{Band, OperatingMode};
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"MD 0\r", b"MD 0,0\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// let mode = radio.get_operating_mode(Band::A).await?;
    /// assert_eq!(mode, OperatingMode::Fm);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_operating_mode(&mut self, band: Band) -> Result<OperatingMode, Error> {
        tracing::debug!(?band, "reading operating mode");
        let response = self.execute(Command::GetOperatingMode { band }).await?;
        match response {
            Response::OperatingMode { mode, .. } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "OperatingMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the operating mode for the given band (MD write).
    ///
    /// # Selection restrictions
    ///
    /// The mode must be selectable on the target band in its current state.
    /// DR is entered through the radio's digital-mode controls; its readable
    /// `MD` value does not make it a generally usable CAT selection value.
    ///
    /// See the [`OperatingMode`] type for valid values. Note that the MD command uses a
    /// different encoding than FO/ME commands; [`OperatingMode`] handles this mapping.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_operating_mode(
        &mut self,
        band: Band,
        mode: OperatingMode,
    ) -> Result<(), Error> {
        tracing::debug!(?band, ?mode, "setting operating mode");
        let response = match self.execute(Command::SetOperatingMode { band, mode }).await {
            Ok(response) => response,
            Err(
                error @ (Error::CommandRejected { .. } | Error::NotAvailableInCurrentMode { .. }),
            ) => {
                return match self.get_operating_mode(band).await {
                    Ok(readback) if readback == mode => Ok(()),
                    Ok(_) => Err(error),
                    Err(readback_error) => Err(Error::OperatingModeWriteUnconfirmed {
                        band,
                        requested: mode,
                        rejection: Box::new(error),
                        readback: Box::new(readback_error),
                    }),
                };
            }
            Err(error) => return Err(error),
        };
        match response {
            Response::OperatingMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "OperatingMode".into(),
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
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::{Band, SquelchLevel};
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"SQ 0\r", b"SQ 0,05\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// let level = radio.get_squelch(Band::A).await?;
    /// assert_eq!(level, SquelchLevel::new(5)?);
    /// # Ok(())
    /// # }
    /// ```
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
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::{Band, SquelchLevel};
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"SQ 0,4\r", b"SQ 0,4\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// radio.set_squelch(Band::A, SquelchLevel::new(4)?).await?;
    /// # Ok(())
    /// # }
    /// ```
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
    /// separately before transmitting. The radio echoes `TX\r` on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn transmit(&mut self) -> Result<(), Error> {
        tracing::info!("keying transmitter on the current operating context");
        let response = self.execute(Command::Transmit).await?;
        match response {
            Response::TransmitAck => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Transmit".into(),
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
    /// `RX\r`. The radio echoes `RX\r` on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn receive(&mut self) -> Result<(), Error> {
        tracing::info!("returning the current operating context to receive");
        let response = self.execute(Command::Receive).await?;
        match response {
            Response::ReceiveAck => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Receive".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the exact firmware identity (FV read).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"FV\r", b"FV 1.03.000\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// assert_eq!(radio.get_firmware_version().await?.as_str(), "1.03.000");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_firmware_version(&mut self) -> Result<FirmwareIdentity, Error> {
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

    /// Get the current active band (BC read).
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::Band;
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"BC\r", b"BC 1\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// assert_eq!(radio.get_band().await?, Band::B);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_band(&mut self) -> Result<Band, Error> {
        tracing::debug!("reading active band");
        let response = self.execute(Command::GetBand).await?;
        match response {
            Response::Band { band } => Ok(band),
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
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::Band;
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"BC 1\r", b"BC 1\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// radio.set_band(Band::B).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_band(&mut self, band: Band) -> Result<(), Error> {
        tracing::info!(?band, "setting active band");
        let response = self.execute(Command::SetBand { band }).await?;
        match response {
            Response::Band { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandResponse".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the tuning mode for a band (VM read).
    ///
    /// Returns a mode index: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_tuning_mode(&mut self, band: Band) -> Result<TuningMode, Error> {
        tracing::debug!(?band, "reading tuning mode");
        let response = self.execute(Command::GetTuningMode { band }).await?;
        match response {
            Response::TuningMode { mode, .. } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TuningMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the tuning mode for a band (VM write).
    ///
    /// Tuning mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_tuning_mode(&mut self, band: Band, mode: TuningMode) -> Result<(), Error> {
        tracing::info!(?band, ?mode, "setting tuning mode");
        let response = self.execute(Command::SetTuningMode { band, mode }).await?;
        match response {
            Response::TuningMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TuningMode".into(),
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
    /// operating in VFO mode) returns `N`, surfaced as
    /// [`Error::NotAvailableInCurrentMode`].
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_current_channel(
        &mut self,
        band: Band,
    ) -> Result<CurrentMemorySelector, Error> {
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
    pub async fn recall_channel(
        &mut self,
        band: Band,
        channel: RegularChannel,
    ) -> Result<(), Error> {
        self.recall_memory(band, channel.into()).await
    }

    /// Recall an exact memory selector on the given band (MR action).
    ///
    /// Supports ordinary channels, program-scan edges, and regional memory
    /// addresses without collapsing them to a fabricated numeric channel.
    /// `Pri` is not accepted by the firmware's MR input parser and therefore
    /// cannot be represented by [`MemoryChannelAddress`].
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn recall_memory(
        &mut self,
        band: Band,
        selector: MemoryChannelAddress,
    ) -> Result<(), Error> {
        tracing::info!(?band, %selector, "recalling memory selector");
        let response = self
            .execute(Command::RecallMemoryChannel { band, selector })
            .await?;
        match response {
            Response::MemoryRecallAck {
                band: response_band,
                selector: response_selector,
            } if response_band == band && response_selector == selector => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MemoryRecallAck {{ band: {band:?}, selector: {selector} }}"),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Step the current operating context up by one increment and read back
    /// the resulting frequency.
    ///
    /// This is an ACTION command that changes the radio's active frequency.
    /// There is no undo; the previous frequency is not preserved.
    ///
    /// # Errors
    ///
    /// Returns an error if resolving the current band, stepping, or reading
    /// back the resulting frequency fails.
    pub async fn frequency_up(&mut self) -> Result<Frequency, Error> {
        let band = self.get_band().await?;
        tracing::info!(?band, "stepping current operating context up");
        let response = self.execute(Command::FrequencyUp).await?;
        match response {
            Response::FrequencyUpAck => {}
            other => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "FrequencyUpAck".into(),
                    actual: format!("{other:?}").into_bytes(),
                }));
            }
        }
        self.get_frequency(band).await
    }

    /// Step the current operating context up without reading back the result.
    ///
    /// This is the fire-and-forget counterpart to [`frequency_up`](Self::frequency_up).
    ///
    /// # Errors
    ///
    /// Returns an error if the action fails or the response is unexpected.
    pub async fn frequency_up_blind(&mut self) -> Result<(), Error> {
        tracing::info!("stepping current operating context up without read-back");
        let response = self.execute(Command::FrequencyUp).await?;
        match response {
            Response::FrequencyUpAck => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyUpAck".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Step the current operating context down by one increment and read back
    /// the resulting frequency.
    ///
    /// This is an action command that changes the radio's active frequency.
    ///
    /// # Errors
    ///
    /// Returns an error if resolving the current band, stepping, or reading
    /// back the resulting frequency fails.
    pub async fn frequency_down(&mut self) -> Result<Frequency, Error> {
        let band = self.get_band().await?;
        tracing::info!(?band, "stepping current operating context down");
        let response = self.execute(Command::FrequencyDown).await?;
        match response {
            Response::FrequencyDownAck => {}
            other => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: "FrequencyDownAck".into(),
                    actual: format!("{other:?}").into_bytes(),
                }));
            }
        }
        self.get_frequency(band).await
    }

    /// Step the current operating context down without reading back the result.
    ///
    /// This is the fire-and-forget counterpart to
    /// [`frequency_down`](Self::frequency_down).
    ///
    /// # Errors
    ///
    /// Returns an error if the action fails or the response is unexpected.
    pub async fn frequency_down_blind(&mut self) -> Result<(), Error> {
        tracing::info!("stepping current operating context down without read-back");
        let response = self.execute(Command::FrequencyDown).await?;
        match response {
            Response::FrequencyDownAck => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyDownAck".into(),
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

    /// Set FM Radio mode through Menu 700's MCP cell.
    ///
    /// This changes `radio.FmRadioMode` at exact MCP offset `0x1040`. It does
    /// not emit an `FR` write: retained hardware evidence reports `N` for
    /// that CAT form. The MCP page write is verified by read-back.
    ///
    /// # Connection lifetime
    ///
    /// This enters and exits MCP programming mode. The exit path waits for
    /// USB re-enumeration and restores a qualified CAT session before this
    /// method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading or writing the
    /// setting page, verifying the write, exiting, or reconnecting fails.
    pub async fn set_fm_radio_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        use super::mcp_offsets;

        tracing::info!(
            enabled,
            offset = mcp_offsets::FM_RADIO_MODE,
            "setting FM Radio mode via MCP"
        );
        self.modify_memory_page(
            programming::WritableMcpPage::new(mcp_offsets::page(mcp_offsets::FM_RADIO_MODE))?,
            |data| {
                data[const { mcp_offsets::byte_index(mcp_offsets::FM_RADIO_MODE) }] =
                    u8::from(enabled);
            },
        )
        .await
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

    /// Get the Fine Tune state (FT read).
    ///
    /// Fine Tune is available only for AM operation on Band B.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_fine_tune(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading Fine Tune state");
        let response = self.execute(Command::GetFineTune).await?;
        match response {
            Response::FineTune { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FineTune".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the Fine Tune state (FT write).
    ///
    /// Fine Tune is available only for AM operation on Band B.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_fine_tune(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::debug!(enabled, "setting Fine Tune state");
        let response = self.execute(Command::SetFineTune { enabled }).await?;
        match response {
            Response::FineTune { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FineTune".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the filter width for a receiver mode (SH read).
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
            Response::FilterWidth { width } => Ok(width),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FilterWidth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set a mode-qualified filter width (SH write).
    ///
    /// [`FilterWidthIndex::mode`] selects SSB, CW, or AM. Its index is already
    /// validated against that mode's table (per Operating Tips
    /// §5.10.1–§5.10.3), so a cross-domain pair cannot reach the wire.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_filter_width(&mut self, width: FilterWidthIndex) -> Result<(), Error> {
        tracing::info!(mode = ?width.mode(), ?width, "setting filter width");
        let response = self.execute(Command::SetFilterWidth { width }).await?;
        match response {
            Response::FilterWidth { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FilterWidth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
