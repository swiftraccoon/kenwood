//! High-level tuning and memory-recall APIs.
//!
//! Memory recall handles VFO/Memory mode switching automatically. Direct
//! frequency tuning is quarantined until the complete FO write record can be
//! preserved and verified.

use crate::error::{Error, ProtocolError};
use crate::transport::Transport;
use crate::types::{Band, Frequency, Mode, StepSize};

use super::{Radio, RadioMode};

impl<T: Transport> Radio<T> {
    /// Attempt to tune a band to a specific frequency.
    ///
    /// Frequency tuning is temporarily quarantined because the only formerly
    /// exposed writer was the lossy full-record FO path. This method returns
    /// before changing mode or performing any I/O.
    ///
    /// # Errors
    ///
    /// Returns an error if the mode switch, frequency set, or verification
    /// read fails.
    #[expect(
        clippy::unused_async,
        reason = "Compatibility quarantine: keep the existing async public API while returning \
                  before I/O until a frequency writer is qualified"
    )]
    pub async fn tune_frequency(&mut self, _band: Band, _freq: Frequency) -> Result<(), Error> {
        Err(Error::UnqualifiedCatWrite {
            command: "FO/FQ",
            reason: "FO is lossy and the short FQ writer has not been qualified",
        })
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

        // Verify the channel exists and is populated by trying to read
        // it. Recalling an empty channel would leave the radio in an
        // unusable state; the documented contract is an error.
        let ch_data = self.read_channel(channel).await?;
        if ch_data.rx_frequency.as_hz() == 0 {
            tracing::warn!(channel, "channel is empty (frequency is 0 Hz)");
            return Err(Error::RadioError);
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
    pub async fn find_channel_by_name(&mut self, band: Band, name: &str) -> Result<u16, Error> {
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

    /// Attempt to set frequency, operating mode, and step size in one call.
    ///
    /// This compatibility API currently returns the fail-closed error from
    /// [`tune_frequency`](Self::tune_frequency) before changing mode or
    /// performing I/O. If a direct-frequency writer is qualified later, the
    /// remaining mode and step operations must still retain their existing
    /// readback contracts.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the individual operations fail.
    pub async fn quick_tune(
        &mut self,
        band: Band,
        freq_hz: u32,
        mode: Mode,
        step: StepSize,
    ) -> Result<(), Error> {
        tracing::info!(?band, freq_hz, ?mode, ?step, "quick-tuning band");

        // Set frequency (handles VFO mode switch internally).
        self.tune_frequency(band, Frequency::new(freq_hz)).await?;

        // Set operating mode.
        self.set_mode(band, mode).await?;

        // Set step size.
        self.set_step_size(band, step).await?;

        Ok(())
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
            let vfo_mode = self.get_vfo_memory_mode(band).await?;
            let actual = RadioMode::from_vfo_mode(vfo_mode);
            if actual == target {
                tracing::debug!(?band, ?target, "queried mode matches target");
                return Ok(());
            }
        }

        // Switch mode.
        tracing::info!(?band, ?target, "switching band mode");
        self.set_vfo_memory_mode(band, target.as_vfo_mode()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn tune_frequency_is_quarantined_before_io() -> TestResult {
        // No exchanges are scripted. Any VM/FO/FQ access would produce a
        // transport error instead of the explicit pre-I/O quarantine error.
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .tune_frequency(Band::A, Frequency::new(146_520_000))
            .await;
        assert!(
            matches!(
                result,
                Err(Error::UnqualifiedCatWrite {
                    command: "FO/FQ",
                    ..
                })
            ),
            "frequency tuning must fail before changing mode or performing I/O: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tune_channel_switches_to_memory_mode() -> TestResult {
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

        let mut radio = Radio::connect(mock).await?;
        radio.tune_channel(Band::A, 21).await?;
        Ok(())
    }

    #[tokio::test]
    async fn tune_channel_already_in_memory_mode() -> TestResult {
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

        let mut radio = Radio::connect(mock).await?;
        radio.tune_channel(Band::A, 5).await?;
        Ok(())
    }

    #[tokio::test]
    async fn quick_tune_stops_before_mode_or_step_io() -> TestResult {
        // No exchanges are scripted. This proves quick_tune does not proceed
        // to VM, MD, or SF after the quarantined frequency operation fails.
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .quick_tune(Band::A, 146_520_000, Mode::Fm, StepSize::Hz5000)
            .await;
        assert!(
            matches!(
                result,
                Err(Error::UnqualifiedCatWrite {
                    command: "FO/FQ",
                    ..
                })
            ),
            "quick_tune must stop before mode or step I/O: {result:?}"
        );
        Ok(())
    }
}
