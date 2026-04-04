//! D-STAR (Digital Smart Technologies for Amateur Radio) subsystem methods.
//!
//! D-STAR is a digital voice and data protocol developed by JARL (Japan Amateur Radio League).
//! The TH-D75 supports D-STAR voice (DV mode) and data, including gateway linking for
//! internet-connected repeater access.
//!
//! # Command relationships
//!
//! - **DS**: selects the active D-STAR callsign slot (which stored callsign configuration to use)
//! - **CS**: selects the active callsign slot number (0-10) — similar to DS but for the CS
//!   slot register. The actual callsign text is read via DC.
//! - **DC**: reads D-STAR callsign data for a given slot (1-6). This command lives in
//!   [`audio.rs`](super) because it was discovered during audio subsystem probing — the DC
//!   mnemonic is overloaded on the D75 compared to the D74.
//! - **GW**: D-STAR gateway setting for repeater linking

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get the active D-STAR callsign slot (DS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_dstar_slot(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading D-STAR callsign slot");
        let response = self.execute(Command::GetDstarSlot).await?;
        match response {
            Response::DstarSlot { slot } => Ok(slot),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DstarSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the active callsign slot number (CS bare read).
    ///
    /// CS returns a slot number (0-10), NOT the callsign text itself.
    /// The actual callsign text is accessible via the CS callsign slots.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_active_callsign_slot(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading active callsign slot");
        let response = self.execute(Command::GetActiveCallsignSlot).await?;
        match response {
            Response::ActiveCallsignSlot { slot } => Ok(slot),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ActiveCallsignSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the active callsign slot (CS write).
    ///
    /// Selects which callsign slot is active. The callsign text itself
    /// is read via DC (D-STAR callsign) slots 1-6.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_active_callsign_slot(&mut self, slot: u8) -> Result<(), Error> {
        tracing::info!(slot, "setting active callsign slot");
        let response = self
            .execute(Command::SetActiveCallsignSlot { slot })
            .await?;
        match response {
            Response::ActiveCallsignSlot { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ActiveCallsignSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the active D-STAR callsign slot (DS write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_dstar_slot(&mut self, slot: u8) -> Result<(), Error> {
        tracing::info!(slot, "setting D-STAR callsign slot");
        let response = self.execute(Command::SetDstarSlot { slot }).await?;
        match response {
            Response::DstarSlot { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DstarSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the gateway value (GW read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gateway(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading D-STAR gateway");
        let response = self.execute(Command::GetGateway).await?;
        match response {
            Response::Gateway { value } => Ok(value),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Gateway".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the gateway value (GW write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_gateway(&mut self, value: u8) -> Result<(), Error> {
        tracing::info!(value, "setting D-STAR gateway");
        let response = self.execute(Command::SetGateway { value }).await?;
        match response {
            Response::Gateway { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Gateway".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
