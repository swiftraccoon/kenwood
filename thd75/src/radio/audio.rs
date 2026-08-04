//! Audio control methods.
//!
//! Controls global AF (Audio Frequency) gain and VOX (Voice-Operated
//! Exchange) settings for hands-free transmit.
//!
use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{AfGainLevel, VoxDelay, VoxGain};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get the AF gain level (AG read).
    ///
    /// Bare `AG\r` returns the global gain level. A band-indexed read returns
    /// `?`, so this is a global query.
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
    /// Both reads (`AG\r`) and writes (`AG NNN\r`) operate on one global
    /// level. Band-indexed AG commands are rejected by the radio.
    ///
    /// # Valid range
    ///
    /// `level` is validated to 0 through 200. The wire format zero-pads to
    /// three digits (for example, `AG 005\r` or `AG 200\r`).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_af_gain(&mut self, level: AfGainLevel) -> Result<(), Error> {
        tracing::debug!(?level, "setting global AF gain");
        let response = self.execute(Command::SetAfGain { level }).await?;
        match response {
            Response::AfGain { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AfGain".into(),
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
    /// [`get_vox_delay`](Self::get_vox_delay) will succeed; those commands return `N`
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
