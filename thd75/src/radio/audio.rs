//! Audio control methods.
//!
//! Controls AF (Audio Frequency) gain (band-indexed) and VOX (Voice-Operated
//! Exchange) settings for hands-free transmit.
//!
//! # D75 tone commands
//!
//! The D75 firmware RE originally identified TN, DC, and RT as tone commands.
//! Hardware testing revealed their actual functions:
//! - **TN**: TNC mode (not CTCSS tone)
//! - **DC**: D-STAR callsign slots (not DCS code)
//! - **RT**: Real-time clock (not repeater tone)
//!
//! CTCSS tone and DCS code are instead configured through the FO (full
//! frequency/offset) command's channel data fields.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{AfGainLevel, Band, DstarSlot, TncBaud, TncMode, VoxDelay, VoxGain};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get the AF gain level (AG read).
    ///
    /// D75 RE: bare `AG\r` returns global gain level. Band-indexed read
    /// returns `?`, so this is a global query.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_af_gain(&mut self) -> Result<AfGainLevel, Error> {
        tracing::debug!("reading AF gain");
        let response = self.execute(Command::GetAfGain).await?;
        match response {
            Response::AfGain { level } => Ok(level),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AfGain".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the AF gain level (AG write).
    ///
    /// # Get/set asymmetry
    ///
    /// The get and set commands have different wire formats on the D75:
    /// - **Read** (`AG\r`): bare command, returns a global gain level. Band-indexed read
    ///   (`AG 0\r`) returns `?`.
    /// - **Write** (`AG NNN\r`): bare 3-digit zero-padded value (e.g., `AG 015\r`). Despite
    ///   the `band` parameter in this method's signature, the wire format is bare (no band
    ///   index) — the value applies globally.
    ///
    /// # Valid range
    ///
    /// `level` must be 0 through 99. The wire format zero-pads to 3 digits (e.g., `AG 005\r`).
    /// Values outside 0-99 may be rejected or cause unexpected behavior.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_af_gain(&mut self, band: Band, level: AfGainLevel) -> Result<(), Error> {
        tracing::debug!(?band, ?level, "setting AF gain");
        let response = self.execute(Command::SetAfGain { band, level }).await?;
        match response {
            Response::AfGain { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AfGain".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the TNC mode (TN bare read).
    ///
    /// Hardware-verified: bare `TN\r` returns `TN mode,setting`.
    /// Returns `(mode, setting)`.
    ///
    /// Valid mode values per firmware validation: 0, 1, 2, 3.
    /// Mode 3 may correspond to MMDVM or Reflector Terminal mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_tnc_mode(&mut self) -> Result<(TncMode, TncBaud), Error> {
        tracing::debug!("reading TNC mode");
        let response = self.execute(Command::GetTncMode).await?;
        match response {
            Response::TncMode { mode, setting } => Ok((mode, setting)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TncMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the TNC mode (TN write).
    ///
    /// Valid mode values per firmware validation: 0, 1, 2, 3.
    /// Mode 3 may correspond to MMDVM or Reflector Terminal mode.
    ///
    /// # Wire format
    ///
    /// `TN mode,setting\r`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_tnc_mode(&mut self, mode: TncMode, setting: TncBaud) -> Result<(), Error> {
        tracing::info!(?mode, ?setting, "setting TNC mode");
        let response = self.execute(Command::SetTncMode { mode, setting }).await?;
        match response {
            Response::TncMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TncMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get D-STAR callsign data for a slot (DC read).
    ///
    /// Hardware-verified: `DC slot\r` where slot is 1-6.
    /// Returns `(callsign, suffix)`.
    ///
    /// Note: This method lives in `audio.rs` rather than `dstar.rs` because
    /// it was discovered during audio subsystem hardware probing. The `DC`
    /// mnemonic is overloaded on the D75 (DCS code, not D-STAR callsign
    /// as on D74).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_dstar_callsign(&mut self, slot: DstarSlot) -> Result<(String, String), Error> {
        tracing::debug!(?slot, "reading D-STAR callsign");
        let response = self.execute(Command::GetDstarCallsign { slot }).await?;
        match response {
            Response::DstarCallsign {
                callsign, suffix, ..
            } => Ok((callsign, suffix)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DstarCallsign".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set D-STAR callsign data for a slot (DC write).
    ///
    /// Writes callsign and suffix data to one of the 6 D-STAR callsign slots.
    ///
    /// # Wire format
    ///
    /// `DC slot,callsign,suffix\r` where slot is 1-6, callsign is 8 characters
    /// (space-padded), and suffix is up to 4 characters.
    ///
    /// # Parameters
    ///
    /// - `slot`: Callsign slot number (1-6).
    /// - `callsign`: Callsign string (8 characters, space-padded to length).
    /// - `suffix`: Callsign suffix (up to 4 characters, e.g., "D75A").
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_dstar_callsign(
        &mut self,
        slot: DstarSlot,
        callsign: &str,
        suffix: &str,
    ) -> Result<(), Error> {
        tracing::info!(?slot, callsign, suffix, "setting D-STAR callsign");
        let response = self
            .execute(Command::SetDstarCallsign {
                slot,
                callsign: callsign.to_owned(),
                suffix: suffix.to_owned(),
            })
            .await?;
        match response {
            Response::DstarCallsign { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DstarCallsign".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the real-time clock (RT bare read).
    ///
    /// Note: This method lives in `audio.rs` rather than `system.rs` because
    /// `RT` is overloaded on the D75 (repeater tone vs real-time clock on D74).
    /// It was discovered during audio subsystem probing.
    ///
    /// Hardware-verified: bare `RT\r` returns `RT YYMMDDHHmmss`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_real_time_clock(&mut self) -> Result<String, Error> {
        tracing::debug!("reading real-time clock");
        let response = self.execute(Command::GetRealTimeClock).await?;
        match response {
            Response::RealTimeClock { datetime } => Ok(datetime),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RealTimeClock".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the VOX (Voice-Operated Exchange/Transmit) enabled state (VX read).
    ///
    /// VOX allows hands-free transmit operation. When enabled, the radio automatically keys
    /// the transmitter when it detects audio input from the microphone, and returns to receive
    /// after a configurable delay when audio stops.
    ///
    /// VOX must be enabled before [`get_vox_gain`](Self::get_vox_gain) or
    /// [`get_vox_delay`](Self::get_vox_delay) will succeed — those commands return `N`
    /// (not available) when VOX is disabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_vox(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading VOX state");
        let response = self.execute(Command::GetVox).await?;
        match response {
            Response::Vox { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Vox".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the VOX (Voice-Operated Exchange/Transmit) enabled state (VX write).
    ///
    /// See [`get_vox`](Self::get_vox) for a description of VOX operation. Enabling VOX
    /// (`true`) unlocks the [`set_vox_gain`](Self::set_vox_gain) and
    /// [`set_vox_delay`](Self::set_vox_delay) commands. Disabling VOX (`false`) causes
    /// those commands to return `N` (not available).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_vox(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::debug!(enabled, "setting VOX state");
        let response = self.execute(Command::SetVox { enabled }).await?;
        match response {
            Response::Vox { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Vox".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the VOX gain level (VG read).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VG read to succeed.
    /// Returns `N` (not available) when VOX is off.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_vox_gain(&mut self) -> Result<VoxGain, Error> {
        tracing::debug!("reading VOX gain");
        let response = self.execute(Command::GetVoxGain).await?;
        match response {
            Response::VoxGain { gain } => Ok(gain),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "VoxGain".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the VOX gain level (VG write).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VG write to succeed.
    /// Returns `N` (not available) when VOX is off.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_vox_gain(&mut self, gain: VoxGain) -> Result<(), Error> {
        tracing::debug!(?gain, "setting VOX gain");
        let response = self.execute(Command::SetVoxGain { gain }).await?;
        match response {
            Response::VoxGain { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "VoxGain".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the VOX delay value (VD read).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VD read to succeed.
    /// Returns `N` (not available) when VOX is off.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_vox_delay(&mut self) -> Result<VoxDelay, Error> {
        tracing::debug!("reading VOX delay");
        let response = self.execute(Command::GetVoxDelay).await?;
        match response {
            Response::VoxDelay { delay } => Ok(delay),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "VoxDelay".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the VOX delay value (VD write).
    ///
    /// # Mode requirement
    /// VOX must be enabled (`VX 1`) for VD write to succeed.
    /// Returns `N` (not available) when VOX is off.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_vox_delay(&mut self, delay: VoxDelay) -> Result<(), Error> {
        tracing::debug!(?delay, "setting VOX delay");
        let response = self.execute(Command::SetVoxDelay { delay }).await?;
        match response {
            Response::VoxDelay { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "VoxDelay".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
