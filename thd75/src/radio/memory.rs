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
        let response = self.execute(Command::GetMemoryChannel { channel }).await?;
        match response {
            Response::MemoryChannel { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "MemoryChannel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read multiple memory channels efficiently.
    ///
    /// Reads channels in the given range and returns only occupied channels
    /// (skips channels that return N/not available).
    ///
    /// # Errors
    ///
    /// Returns an error if a transport or protocol error occurs (other than
    /// the radio returning N for an empty channel).
    pub async fn read_channels(
        &mut self,
        range: std::ops::Range<u16>,
    ) -> Result<Vec<(u16, ChannelMemory)>, Error> {
        tracing::debug!(
            start = range.start,
            end = range.end,
            "reading memory channels"
        );
        let mut results = Vec::new();
        for ch in range {
            match self.read_channel(ch).await {
                Ok(data) => {
                    // Skip channels with a zero frequency (empty).
                    if data.rx_frequency.as_hz() != 0 {
                        results.push((ch, data));
                    }
                }
                Err(Error::NotAvailable) => {
                    // Channel is empty/not programmed — skip.
                }
                Err(e) => return Err(e),
            }
        }
        Ok(results)
    }

    /// Write a memory channel by number (ME write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn write_channel(&mut self, channel: u16, data: &ChannelMemory) -> Result<(), Error> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    /// ME response for channel 5 with a valid frequency.
    const ME_RESP_005: &[u8] =
        b"ME 005,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r";

    #[tokio::test]
    async fn read_channels_returns_populated() {
        let mut mock = MockTransport::new();
        // Channel 0: not available.
        mock.expect(b"ME 000\r", b"N\r");
        // Channel 1: populated.
        mock.expect(
            b"ME 001\r",
            b"ME 001,0146520000,0000600000,5,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
        );
        // Channel 2: not available.
        mock.expect(b"ME 002\r", b"N\r");

        let mut radio = Radio::connect(mock).await.unwrap();
        let channels = radio.read_channels(0..3).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].0, 1);
        assert_eq!(channels[0].1.rx_frequency.as_hz(), 146_520_000);
    }

    #[tokio::test]
    async fn read_channels_empty_range() {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await.unwrap();
        let channels = radio.read_channels(0..0).await.unwrap();
        assert!(channels.is_empty());
    }

    #[tokio::test]
    async fn read_channel_populated() {
        let mut mock = MockTransport::new();
        mock.expect(b"ME 005\r", ME_RESP_005);
        let mut radio = Radio::connect(mock).await.unwrap();
        let data = radio.read_channel(5).await.unwrap();
        assert_eq!(data.rx_frequency.as_hz(), 440_000_000);
    }

    #[tokio::test]
    async fn read_channel_not_available() {
        let mut mock = MockTransport::new();
        mock.expect(b"ME 999\r", b"N\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let result = radio.read_channel(999).await;
        assert!(matches!(result, Err(Error::NotAvailable)));
    }
}
