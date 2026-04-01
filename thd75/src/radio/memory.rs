//! Memory channel read/write methods.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::ChannelMemory;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read a memory channel by number (ME read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn read_channel(&mut self, channel: u16) -> Result<ChannelMemory, Error> {
        tracing::debug!(channel, "reading memory channel");
        let response = self
            .execute(Command::GetMemoryChannel { channel })
            .await?;
        match response {
            Response::MemoryChannel { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MemoryChannel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write a memory channel by number (ME write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn write_channel(
        &mut self,
        channel: u16,
        data: &ChannelMemory,
    ) -> Result<(), Error> {
        tracing::info!(channel, "writing memory channel");
        let response = self
            .execute(Command::SetMemoryChannel {
                channel,
                data: data.clone(),
            })
            .await?;
        match response {
            Response::MemoryChannel { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MemoryChannel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
