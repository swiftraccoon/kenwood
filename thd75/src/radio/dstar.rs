//! D-STAR subsystem methods.

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
