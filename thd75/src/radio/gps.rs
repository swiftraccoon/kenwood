//! GPS subsystem methods.
//!
//! The TH-D75 has a built-in GPS receiver that provides position data for APRS beaconing,
//! waypoint navigation, and time synchronization. The GPS integrates directly with the APRS
//! TNC — when APRS beaconing is enabled and the GPS has a fix, position reports are
//! automatically included in transmitted beacons.
//!
//! The `pc_output` flag in the GPS configuration controls whether raw NMEA sentences are
//! forwarded over the serial (USB/BT) connection. This is useful for feeding GPS data to
//! mapping software, but **competes with CAT command I/O** on the same serial channel.
//!
//! # Related commands
//!
//! - **GP**: GPS enable and PC output configuration
//! - **GS**: NMEA sentence selection (which sentence types to output)
//! - **GM**: GPS/Radio mode (bare read only — `GM 1` reboots the radio into GPS-only mode)

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::GpsRadioMode;

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
    /// Returns `(gga, gll, gsa, gsv, rmc, vtg)` — six booleans indicating which NMEA 0183
    /// sentence types are enabled for output when `pc_output` is active.
    ///
    /// # Sentence types
    ///
    /// - **GGA** (Global Positioning System Fix Data): time, position, fix quality, number of
    ///   satellites, HDOP, altitude. The primary fix sentence.
    /// - **GLL** (Geographic Position - Latitude/Longitude): position and time, simpler than GGA.
    /// - **GSA** (GNSS DOP and Active Satellites): fix type (2D/3D), satellite IDs in use,
    ///   PDOP/HDOP/VDOP dilution of precision values.
    /// - **GSV** (GNSS Satellites in View): satellite count, PRN numbers, elevation, azimuth,
    ///   and SNR for each satellite. Multiple sentences for all visible satellites.
    /// - **RMC** (Recommended Minimum Navigation Information): time, position, speed over
    ///   ground, course, date, magnetic variation. The most commonly used sentence.
    /// - **VTG** (Course Over Ground and Ground Speed): track (true and magnetic) and speed
    ///   (knots and km/h).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gps_sentences(
        &mut self,
    ) -> Result<(bool, bool, bool, bool, bool, bool), Error> {
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
    /// - `gps_enabled`: turns the GPS receiver on or off. When off, no position fix is
    ///   available for APRS beaconing or display.
    /// - `pc_output`: when `true`, the radio outputs raw NMEA sentences over the serial
    ///   connection (USB or Bluetooth SPP). **This competes with CAT command I/O** — NMEA
    ///   data will be interleaved with CAT responses on the same serial channel, which can
    ///   confuse the protocol parser. Only enable this if you are prepared to handle mixed
    ///   NMEA/CAT traffic, or if you are using the serial port exclusively for GPS data.
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
    /// Returns the current GPS/Radio operating mode. `Normal` (0) means
    /// standard transceiver operation. `GpsReceiver` (1) means GPS-only mode.
    ///
    /// # Warning
    /// Only the bare `GM\r` read is safe. Sending `GM 1\r` would reboot
    /// the radio into GPS-only mode. This method only sends the bare read.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gps_mode(&mut self) -> Result<GpsRadioMode, Error> {
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
