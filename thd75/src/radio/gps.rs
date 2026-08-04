//! GPS subsystem methods.
//!
//! The TH-D75 has a built-in GPS receiver that provides position data for APRS beaconing,
//! waypoint navigation, and time synchronization. The GPS integrates directly with the APRS
//! TNC: when APRS beaconing is enabled and the GPS has a fix, position reports are
//! automatically included in transmitted beacons.
//!
//! The `pc_output` flag in the GPS settings controls whether raw NMEA sentences are
//! forwarded over the serial (USB/BT) connection. This is useful for feeding GPS data to
//! mapping software, but **competes with CAT command I/O** on the same serial channel.
//!
//! # Related commands
//!
//! - **GP**: GPS enable and PC output settings
//! - **GS**: NMEA sentence selection (which sentence types to output)
//! - **GM**: GPS/Radio mode (bare read only; `GM 1` reboots the radio into GPS-only mode)

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{GpsRadioMode, GpsSettings, NmeaSentences};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get GPS settings (GP read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gps_settings(&mut self) -> Result<GpsSettings, Error> {
        tracing::debug!("reading GPS settings");
        let response = self.execute(Command::GetGpsSettings).await?;
        match response {
            Response::GpsSettings { settings } => Ok(settings),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsSettings".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get GPS NMEA sentence enable flags (GS read).
    ///
    /// Returns a validated, nonempty selection of NMEA 0183 sentence types
    /// enabled for output when PC output is active.
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
    pub async fn get_gps_sentences(&mut self) -> Result<NmeaSentences, Error> {
        tracing::debug!("reading GPS NMEA sentence flags");
        let response = self.execute(Command::GetGpsSentences).await?;
        match response {
            Response::GpsSentences { sentences } => Ok(sentences),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsSentences".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set GPS settings (GP write).
    ///
    /// Turning the receiver off makes no position fix available for APRS
    /// beaconing or display. When PC output is enabled, the radio emits raw
    /// NMEA sentences over the USB or Bluetooth serial connection. This
    /// competes with CAT command I/O: NMEA data is interleaved with CAT
    /// responses on the same stream. Enable it only when the caller handles
    /// mixed NMEA/CAT traffic or uses the serial port exclusively for GPS.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_gps_settings(&mut self, settings: GpsSettings) -> Result<(), Error> {
        tracing::info!(
            gps_enabled = settings.enabled(),
            pc_output = settings.pc_output(),
            "setting GPS settings"
        );
        let response = self.execute(Command::SetGpsSettings { settings }).await?;
        match response {
            Response::GpsSettings { .. } => {
                // Remembered so `Radio::reconnect` re-asserts it.
                self.gps_settings = Some(settings);
                Ok(())
            }
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsSettings".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set GPS NMEA sentence enable flags (GS write).
    ///
    /// The selection is validated before I/O and therefore cannot contain
    /// reserved bits or disable every sentence.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_gps_sentences(&mut self, sentences: NmeaSentences) -> Result<(), Error> {
        tracing::info!(bits = sentences.bits(), "setting GPS NMEA sentences");
        let response = self.execute(Command::SetGpsSentences { sentences }).await?;
        match response {
            Response::GpsSentences { .. } => {
                // Remembered so `Radio::reconnect` re-asserts it.
                self.gps_sentences = Some(sentences);
                Ok(())
            }
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "GpsSentences".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read GPS/Radio mode status, querying firmware first when it is not cached.
    ///
    /// Returns the current GPS/Radio operating mode. `Normal` (0) means
    /// standard transceiver operation. `GpsReceiver` (1) means GPS-only mode.
    ///
    /// # Warning
    /// On a qualified standard CAT firmware identity, only the bare `GM\r`
    /// read is safe; sending `GM 1\r` would reboot the radio into GPS-only
    /// mode. The exact `1.03.AZM` firmware repurposes bare `GM`, and unknown
    /// firmware may do the same, so this method refuses both profiles before
    /// sending `GM`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CommandUnavailableOnFirmware`] without sending `GM`
    /// unless the exact firmware identity is in
    /// [`super::STANDARD_CAT_FIRMWARE_IDENTITIES`]. On qualified firmware,
    /// returns an error if the command fails or the response is unexpected.
    pub async fn read_gps_mode(&mut self) -> Result<GpsRadioMode, Error> {
        self.require_firmware_command("GM", super::FirmwareProfile::supports_bare_gps_mode)
            .await?;
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
