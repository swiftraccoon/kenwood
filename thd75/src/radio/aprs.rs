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
//! - **MS**: My Position selection
//! - **CS**: APRS My Callsign
//! - **AE**: Serial number info (not actually APRS-related, but shares the A prefix)
//! - **BE**: Sends an APRS beacon (transmits on air, so it requires a valid
//!   amateur licence and appropriate authorisation; use deliberately)

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{AprsCallsign, BeaconMode, MyPositionSelection, TncBaud};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read the APRS My Callsign value (CS read).
    ///
    /// This is the live CAT view of MCP `aprs.MyCallsign`; it is not a
    /// D-STAR slot selector.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the callsign response is invalid.
    pub async fn get_aprs_callsign(&mut self) -> Result<AprsCallsign, Error> {
        tracing::debug!("reading APRS My Callsign");
        let response = self.execute(Command::GetAprsCallsign).await?;
        match response {
            Response::AprsCallsign { callsign } => Ok(callsign),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AprsCallsign".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write the APRS My Callsign value (CS write).
    ///
    /// # Errors
    ///
    /// Returns an error if the radio rejects the value or returns an
    /// unexpected response. Persistence across a power cycle has not yet been
    /// dynamically qualified, so callers should verify with
    /// [`get_aprs_callsign`](Self::get_aprs_callsign).
    pub async fn set_aprs_callsign(&mut self, callsign: AprsCallsign) -> Result<(), Error> {
        tracing::info!(callsign = callsign.as_str(), "setting APRS My Callsign");
        let response = self.execute(Command::SetAprsCallsign { callsign }).await?;
        match response {
            Response::AprsCallsign { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AprsCallsign".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

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
    /// - `0` = Manual (beacon sent only when explicitly triggered)
    /// - `1` = PTT (beacon sent after each PTT release)
    /// - `2` = Auto (beacon sent at fixed intervals set by the beacon interval timer)
    /// - `3` = `SmartBeaconing` (adaptive beaconing based on speed and direction changes)
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

    /// Get the selected APRS/GPS My Position entry (MS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_my_position_selection(&mut self) -> Result<MyPositionSelection, Error> {
        tracing::debug!("reading My Position selection");
        let response = self.execute(Command::GetMyPositionSelection).await?;
        match response {
            Response::MyPositionSelection { selection } => Ok(selection),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MyPositionSelection".into(),
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

    /// Set the selected APRS/GPS My Position entry (MS write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_my_position_selection(
        &mut self,
        selection: MyPositionSelection,
    ) -> Result<(), Error> {
        tracing::info!(?selection, "setting My Position selection");
        let response = self
            .execute(Command::SetMyPositionSelection { selection })
            .await?;
        match response {
            Response::MyPositionSelection { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MyPositionSelection".into(),
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
