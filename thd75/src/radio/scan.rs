//! Scan-related radio methods: scan resume (SR write-only), step size (SF), band scope (BS).
//!
//! # Single Band Display (per Operating Tips §5.10.4)
//!
//! Menu No. 904 controls the Single Band Display information line:
//! Off, GPS (Altitude), GPS (Ground Speed), Date, or Demodulation Mode.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{Band, ScanResumeMethod, StepSize};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Set the scan resume mode (SR write).
    ///
    /// Hardware-verified: bare `SR\r` returns `?` (no read form).
    /// Sets the scan resume method (SR write).
    ///
    /// Firmware-verified: SR reads/writes scan resume configuration via
    /// hardware registers, NOT a radio reset (previous documentation was wrong).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_scan_resume(&mut self, mode: ScanResumeMethod) -> Result<(), Error> {
        tracing::info!(?mode, "setting scan resume mode (SR)");
        let response = self.execute(Command::SetScanResume { mode }).await?;
        match response {
            Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Ok".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the step size for a band (SF read).
    ///
    /// Firmware-verified: SF = Step Size. `SF band\r` returns `SF band,step`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_step_size(&mut self, band: Band) -> Result<(Band, StepSize), Error> {
        tracing::debug!(?band, "reading step size");
        let response = self.execute(Command::GetStepSize { band }).await?;
        match response {
            Response::StepSize {
                band: resp_band,
                step,
            } => Ok((resp_band, step)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "StepSize".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the step size for a band (SF write).
    ///
    /// Firmware-verified: SF = Step Size. `SF band,step\r` (band 0-1, step 0-11).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_step_size(&mut self, band: Band, step: StepSize) -> Result<(), Error> {
        tracing::info!(?band, ?step, "setting step size");
        let response = self.execute(Command::SetStepSize { band, step }).await?;
        match response {
            Response::StepSize { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "StepSize".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get band scope data for a band (BS read).
    ///
    /// The radio echoes back the band number when queried.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_band_scope(&mut self, band: Band) -> Result<Band, Error> {
        tracing::debug!(?band, "reading band scope");
        let response = self.execute(Command::GetBandScope { band }).await?;
        match response {
            Response::BandScope { band: scope_band } => Ok(scope_band),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandScope".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set band scope configuration for a band (BS write).
    ///
    /// # Wire format
    ///
    /// `BS band,value\r` where band is 0 (A) or 1 (B). The exact meaning
    /// of the value parameter is unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_band_scope(&mut self, band: Band, value: u8) -> Result<(), Error> {
        tracing::info!(?band, value, "setting band scope configuration");
        let response = self.execute(Command::SetBandScope { band, value }).await?;
        match response {
            Response::BandScope { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandScope".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
