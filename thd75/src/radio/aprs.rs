//! APRS (Automatic Packet Reporting System) subsystem methods.
//!
//! APRS is a digital communications protocol for real-time tactical information exchange. The
//! TH-D75 has a built-in TNC (Terminal Node Controller) that handles AX.25 packet encoding and
//! decoding, supporting both 1200 baud (VHF, standard APRS on 144.390 MHz in North America) and
//! 9600 baud (UHF) operation.
//!
//! The TNC handles position beaconing, message exchange, and weather reporting. Beacon
//! transmission is controlled by the beacon type setting (PT command), which determines whether
//! beacons are sent manually, at fixed intervals, or based on `SmartBeaconing` rules.
//!
//! # Related commands
//!
//! - **AS**: TNC baud rate (1200/9600)
//! - **PT**: Beacon TX control mode
//! - **MS**: Position source / message send (overloaded mnemonic)
//! - **AE**: Serial number info (not actually APRS-related, but shares the A prefix)
//! - **BE**: Sends an APRS beacon (transmits on air — requires a valid
//!   amateur licence and appropriate authorisation; use deliberately)

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{BeaconMode, TncBaud};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get the TNC baud rate (AS read).
    ///
    /// Returns 0 = 1200 baud, 1 = 9600 baud.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_tnc_baud(&mut self) -> Result<TncBaud, Error> {
        tracing::debug!("reading TNC baud rate");
        let response = self.execute(Command::GetTncBaud).await?;
        match response {
            Response::TncBaud { rate } => Ok(rate),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TncBaud".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the beacon TX control mode (PT read).
    ///
    /// Returns the current beacon transmission mode:
    ///
    /// - `0` = Off (no automatic beaconing)
    /// - `1` = Manual (beacon sent only when explicitly triggered)
    /// - `2` = PTT (beacon sent after each PTT release)
    /// - `3` = Auto (beacon sent at fixed intervals set by the beacon interval timer)
    /// - `4` = `SmartBeaconing` (adaptive beaconing based on speed and direction changes)
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_beacon_type(&mut self) -> Result<BeaconMode, Error> {
        tracing::debug!("reading beacon type");
        let response = self.execute(Command::GetBeaconType).await?;
        match response {
            Response::BeaconType { mode } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BeaconType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the APRS position source (MS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_position_source(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading APRS position source");
        let response = self.execute(Command::GetPositionSource).await?;
        match response {
            Response::PositionSource { source } => Ok(source),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "PositionSource".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the TNC baud rate (AS write).
    ///
    /// Values: 0 = 1200 baud, 1 = 9600 baud.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_tnc_baud(&mut self, rate: TncBaud) -> Result<(), Error> {
        tracing::info!(?rate, "setting TNC baud rate");
        let response = self.execute(Command::SetTncBaud { rate }).await?;
        match response {
            Response::TncBaud { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TncBaud".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the beacon TX control mode (PT write).
    ///
    /// See [`get_beacon_type`](Self::get_beacon_type) for valid mode values and their meanings.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_beacon_type(&mut self, mode: BeaconMode) -> Result<(), Error> {
        tracing::info!(?mode, "setting beacon type");
        let response = self.execute(Command::SetBeaconType { mode }).await?;
        match response {
            Response::BeaconType { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BeaconType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Send a message via the APRS/TNC interface (MS write).
    ///
    /// # RF emission warning
    ///
    /// **This command causes the radio to transmit on the air.** The TNC will key the
    /// transmitter and send an AX.25 packet containing the message on the currently configured
    /// APRS frequency. Ensure you are authorized to transmit on the current frequency before
    /// calling this method.
    ///
    /// The transmission is a single packet burst (not continuous like [`transmit`](super::Radio::transmit)),
    /// but it still constitutes an RF emission that must comply with radio regulations.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn send_message(&mut self, text: &str) -> Result<(), Error> {
        tracing::info!("sending APRS message");
        let response = self
            .execute(Command::SendMessage {
                text: text.to_owned(),
            })
            .await?;
        match response {
            // MS write echoes back as an MS response, which the parser
            // decodes as PositionSource (the MS read variant). Both use
            // the same wire mnemonic.
            Response::PositionSource { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MS (PositionSource echo from message send)".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the radio's serial number and model code (AE read).
    ///
    /// Despite the AE mnemonic, this returns serial info, not APRS data.
    /// Returns `(serial, model_code)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_serial_info(&mut self) -> Result<(String, String), Error> {
        tracing::debug!("reading serial info");
        let response = self.execute(Command::GetSerialInfo).await?;
        match response {
            Response::SerialInfo { serial, model_code } => Ok((serial, model_code)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "SerialInfo".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
