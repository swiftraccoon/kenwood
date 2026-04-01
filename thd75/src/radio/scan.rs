//! Scan-related radio methods: scan resume (SR write-only), scan range (SF), band scope (BS).

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::Band;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Set the scan resume mode (SR write).
    ///
    /// Hardware-verified: bare `SR\r` returns `?` (no read form).
    /// SR is write-only on the D75.
    ///
    /// # Safety warning
    /// On hardware, `SR 0` was observed to reboot the radio. The D75 RE
    /// identifies this as scan resume, but the behavior may coincide with
    /// a reset action. Use with caution.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_scan_resume(&mut self, mode: u8) -> Result<(), Error> {
        tracing::warn!(
            mode,
            "setting scan resume mode (SR) — may reboot radio if mode=0"
        );
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
}
