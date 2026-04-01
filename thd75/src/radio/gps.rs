//! GPS subsystem methods.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get GPS configuration (GP read).
    ///
    /// Returns `(gps_enabled, pc_output)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gps_config(&mut self) -> Result<(bool, bool), Error> {
        tracing::debug!("reading GPS config");
        let response = self.execute(Command::GetGpsConfig).await?;
        match response {
            Response::GpsConfig {
                gps_enabled,
                pc_output,
            } => Ok((gps_enabled, pc_output)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsConfig".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get GPS NMEA sentence enable flags (GS read).
    ///
    /// Returns `(gga, gll, gsa, gsv, rmc, vtg)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gps_sentences(&mut self) -> Result<(bool, bool, bool, bool, bool, bool), Error> {
        tracing::debug!("reading GPS NMEA sentence flags");
        let response = self.execute(Command::GetGpsSentences).await?;
        match response {
            Response::GpsSentences {
                gga,
                gll,
                gsa,
                gsv,
                rmc,
                vtg,
            } => Ok((gga, gll, gsa, gsv, rmc, vtg)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsSentences".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set GPS configuration (GP write).
    ///
    /// Sets `gps_enabled` and `pc_output` flags.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_gps_config(
        &mut self,
        gps_enabled: bool,
        pc_output: bool,
    ) -> Result<(), Error> {
        tracing::info!(gps_enabled, pc_output, "setting GPS config");
        let response = self
            .execute(Command::SetGpsConfig {
                gps_enabled,
                pc_output,
            })
            .await?;
        match response {
            Response::GpsConfig { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsConfig".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set GPS NMEA sentence enable flags (GS write).
    ///
    /// Sets 6 boolean flags controlling which NMEA sentences are output:
    /// GGA, GLL, GSA, GSV, RMC, VTG.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    #[allow(clippy::fn_params_excessive_bools, clippy::similar_names)]
    pub async fn set_gps_sentences(
        &mut self,
        gga: bool,
        gll: bool,
        gsa: bool,
        gsv: bool,
        rmc: bool,
        vtg: bool,
    ) -> Result<(), Error> {
        tracing::info!(gga, gll, gsa, gsv, rmc, vtg, "setting GPS NMEA sentences");
        let response = self
            .execute(Command::SetGpsSentences {
                gga,
                gll,
                gsa,
                gsv,
                rmc,
                vtg,
            })
            .await?;
        match response {
            Response::GpsSentences { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsSentences".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get GPS/Radio mode status (GM bare read).
    ///
    /// # Warning
    /// Only the bare `GM\r` read is safe. Sending `GM 1\r` would reboot
    /// the radio into GPS-only mode. This method only sends the bare read.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gps_mode(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading GPS/Radio mode");
        let response = self.execute(Command::GetGpsMode).await?;
        match response {
            Response::GpsMode { mode } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
