//! Memory channel read/write methods.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{ChannelMemory, MemoryChannelRecord, MemorySelector};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read a memory channel by number (ME read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn read_channel(&mut self, channel: u16) -> Result<ChannelMemory, Error> {
        let selector = MemorySelector::try_from(channel)?;
        let record = self.read_memory(selector).await?;
        Ok(record.channel.channel)
    }

    /// Read a complete ME record by its exact selector.
    ///
    /// Unlike [`read_channel`](Self::read_channel), this preserves the CAT
    /// transmit-step field and both currently-unidentified ME-only fields.
    ///
    /// # Errors
    ///
    /// Returns a transport/protocol error if the command fails.
    pub async fn read_memory(
        &mut self,
        selector: MemorySelector,
    ) -> Result<MemoryChannelRecord, Error> {
        tracing::debug!(%selector, "reading memory record");
        let response = self.execute(Command::GetMemoryChannel { selector }).await?;
        match response {
            Response::MemoryChannel {
                selector: response_selector,
                record,
            } if response_selector == selector => Ok(record),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("MemoryChannel {{ selector: {selector} }}"),
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
                    // Channel is empty/not programmed, so skip it.
                }
                Err(e) => return Err(e),
            }
        }
        Ok(results)
    }

    /// Attempt to write a memory channel by number (ME write).
    ///
    /// This writer is quarantined. The former codec discarded four shared
    /// FO fields and both ME-only fields, then replaced them with zeroes. It
    /// remains unavailable until all 22 ME fields can be preserved and the
    /// restore/readback behavior is qualified on hardware.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    #[expect(
        clippy::unused_async,
        reason = "Compatibility quarantine: keep the existing async public API while returning \
                  before I/O until the full ME wire record is qualified"
    )]
    pub async fn write_channel(
        &mut self,
        channel: u16,
        _data: &ChannelMemory,
    ) -> Result<(), Error> {
        if channel > 999 {
            return Err(Error::Validation(
                crate::error::ValidationError::ChannelOutOfRange { channel, max: 999 },
            ));
        }
        Err(Error::UnqualifiedCatWrite {
            command: "ME",
            reason: "the current channel model cannot preserve all 22 wire fields",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// ME response for channel 5 with a valid frequency.
    const ME_RESP_005: &[u8] =
        b"ME 005,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r";

    #[tokio::test]
    async fn read_channels_returns_populated() -> TestResult {
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

        let mut radio = Radio::connect(mock).await?;
        let channels = radio.read_channels(0..3).await?;
        assert_eq!(channels.len(), 1);
        let first = channels.first().ok_or("channels[0] missing")?;
        assert_eq!(first.0, 1);
        assert_eq!(first.1.rx_frequency.as_hz(), 146_520_000);
        Ok(())
    }

    #[tokio::test]
    async fn read_channels_empty_range() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let channels = radio.read_channels(0..0).await?;
        assert!(channels.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn read_channel_populated() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ME 005\r", ME_RESP_005);
        let mut radio = Radio::connect(mock).await?;
        let data = radio.read_channel(5).await?;
        assert_eq!(data.rx_frequency.as_hz(), 440_000_000);
        Ok(())
    }

    #[tokio::test]
    async fn read_channel_not_available() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ME 999\r", b"N\r");
        let mut radio = Radio::connect(mock).await?;
        let result = radio.read_channel(999).await;
        assert!(
            matches!(result, Err(Error::NotAvailable)),
            "expected NotAvailable, got {result:?}"
        );
        Ok(())
    }
}
