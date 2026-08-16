//! High-level memory-recall APIs.
//!
//! Memory recall handles tuning-mode switching automatically. The crate
//! deliberately exposes no arbitrary-frequency writer: retained evidence does
//! not qualify the complete FO record's write and read-back behavior.

use crate::error::{Error, ProtocolError};
use crate::transport::Transport;
use crate::types::{Band, ChannelDisplayName, Frequency, RegularChannel, StepSize, TuningMode};

use super::Radio;

/// Upper bound on UP/DW steps a single verified walk will perform.
pub const MAX_RETUNE_STEPS: u32 = 1_000;

/// Consecutive verified-as-unmoved steps after which a walk fails closed.
///
/// A stalled walk means the radio is acknowledging step commands without
/// acting on them (or something else owns the VFO); retrying forever would
/// hide that.
pub const MAX_RETUNE_STALLS: u8 = 5;

/// Check that `target` is a whole, bounded number of steps from `current`.
pub(crate) const fn preflight_walk(
    current: Frequency,
    target: Frequency,
    step: StepSize,
) -> Result<(), Error> {
    let step_hz = step.as_hz();
    let span = current.as_hz().abs_diff(target.as_hz());
    if !span.is_multiple_of(step_hz) {
        return Err(Error::RetuneOffStep {
            current,
            target,
            step,
        });
    }
    let steps_required = span / step_hz;
    if steps_required > MAX_RETUNE_STEPS {
        return Err(Error::RetuneSpanTooLarge {
            steps_required,
            maximum: MAX_RETUNE_STEPS,
        });
    }
    Ok(())
}

impl<T: Transport> Radio<T> {
    /// Step the band's VFO to `target` with individually verified UP/DW
    /// steps.
    ///
    /// This is an ACTION composite: it selects `band` as the active band
    /// when necessary (UP/DW act on the active band), requires that band to
    /// be in VFO tuning mode (so a selected memory, call, or weather channel
    /// is never changed), and then walks to the target one step at a time.
    /// Every step is verified by a frequency readback before the next step
    /// is sent. Hardware-observed (live radio, 2026-08-09): the radio
    /// acknowledges rapid consecutive step commands but can swallow all
    /// except the last, so a fire-and-forget burst lands short; the verified
    /// walk retries a swallowed step and fails closed after
    /// [`MAX_RETUNE_STALLS`] consecutive steps with no movement.
    ///
    /// Direct FQ/FO frequency writes remain quarantined; this walk is built
    /// entirely from the qualified UP/DW stepping commands.
    ///
    /// Returns the verified frequency.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use kenwood_thd75::radio::Radio;
    /// # use kenwood_thd75::transport::SerialTransport;
    /// # use kenwood_thd75::types::{Band, Frequency};
    /// # async fn example() -> Result<(), kenwood_thd75::error::Error> {
    /// let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
    /// let mut radio = Radio::new(transport);
    ///
    /// // Band B must be in VFO tuning mode; the walk verifies every step.
    /// let landed = radio
    ///     .step_tune(Band::B, Frequency::from_mhz_str("145.240")?)
    ///     .await?;
    /// println!("landed on {landed}");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::VfoTuningRequired`] when the band is not in VFO
    /// tuning mode, [`Error::RetuneOffStep`] when `target` is not a whole
    /// number of tuning steps away, [`Error::RetuneSpanTooLarge`] beyond
    /// [`MAX_RETUNE_STEPS`] steps, and [`Error::RetuneNotVerified`] when the
    /// walk stalls or exceeds its budget.
    pub async fn step_tune(&mut self, band: Band, target: Frequency) -> Result<Frequency, Error> {
        if self.get_band().await? != band {
            self.set_band(band).await?;
        }
        let tuning_mode = self.get_tuning_mode(band).await?;
        if tuning_mode != TuningMode::Vfo {
            return Err(Error::VfoTuningRequired {
                band,
                current: tuning_mode,
            });
        }
        let current = self.get_frequency(band).await?;
        if current == target {
            return Ok(current);
        }
        let step = self.get_step_size(band).await?;
        preflight_walk(current, target, step)?;
        self.verified_step_walk(band, current, target, step).await
    }

    /// The shared verified walk: one step, one readback, stall detection.
    pub(crate) async fn verified_step_walk(
        &mut self,
        band: Band,
        mut current: Frequency,
        target: Frequency,
        step: StepSize,
    ) -> Result<Frequency, Error> {
        let step_hz = step.as_hz();
        let mut attempts: u32 = 0;
        let mut consecutive_stalls: u8 = 0;
        while current != target {
            if attempts >= MAX_RETUNE_STEPS {
                return Err(Error::RetuneNotVerified {
                    requested: target,
                    actual: current,
                });
            }
            let remaining = current.as_hz().abs_diff(target.as_hz());
            if !remaining.is_multiple_of(step_hz) {
                return Err(Error::RetuneOffStep {
                    current,
                    target,
                    step,
                });
            }
            if target.as_hz() > current.as_hz() {
                self.frequency_up_blind().await?;
            } else {
                self.frequency_down_blind().await?;
            }
            attempts = attempts.saturating_add(1);
            let landed = self.get_frequency(band).await?;
            if landed == current {
                consecutive_stalls = consecutive_stalls.saturating_add(1);
                if consecutive_stalls >= MAX_RETUNE_STALLS {
                    return Err(Error::RetuneNotVerified {
                        requested: target,
                        actual: landed,
                    });
                }
            } else {
                consecutive_stalls = 0;
            }
            current = landed;
        }
        Ok(current)
    }

    /// Tune a band to a memory channel by number.
    ///
    /// Automatically switches to memory mode if needed and recalls
    /// the channel. Verifies the channel is populated by reading it
    /// first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyMemoryChannel`] if the channel is empty. Returns
    /// transport/protocol errors on communication failure.
    pub async fn tune_channel(&mut self, band: Band, channel: RegularChannel) -> Result<(), Error> {
        tracing::info!(?band, %channel, "tuning to memory channel");

        // Verify the channel exists and is populated by trying to read
        // it. Recalling an empty channel would leave the radio in an
        // unusable state; the documented contract is an error.
        let ch_data = self.get_regular_channel_record(channel).await?;
        if ch_data.channel.receive_frequency.as_hz() == 0 {
            tracing::warn!(%channel, "channel is empty (frequency is 0 Hz)");
            return Err(Error::EmptyMemoryChannel { channel });
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

    #[tokio::test]
    async fn step_tune_walks_the_active_band_vfo_with_verified_steps() -> TestResult {
        use crate::types::Frequency;
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        let mut radio = Radio::new(mock);
        let landed = radio
            .step_tune(Band::B, Frequency::new(145_005_000))
            .await?;
        assert_eq!(landed.as_hz(), 145_005_000);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_tune_switches_the_active_band_first() -> TestResult {
        use crate::types::Frequency;
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        let mut radio = Radio::new(mock);
        let landed = radio
            .step_tune(Band::B, Frequency::new(145_000_000))
            .await?;
        assert_eq!(landed.as_hz(), 145_000_000);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_tune_refuses_non_vfo_tuning_before_any_step() -> TestResult {
        use crate::types::Frequency;
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"VM 1\r", b"VM 1,1\r");
        let mut radio = Radio::new(mock);
        let result = radio.step_tune(Band::B, Frequency::new(145_000_000)).await;
        assert!(
            matches!(
                result,
                Err(Error::VfoTuningRequired {
                    band: Band::B,
                    current: TuningMode::Memory
                })
            ),
            "memory tuning mode must refuse stepped tuning: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }
}
