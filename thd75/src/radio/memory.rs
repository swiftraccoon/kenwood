//! Memory channel read/write methods.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{CatMemoryChannelRecord, MemoryChannelAddress, RegularChannel};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Read a memory channel by number (ME read).
    ///
    /// Returns the complete ME record, including split and scan-lockout state.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_regular_channel_record(
        &mut self,
        channel: RegularChannel,
    ) -> Result<CatMemoryChannelRecord, Error> {
        self.get_channel_record(channel.into()).await
    }

    /// Read a complete ME record by its exact selector.
    ///
    /// Unlike [`get_regular_channel_record`](Self::get_regular_channel_record), this accepts every ME
    /// selector form rather than only regular channels 000-999.
    ///
    /// # Errors
    ///
    /// Returns a transport/protocol error if the command fails.
    pub async fn get_channel_record(
        &mut self,
        selector: MemoryChannelAddress,
    ) -> Result<CatMemoryChannelRecord, Error> {
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

    /// Read multiple memory channels sequentially.
    ///
    /// Reads channels in the given range and returns their complete ME records,
    /// including split and scan-lockout state. Only occupied channels are
    /// returned; channels that report N/not available are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if a transport or protocol error occurs (other than
    /// the radio returning N for an empty channel).
    pub async fn read_regular_channel_records<I>(
        &mut self,
        channels: I,
    ) -> Result<Vec<(RegularChannel, CatMemoryChannelRecord)>, Error>
    where
        I: IntoIterator<Item = RegularChannel>,
    {
        let channels: Vec<_> = channels.into_iter().collect();
        tracing::debug!(count = channels.len(), "reading memory channels");
        let mut results = Vec::new();
        for channel in channels {
            match self.get_regular_channel_record(channel).await {
                Ok(data) => {
                    // Skip channels with a zero frequency (empty).
                    if data.channel.receive_frequency.as_hz() != 0 {
                        results.push((channel, data));
                    }
                }
                Err(Error::NotAvailableInCurrentMode { .. }) => {
                    // Channel is empty/not programmed, so skip it.
                }
                Err(e) => return Err(e),
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::{ChannelTransmitValue, Frequency};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// ME response for channel 5 with a valid frequency.
    const ME_RESP_005: &[u8] =
        b"ME 005,0440000000,0005000000,0,0,0,0,0,1,0,0,0,0,1,2,14,14,023,0,REPEATER,1,05,1\r";

    #[tokio::test]
    async fn read_regular_channel_records_returns_populated() -> TestResult {
        let mut mock = MockTransport::new();
        // Channel 0: not available.
        mock.expect(b"ME 000\r", b"N\r");
        // Channel 1: populated.
        mock.expect(
            b"ME 001\r",
            b"ME 001,0146520000,0000600000,5,0,0,0,0,0,0,0,0,0,1,0,08,08,000,0,,0,00,1\r",
        );
        // Channel 2: not available.
        mock.expect(b"ME 002\r", b"N\r");

        let mut radio = Radio::new(mock);
        let channels = radio
            .read_regular_channel_records(RegularChannel::range_inclusive(
                RegularChannel::new(0)?,
                RegularChannel::new(2)?,
            ))
            .await?;
        assert_eq!(channels.len(), 1);
        let first = channels.first().ok_or("channels[0] missing")?;
        assert_eq!(first.0, RegularChannel::new(1)?);
        assert_eq!(first.1.channel.receive_frequency.as_hz(), 146_520_000);
        assert!(first.1.split);
        assert!(first.1.scan_lockout);
        assert_eq!(
            first.1.transmit_value(),
            ChannelTransmitValue::SplitTransmitFrequency(Frequency::new(600_000)),
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_regular_channel_records_empty_range() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::new(mock);
        let channels = radio
            .read_regular_channel_records(std::iter::empty())
            .await?;
        assert!(channels.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn get_regular_channel_record_populated() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ME 005\r", ME_RESP_005);
        let mut radio = Radio::new(mock);
        let data = radio
            .get_regular_channel_record(RegularChannel::new(5)?)
            .await?;
        assert_eq!(data.channel.receive_frequency.as_hz(), 440_000_000);
        assert!(data.split);
        assert!(data.scan_lockout);
        assert_eq!(
            data.transmit_value(),
            ChannelTransmitValue::SplitTransmitFrequency(Frequency::new(5_000_000)),
        );
        Ok(())
    }

    #[tokio::test]
    async fn get_regular_channel_record_not_available() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ME 999\r", b"N\r");
        let mut radio = Radio::new(mock);
        let result = radio
            .get_regular_channel_record(RegularChannel::new(999)?)
            .await;
        assert!(
            matches!(result, Err(Error::NotAvailableInCurrentMode { .. })),
            "expected NotAvailableInCurrentMode, got {result:?}"
        );
        Ok(())
    }
}
