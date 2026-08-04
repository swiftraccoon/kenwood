//! High-level memory-recall APIs.
//!
//! Memory recall handles tuning-mode switching automatically. The crate
//! deliberately exposes no arbitrary-frequency writer: retained evidence does
//! not qualify the complete FO record's write and read-back behavior.

use crate::error::{Error, ProtocolError};
use crate::transport::Transport;
use crate::types::{Band, ChannelDisplayName, RegularChannel, TuningMode};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Tune a band to a memory channel by number.
    ///
    /// Automatically switches to memory mode if needed and recalls
    /// the channel. Verifies the channel is populated by reading it
    /// first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CommandRejected`] if the channel is empty. Returns
    /// transport/protocol errors on communication failure.
    pub async fn tune_channel(&mut self, band: Band, channel: RegularChannel) -> Result<(), Error> {
        tracing::info!(?band, %channel, "tuning to memory channel");

        // Verify the channel exists and is populated by trying to read
        // it. Recalling an empty channel would leave the radio in an
        // unusable state; the documented contract is an error.
        let ch_data = self.get_regular_channel_record(channel).await?;
        if ch_data.channel.receive_frequency.as_hz() == 0 {
            tracing::warn!(%channel, "channel is empty (frequency is 0 Hz)");
            return Err(Error::CommandRejected);
        }

        // Ensure memory mode.
        self.ensure_tuning_mode(band, TuningMode::Memory).await?;

        // Recall the channel.
        self.recall_channel(band, channel).await?;

        Ok(())
    }

    /// Find a memory channel number by its display name.
    ///
    /// Searches all channel names for a match and returns the channel
    /// number. Does **not** tune the radio to that channel. MCP exit resets
    /// USB, so this method waits for re-enumeration and proves CAT identity
    /// before returning. The returned value can be passed directly to
    /// [`Radio::tune_channel`](Radio::tune_channel).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] with [`ProtocolError::UnexpectedResponse`]
    /// if no channel with the given name is found. Returns transport/protocol
    /// errors on communication failure.
    ///
    /// This method temporarily enters MCP programming mode and restores a
    /// qualified CAT session before it returns.
    pub async fn find_channel_by_name_via_mcp(
        &mut self,
        name: &ChannelDisplayName,
    ) -> Result<RegularChannel, Error> {
        tracing::info!(name = %name, "searching for regular channel by name");

        // Read all channel names via programming mode.
        let names = self.read_channel_names().await?;

        // Find a matching channel (skip empty names).
        let found = names
            .iter()
            .enumerate()
            .find(|(_, candidate)| !candidate.is_empty() && *candidate == name);

        let (channel_num, _) = found.ok_or_else(|| {
            Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("channel named {name:?}"),
                actual: b"no matching channel found".to_vec(),
            })
        })?;

        let channel_number = u16::try_from(channel_num).map_err(|_| {
            Error::Protocol(ProtocolError::FieldParse {
                command: "find_channel_by_name_via_mcp".into(),
                field: "channel".into(),
                detail: format!("channel index {channel_num} exceeds u16 range"),
            })
        })?;
        let channel = RegularChannel::new(channel_number)?;

        tracing::info!(channel = channel.as_raw(), name = %name, "found channel by name");

        // Searching and tuning remain separate operations so callers decide
        // which receiver band should recall the matching regular channel.
        Ok(channel)
    }

    /// Ensure a band is in the specified mode, switching if necessary.
    ///
    /// # Errors
    ///
    /// Returns an error if querying or setting the mode fails.
    async fn ensure_tuning_mode(&mut self, band: Band, target: TuningMode) -> Result<(), Error> {
        // Check the cached tuning mode first.
        let current = self.cached_tuning_mode(band);
        if current == Some(target) {
            tracing::debug!(?band, ?target, "already in target mode");
            return Ok(());
        }

        // If unknown, query the radio.
        if current.is_none() {
            let actual = self.get_tuning_mode(band).await?;
            if actual == target {
                tracing::debug!(?band, ?target, "queried mode matches target");
                return Ok(());
            }
        }

        // Switch mode.
        tracing::info!(?band, ?target, "switching band mode");
        self.set_tuning_mode(band, target).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn tune_channel_switches_to_memory_mode() -> TestResult {
        let mut mock = MockTransport::new();
        // get_regular_channel_record: ME read to verify channel is populated
        mock.expect(
            b"ME 021\r",
            b"ME 021,0146520000,0000600000,5,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
        );
        // ensure_mode: query VM -> VFO (0), need to switch
        mock.expect(b"VM 0\r", b"VM 0,0\r");
        // ensure_mode: switch to memory mode (1)
        mock.expect(b"VM 0,1\r", b"VM 0,1\r");
        // recall_channel: MR action
        mock.expect(b"MR 0,021\r", b"MR 0,021\r");

        let mut radio = Radio::new(mock);
        radio
            .tune_channel(Band::A, RegularChannel::new(21)?)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn tune_channel_already_in_memory_mode() -> TestResult {
        let mut mock = MockTransport::new();
        // get_regular_channel_record: ME read to verify channel is populated
        mock.expect(
            b"ME 005\r",
            b"ME 005,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
        );
        // ensure_mode: query VM -> already Memory (1)
        mock.expect(b"VM 0\r", b"VM 0,1\r");
        // recall_channel: MR action
        mock.expect(b"MR 0,005\r", b"MR 0,005\r");

        let mut radio = Radio::new(mock);
        radio.tune_channel(Band::A, RegularChannel::new(5)?).await?;
        Ok(())
    }
}
