//! Scan-related radio methods plus MW/SW antenna selection.
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

    /// Get the MW/SW receive antenna selection (BS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_bar_antenna(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading MW/SW antenna selection");
        let response = self.execute(Command::GetBarAntenna).await?;
        match response {
            Response::BarAntenna { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BarAntenna".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Select the MW/SW receive antenna (BS write).
    ///
    /// `true` selects the internal bar antenna; `false` selects the external
    /// ANT Connector.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_bar_antenna(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::info!(enabled, "setting MW/SW antenna selection");
        let response = self.execute(Command::SetBarAntenna { enabled }).await?;
        match response {
            Response::BarAntenna { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BarAntenna".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
