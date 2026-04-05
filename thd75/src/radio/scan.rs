//! Scan-related radio methods: scan resume (SR write-only), scan range (SF), band scope (BS).

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{Band, ScanResumeMethod};

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

    /// Get the scan range setting for a band (SF read).
    ///
    /// Hardware-verified: `SF band\r` returns `SF band,value`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_scan_range(&mut self, band: Band) -> Result<(Band, u8), Error> {
        tracing::debug!(?band, "reading scan range");
        let response = self.execute(Command::GetScanRange { band }).await?;
        match response {
            Response::ScanRange {
                band: resp_band,
                value,
            } => Ok((resp_band, value)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ScanRange".into(),
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
