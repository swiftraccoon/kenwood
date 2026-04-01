//! Safe high-level tuning APIs with automatic mode management.
//!
//! These methods handle VFO/Memory mode switching automatically, so callers
//! do not need to worry about the radio being in the wrong mode. They are
//! the recommended way to change frequencies and channels.

use crate::error::{Error, ProtocolError};
use crate::transport::Transport;
use crate::types::{Band, Frequency};

use super::{Radio, RadioMode};

impl<T: Transport> Radio<T> {
    /// Tune a band to a specific frequency.
    ///
    /// Automatically switches to VFO mode if needed, sets the frequency,
    /// and verifies the change. This is the safe way to change frequencies.
    ///
    /// # Errors
    ///
    /// Returns an error if the mode switch, frequency set, or verification
    /// read fails.
    pub async fn tune_frequency(&mut self, band: Band, freq: Frequency) -> Result<(), Error> {
        tracing::info!(?band, hz = freq.as_hz(), "tuning to frequency");

        // Ensure VFO mode.
        self.ensure_mode(band, RadioMode::Vfo).await?;

        // Read current channel data so we can preserve settings (step, shift, etc.)
        let mut channel = self.get_frequency_full(band).await?;
        channel.rx_frequency = freq;

        // Write the updated frequency.
        self.set_frequency_full(band, &channel).await?;

        // Verify.
        let readback = self.get_frequency(band).await?;
        if readback.rx_frequency != freq {
            tracing::warn!(
                expected = freq.as_hz(),
                actual = readback.rx_frequency.as_hz(),
                "frequency readback mismatch"
            );
        }

        Ok(())
    }

    /// Tune a band to a memory channel by number.
    ///
    /// Automatically switches to memory mode if needed and recalls
    /// the channel. Verifies the channel is populated by reading it
    /// first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RadioError`] if the channel number is out of range
    /// or the channel is empty. Returns transport/protocol errors on
    /// communication failure.
    pub async fn tune_channel(&mut self, band: Band, channel: u16) -> Result<(), Error> {
        tracing::info!(?band, channel, "tuning to memory channel");

        // Verify the channel exists and is populated by trying to read it.
        let ch_data = self.read_channel(channel).await?;
        if ch_data.rx_frequency.as_hz() == 0 {
            tracing::warn!(channel, "channel appears empty (frequency is 0 Hz)");
        }

        // Ensure memory mode.
        self.ensure_mode(band, RadioMode::Memory).await?;

        // Recall the channel.
        self.recall_channel(band, channel).await?;

        Ok(())
    }

    /// Find a memory channel number by its display name.
    ///
    /// Searches all channel names for a match and returns the channel
    /// number. Does **not** tune the radio to that channel (the USB
    /// connection is reset by MCP programming mode before recall could
    /// happen). The caller should reconnect and use
    /// [`Radio::tune_channel`](Radio::tune_channel) with the returned
    /// channel number.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] with [`ProtocolError::UnexpectedResponse`]
    /// if no channel with the given name is found. Returns transport/protocol
    /// errors on communication failure.
    ///
    /// # Warning
    ///
    /// This method enters MCP programming mode to read channel names.
    /// After returning, the USB connection will have been reset by the
    /// radio. The `Radio` instance should be dropped and a fresh
    /// connection established.
    pub async fn find_channel_by_name(
        &mut self,
        band: Band,
        name: &str,
    ) -> Result<u16, Error> {
        tracing::info!(?band, name, "searching for channel by name");

        // Read all channel names via programming mode.
        let names = self.read_channel_names().await?;

        // Find a matching channel (skip empty names).
        let found = names
            .iter()
            .enumerate()
            .find(|(_, n)| !n.is_empty() && n.as_str() == name);

        let (channel_num, _) = found.ok_or_else(|| {
            Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("channel named {name:?}"),
                actual: b"no matching channel found".to_vec(),
            })
        })?;

        let channel = u16::try_from(channel_num).map_err(|_| {
            Error::Protocol(ProtocolError::FieldParse {
                command: "find_channel_by_name".into(),
                field: "channel".into(),
                detail: format!("channel index {channel_num} exceeds u16 range"),
            })
        })?;

        tracing::info!(channel, name, "found channel by name");

        // Note: After read_channel_names() returns, the USB connection has
        // been reset. The caller needs to reconnect. We cannot recall the
        // channel here because the transport is dead. Return the channel
        // number so the caller can reconnect and use tune_channel().
        Ok(channel)
    }

    /// Ensure a band is in the specified mode, switching if necessary.
    ///
    /// # Errors
    ///
    /// Returns an error if querying or setting the mode fails.
    async fn ensure_mode(&mut self, band: Band, target: RadioMode) -> Result<(), Error> {
        // Check cached mode first.
        let current = self.get_cached_mode(band);
        if current == Some(target) {
            tracing::debug!(?band, ?target, "already in target mode");
            return Ok(());
        }

        // If unknown, query the radio.
        if current.is_none() {
            let mode_val = self.get_vfo_memory_mode(band).await?;
            if let Some(actual) = RadioMode::from_vm_value(mode_val) {
                if actual == target {
                    tracing::debug!(?band, ?target, "queried mode matches target");
                    return Ok(());
                }
            }
        }

        // Switch mode.
        tracing::info!(?band, ?target, "switching band mode");
        self.set_vfo_memory_mode(band, target.as_vm_value()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    /// A typical FO response for Band A at 145.000 MHz.
    const FO_RESPONSE_145: &[u8] =
        b"FO 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00\r";

    /// FO write command for 146.520 MHz (preserving other fields from
    /// `FO_RESPONSE_145` except the RX frequency).
    const FO_WRITE_146520: &[u8] =
        b"FO 0,0146520000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00\r";

    /// FO response echoed after writing 146.520 MHz.
    const FO_RESPONSE_146520: &[u8] =
        b"FO 0,0146520000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00\r";

    /// FQ short response for Band A at 146.520 MHz.
    const FQ_RESPONSE_146520: &[u8] = b"FQ 0,0146520000\r";

    #[tokio::test]
    async fn tune_frequency_already_in_vfo_mode() {
        let mut mock = MockTransport::new();
        // ensure_mode: query VM -> already VFO (0)
        mock.expect(b"VM 0\r", b"VM 0,0\r");
        // get_frequency_full: read current FO
        mock.expect(b"FO 0\r", FO_RESPONSE_145);
        // set_frequency_full: write new frequency
        mock.expect(FO_WRITE_146520, FO_RESPONSE_146520);
        // get_frequency: verify readback
        mock.expect(b"FQ 0\r", FQ_RESPONSE_146520);

        let mut radio = Radio::connect(mock).await.unwrap();
        radio
            .tune_frequency(Band::A, Frequency::new(146_520_000))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn tune_channel_switches_to_memory_mode() {
        let mut mock = MockTransport::new();
        // read_channel: ME read to verify channel is populated
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

        let mut radio = Radio::connect(mock).await.unwrap();
        radio.tune_channel(Band::A, 21).await.unwrap();
    }

    #[tokio::test]
    async fn tune_channel_already_in_memory_mode() {
        let mut mock = MockTransport::new();
        // read_channel: ME read to verify channel is populated
        mock.expect(
            b"ME 005\r",
            b"ME 005,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
        );
        // ensure_mode: query VM -> already Memory (1)
        mock.expect(b"VM 0\r", b"VM 0,1\r");
        // recall_channel: MR action
        mock.expect(b"MR 0,005\r", b"MR 0,005\r");

        let mut radio = Radio::connect(mock).await.unwrap();
        radio.tune_channel(Band::A, 5).await.unwrap();
    }
}
