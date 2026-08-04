//! APRS (Automatic Packet Reporting System) subsystem methods.
//!
//! APRS is a digital communications protocol for real-time tactical information exchange. The
//! TH-D75 has a built-in TNC (Terminal Node Controller) that handles AX.25 packet encoding and
//! decoding, supporting both 1200 bps (VHF, standard APRS on 144.390 MHz in North America) and
//! 9600 bps (UHF) operation.
//!
//! The TNC handles position beaconing, message exchange, and weather reporting. Beacon
//! transmission is controlled by the beacon mode setting (PT command), which determines whether
//! beacons are sent manually, at fixed intervals, or based on `SmartBeaconing` rules.
//!
//! # Related commands
//!
//! - **AS**: Packet-data rate (1200/9600 bps)
//! - **PT**: Beacon TX control mode
//! - **MS**: My Position selection
//! - **CS**: APRS My Callsign
//! - **BE**: Sends an APRS beacon (transmits on air, so it requires a valid
//!   amateur licence and appropriate authorisation; use deliberately)

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{AprsCallsign, BeaconMode, MyPositionSelection, PacketDataRate};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Trigger one APRS beacon transmission (BE action).
    ///
    /// # On-air operation
    ///
    /// A successful call asks the radio to transmit immediately. Callers are
    /// responsible for a valid amateur-radio licence, an appropriate
    /// frequency, a configured APRS identity and path, and all applicable
    /// operating rules. The radio returns `N` when its TNC is not ready.
    ///
    /// # Errors
    ///
    /// Returns an error if the radio rejects the action, transport fails, or
    /// the response is not the exact bare `BE` acknowledgement.
    pub async fn transmit_aprs_beacon(&mut self) -> Result<(), Error> {
        tracing::info!("requesting an immediate APRS beacon transmission");
        let response = self.execute(Command::TransmitAprsBeacon).await?;
        match response {
            Response::AprsBeaconTransmitAck => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AprsBeaconTransmitAck".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read the APRS My Callsign value (CS read).
    ///
    /// This is the live CAT view of MCP `aprs.MyCallsign`; it is not a
    /// D-STAR slot selector.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the callsign response is invalid.
    /// An unconfigured radio slot is returned as `None`.
    pub async fn get_aprs_callsign(&mut self) -> Result<Option<AprsCallsign>, Error> {
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
        tracing::info!(callsign = %callsign, "setting APRS My Callsign");
        let response = self.execute(Command::SetAprsCallsign { callsign }).await?;
        match response {
            Response::AprsCallsign { callsign: Some(_) } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AprsCallsign".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the packet-data rate (AS read).
    ///
    /// Returns 0 = 1200 bps, 1 = 9600 bps.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_packet_data_rate(&mut self) -> Result<PacketDataRate, Error> {
        tracing::debug!("reading packet data rate");
        let response = self.execute(Command::GetPacketDataRate).await?;
        match response {
            Response::PacketDataRate { data_rate } => Ok(data_rate),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "PacketDataRate".into(),
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
    pub async fn get_beacon_mode(&mut self) -> Result<BeaconMode, Error> {
        tracing::debug!("reading beacon mode");
        let response = self.execute(Command::GetBeaconMode).await?;
        match response {
            Response::BeaconMode { mode } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BeaconMode".into(),
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

    /// Set the packet-data rate (AS write).
    ///
    /// Values: 0 = 1200 bps, 1 = 9600 bps.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_packet_data_rate(&mut self, data_rate: PacketDataRate) -> Result<(), Error> {
        tracing::info!(?data_rate, "setting packet data rate");
        let response = self
            .execute(Command::SetPacketDataRate { data_rate })
            .await?;
        match response {
            Response::PacketDataRate { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "PacketDataRate".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the beacon TX control mode (PT write).
    ///
    /// See [`get_beacon_mode`](Self::get_beacon_mode) for valid mode values and their meanings.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_beacon_mode(&mut self, mode: BeaconMode) -> Result<(), Error> {
        tracing::info!(?mode, "setting beacon mode");
        let response = self.execute(Command::SetBeaconMode { mode }).await?;
        match response {
            Response::BeaconMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BeaconMode".into(),
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
}
