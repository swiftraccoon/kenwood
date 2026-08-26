//! Packet/TNC control methods.
//!
//! The `TN` command reports both the packet operating mode and its data band.
//! Binary KISS and MMDVM transitions use exclusive session types; ordinary
//! CAT writes expose only the off and firmware-managed APRS modes.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{TncControlMode, TncDataBand, TncState};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get the TNC mode (TN bare read).
    ///
    /// Bare `TN\r` returns `TN mode,data_band` as named fields.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_tnc_mode(&mut self) -> Result<TncState, Error> {
        tracing::debug!("reading TNC mode");
        let response = self.execute(Command::GetTncMode).await?;
        match response {
            Response::TncMode { mode, data_band } => Ok(TncState { mode, data_band }),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "TncMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the CAT-selectable TNC mode (TN write).
    ///
    /// The wire form is `TN mode,data_band\r`. KISS and MMDVM are absent from
    /// [`TncControlMode`]; their transitions use consuming session APIs.
    ///
    /// # Errors
    ///
    /// Returns an error if the CAT command fails or its exact echo is
    /// unexpected.
    pub async fn set_tnc_mode(
        &mut self,
        control_mode: TncControlMode,
        data_band: TncDataBand,
    ) -> Result<(), Error> {
        let mode = control_mode.into();
        tracing::info!(?mode, ?data_band, "setting TNC mode");
        let response = self
            .execute(Command::SetTncMode { mode, data_band })
            .await?;
        match response {
            Response::TncMode {
                mode: actual_mode,
                data_band: actual_data_band,
            } if actual_mode == mode && actual_data_band == data_band => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("TncMode {{ mode: {mode:?}, data_band: {data_band:?} }}"),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
