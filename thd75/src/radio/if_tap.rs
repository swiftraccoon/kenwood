//! Typed IF-tap session: the Menu 102 USB-audio-output lifecycle.
//!
//! The TH-D75 can route Band B's intermediate-frequency signal (or its
//! detector output) to the USB audio interface. Engaging that tap requires a
//! specific radio state (Single Band mode on Band B) and, once engaged,
//! frequency and band writes are rejected until the output drops back to the
//! normal audio path. This module owns that whole lifecycle so applications
//! do not each re-implement the save / configure / verify / restore
//! choreography:
//!
//! - [`Radio::enter_if_tap`] guards preconditions (Band B must be in VFO
//!   tuning mode so a selected memory, call, or weather channel is never
//!   changed), snapshots every setting it will touch, applies the requested
//!   configuration, and proves engagement by reading the output selection
//!   back. Every configured value is independently read back after its setter
//!   response. On a mid-configure failure the already-applied settings are
//!   rolled back before the error is returned.
//! - [`IfTapSession::step_to_frequency`] retunes with the qualified UP/DW
//!   stepping commands, dropping the tap to the audio path first (frequency
//!   writes are rejected while the IF tap is engaged) and re-engaging it
//!   afterwards.
//! - [`IfTapSession::exit`] restores the snapshot in the hardware-required
//!   order: the output selection is forced to the audio path first, the saved
//!   tuning step is restored before the saved Band B frequency is walked back,
//!   the saved output selection is re-applied last, and every failure is
//!   reported per step in a typed [`IfTapRestoreReport`].
//!
//! Long-lived applications that cannot keep a borrow alive can detach the
//! snapshot with [`IfTapSession::into_saved_state`] and restore later through
//! [`Radio::restore_if_tap`].

use crate::error::{Error, ProtocolError};
use crate::transport::Transport;
use crate::types::{
    Band, BandMode, Frequency, OperatingMode, SquelchLevel, StepSize, TuningMode, UsbAudioOutput,
};

use super::Radio;

pub use super::tuning::{MAX_RETUNE_STALLS, MAX_RETUNE_STEPS};

use super::tuning::preflight_walk;

/// Requested IF-tap configuration for [`Radio::enter_if_tap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IfTapConfig {
    operating_mode: OperatingMode,
    squelch: SquelchLevel,
    step: Option<StepSize>,
    output: UsbAudioOutput,
}

impl IfTapConfig {
    /// Configuration that engages the intermediate-frequency output with the
    /// squelch held open and the tuning step left untouched.
    #[must_use]
    pub const fn new(operating_mode: OperatingMode) -> Self {
        Self {
            operating_mode,
            squelch: SquelchLevel::OPEN,
            step: None,
            output: UsbAudioOutput::IntermediateFrequency,
        }
    }

    /// Also change the Band B tuning step for this session.
    ///
    /// The original step is always saved and restored, including when this
    /// option is not used. Without this option, entry leaves the live step
    /// untouched.
    #[must_use]
    pub const fn with_step(mut self, step: StepSize) -> Self {
        self.step = Some(step);
        self
    }

    /// Use a squelch level other than fully open.
    #[must_use]
    pub const fn with_squelch(mut self, squelch: SquelchLevel) -> Self {
        self.squelch = squelch;
        self
    }

    /// Engage the demodulated Detect output instead of the IF output.
    #[must_use]
    pub const fn with_detect_output(mut self) -> Self {
        self.output = UsbAudioOutput::Detect;
        self
    }

    /// The USB audio output this configuration engages.
    #[must_use]
    pub const fn output(self) -> UsbAudioOutput {
        self.output
    }
}

/// Snapshot of the radio settings [`Radio::enter_if_tap`] touches.
///
/// Restored by [`IfTapSession::exit`] or [`Radio::restore_if_tap`]. The
/// original tuning step is always captured, even when entry leaves it
/// untouched, because callers can mutate the radio through a live session and
/// frequency restoration must still use the original tuning raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IfTapSavedState {
    active_band: Band,
    band_mode: BandMode,
    output: UsbAudioOutput,
    squelch: SquelchLevel,
    operating_mode: OperatingMode,
    band_b_frequency: Frequency,
    step: StepSize,
}

impl IfTapSavedState {
    /// Band B frequency captured before the IF tap was configured.
    #[must_use]
    pub const fn band_b_frequency(self) -> Frequency {
        self.band_b_frequency
    }
}

/// One step of the ordered IF-tap restore sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfTapRestoreStep {
    /// Forcing the USB audio output to the normal audio path first, so the
    /// remaining writes are not rejected by an engaged tap.
    ForceAudioOutput,
    /// Restoring the saved Band B tuning step before frequency restoration,
    /// so the saved frequency is evaluated on its original tuning raster.
    StepSize,
    /// Restoring and read-back-verifying the saved Band B frequency through
    /// the qualified UP/DW step walk.
    Frequency,
    /// Restoring the saved Band B squelch level.
    Squelch,
    /// Restoring the saved Band B operating mode.
    OperatingMode,
    /// Restoring the saved single/dual band selection.
    BandMode,
    /// Restoring the saved active band.
    ActiveBand,
    /// Re-applying the saved USB audio output selection last.
    SavedOutput,
}

impl std::fmt::Display for IfTapRestoreStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ForceAudioOutput => "USB audio output to the audio path",
            Self::StepSize => "Band B tuning step",
            Self::Frequency => "Band B frequency",
            Self::Squelch => "Band B squelch level",
            Self::OperatingMode => "Band B operating mode",
            Self::BandMode => "single/dual band selection",
            Self::ActiveBand => "active band",
            Self::SavedOutput => "saved USB audio output selection",
        })
    }
}

/// Per-step outcome of an IF-tap restore.
///
/// Restoration is best effort: a failed step is recorded and the remaining
/// steps still run, except after link loss or an ambiguous CAT boundary,
/// where the remaining steps are reported as not attempted instead of being
/// issued against an unsafe stream.
#[derive(Debug)]
#[must_use = "a restore report may carry failed or skipped steps"]
pub struct IfTapRestoreReport {
    failures: Vec<(IfTapRestoreStep, Error)>,
    not_attempted: Vec<IfTapRestoreStep>,
}

impl IfTapRestoreReport {
    const fn empty() -> Self {
        Self {
            failures: Vec::new(),
            not_attempted: Vec::new(),
        }
    }

    /// Whether every restore step completed.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.not_attempted.is_empty()
    }

    /// Steps that ran and failed, with their errors.
    #[must_use]
    pub fn failures(&self) -> &[(IfTapRestoreStep, Error)] {
        &self.failures
    }

    /// Steps skipped after link loss or an ambiguous CAT boundary.
    #[must_use]
    pub fn not_attempted(&self) -> &[IfTapRestoreStep] {
        &self.not_attempted
    }
}

/// [`Radio::enter_if_tap`] failed; already-applied settings were rolled back.
#[derive(Debug, thiserror::Error)]
#[error("entering the IF tap failed: {source}")]
pub struct IfTapEnterError {
    /// The failure that stopped entry.
    #[source]
    pub source: Box<Error>,
    /// Outcome of rolling back the settings that had already been applied.
    pub rollback: IfTapRestoreReport,
    /// The snapshot taken before configuration, retained only when the
    /// rollback did not complete, so the caller can retry
    /// [`Radio::restore_if_tap`] later. `None` means nothing was left
    /// changed on the radio.
    pub snapshot: Option<IfTapSavedState>,
}

/// A live IF-tap session over a borrowed [`Radio`].
///
/// Created by [`Radio::enter_if_tap`]. Dropping the session without calling
/// [`exit`](Self::exit) performs no restore (an async restore cannot run in
/// `Drop`); detach with [`into_saved_state`](Self::into_saved_state) if the
/// borrow cannot live long enough and restore later with
/// [`Radio::restore_if_tap`].
#[derive(Debug)]
pub struct IfTapSession<'radio, T: Transport> {
    radio: &'radio mut Radio<T>,
    saved: IfTapSavedState,
    config: IfTapConfig,
}

impl<T: Transport> IfTapSession<'_, T> {
    /// The snapshot that [`exit`](Self::exit) will restore.
    #[must_use]
    pub const fn saved_state(&self) -> &IfTapSavedState {
        &self.saved
    }

    /// Access the underlying radio during capture.
    ///
    /// Writes that change the saved settings will not be re-captured; the
    /// restore still re-applies the original snapshot, including the original
    /// tuning step and frequency raster.
    pub const fn radio(&mut self) -> &mut Radio<T> {
        self.radio
    }

    /// Detach the snapshot without restoring, for callers that cannot keep
    /// this borrow alive. Restore later with [`Radio::restore_if_tap`].
    #[must_use]
    pub const fn into_saved_state(self) -> IfTapSavedState {
        self.saved
    }

    /// Retune Band B to `target` using the qualified UP/DW stepping commands.
    ///
    /// Frequency writes are rejected while the tap is engaged, so this drops
    /// the USB audio output to the audio path, steps to the target, verifies
    /// the result by reading the frequency back, and re-engages the
    /// configured output (proving engagement by readback).
    ///
    /// Returns the verified frequency.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RetuneOffStep`] when `target` is not a whole number
    /// of current tuning steps from the current frequency or saved tuning
    /// steps from the saved frequency, and
    /// [`Error::RetuneSpanTooLarge`] when it is more than
    /// [`MAX_RETUNE_STEPS`] steps from either one. Bounding every target
    /// against the saved frequency ensures that a sequence of individually
    /// short retunes cannot leave the eventual restore beyond the same
    /// verified-walk limit. Neither preflight failure changes radio state.
    /// Returns [`Error::RetuneNotVerified`] when the stepped result reads
    /// back different from `target`, and [`Error::IfTapNotEngaged`] when
    /// re-engaging the tap does not prove out. After any error past the
    /// preflight checks the USB audio output may be left on the audio path;
    /// the session remains valid, and [`exit`](Self::exit) still restores
    /// every saved setting.
    pub async fn step_to_frequency(&mut self, target: Frequency) -> Result<Frequency, Error> {
        self.radio
            .retune_if_tap(&self.saved, target, self.config.output())
            .await
    }

    /// Restore the saved settings in the hardware-required order and give the
    /// radio borrow back.
    pub async fn exit(self) -> IfTapRestoreReport {
        self.radio.restore_if_tap(self.saved).await
    }
}

/// Write the requested output selection and prove it engaged by readback.
async fn engage_output<T: Transport>(
    radio: &mut Radio<T>,
    output: UsbAudioOutput,
) -> Result<(), Error> {
    radio.set_usb_audio_output(output).await?;
    let actual = radio.get_usb_audio_output().await?;
    if actual == output {
        Ok(())
    } else {
        Err(Error::IfTapNotEngaged {
            requested: output,
            actual,
        })
    }
}

fn verify_if_tap_value<Value: std::fmt::Debug + PartialEq>(
    setting: &'static str,
    requested: &Value,
    actual: &Value,
) -> Result<(), Error> {
    if actual == requested {
        Ok(())
    } else {
        Err(Error::Protocol(ProtocolError::UnexpectedResponse {
            expected: format!("IF-tap {setting} readback {requested:?}"),
            actual: format!("{actual:?}").into_bytes(),
        }))
    }
}

impl<T: Transport> Radio<T> {
    /// Enter the IF tap: guard preconditions, snapshot the touched settings,
    /// apply `config`, and prove the output engaged.
    ///
    /// Band B must be in VFO tuning mode so the selected memory, call, or
    /// weather channel is never changed. The applied sequence is: active
    /// band to B, Single Band mode, tuning step (only when configured),
    /// operating mode, squelch, then the output selection. The original Band
    /// B step is always part of the snapshot even when entry does not change
    /// it. Every setter is followed by its independent getter and an exact
    /// comparison (IF and Detect output require Single Band mode on Band B).
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use kenwood_thd75::radio::Radio;
    /// # use kenwood_thd75::transport::SerialTransport;
    /// # use kenwood_thd75::types::{Frequency, OperatingMode};
    /// # use kenwood_thd75::IfTapConfig;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
    /// let mut radio = Radio::new(transport);
    ///
    /// let mut session = radio
    ///     .enter_if_tap(IfTapConfig::new(OperatingMode::Usb))
    ///     .await?;
    /// session
    ///     .step_to_frequency(Frequency::from_mhz_str("145.240")?)
    ///     .await?;
    /// // ... capture the 12 kHz IF from the "ADC stream IN" device ...
    /// let report = session.exit().await;
    /// assert!(report.is_complete());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`IfTapEnterError`] wrapping the underlying failure. When
    /// configuration had already changed settings, they are rolled back
    /// first and the rollback outcome is reported alongside the error.
    pub async fn enter_if_tap(
        &mut self,
        config: IfTapConfig,
    ) -> Result<IfTapSession<'_, T>, IfTapEnterError> {
        let preflight = |source: Error| IfTapEnterError {
            source: Box::new(source),
            rollback: IfTapRestoreReport::empty(),
            snapshot: None,
        };

        let tuning_mode = self.get_tuning_mode(Band::B).await.map_err(&preflight)?;
        if tuning_mode != TuningMode::Vfo {
            return Err(preflight(Error::VfoTuningRequired {
                band: Band::B,
                current: tuning_mode,
            }));
        }

        let saved = IfTapSavedState {
            active_band: self.get_band().await.map_err(&preflight)?,
            band_mode: self.get_band_mode().await.map_err(&preflight)?,
            output: self.get_usb_audio_output().await.map_err(&preflight)?,
            squelch: self.get_squelch(Band::B).await.map_err(&preflight)?,
            operating_mode: self.get_operating_mode(Band::B).await.map_err(&preflight)?,
            band_b_frequency: self.get_frequency(Band::B).await.map_err(&preflight)?,
            step: self.get_step_size(Band::B).await.map_err(&preflight)?,
        };

        if let Err(source) = self.configure_if_tap(config).await {
            let rollback = self.restore_if_tap(saved).await;
            let snapshot = if rollback.is_complete() {
                None
            } else {
                Some(saved)
            };
            return Err(IfTapEnterError {
                source: Box::new(source),
                rollback,
                snapshot,
            });
        }

        Ok(IfTapSession {
            radio: self,
            saved,
            config,
        })
    }

    /// Apply the IF-tap configuration; the caller owns rollback on failure.
    async fn configure_if_tap(&mut self, config: IfTapConfig) -> Result<(), Error> {
        self.write_and_verify_if_tap_band(Band::B).await?;
        self.write_and_verify_if_tap_band_mode(BandMode::Single)
            .await?;
        if let Some(step) = config.step {
            self.write_and_verify_if_tap_step(step).await?;
        }
        self.write_and_verify_if_tap_operating_mode(config.operating_mode)
            .await?;
        self.write_and_verify_if_tap_squelch(config.squelch).await?;
        engage_output(self, config.output()).await
    }

    /// Retune Band B under an engaged IF tap using the qualified UP/DW
    /// stepping commands.
    ///
    /// This is the detached-snapshot counterpart of
    /// [`IfTapSession::step_to_frequency`], for callers that hold an
    /// [`IfTapSavedState`] instead of a borrowed session. The matching
    /// `saved` snapshot is required so every target can be proved close
    /// enough to restore. It drops the USB audio output to the audio path
    /// (frequency stepping is rejected while the tap is engaged), walks to
    /// `target` one step at a time, and re-engages `engaged_output` with a
    /// readback proof.
    ///
    /// Every step is individually verified by a frequency readback before
    /// the next step is sent. Hardware-observed (live radio, 2026-08-09):
    /// the radio acknowledges rapid consecutive step commands but can
    /// swallow all except the last, so a fire-and-forget burst lands short.
    /// The verified walk retries a swallowed step and fails closed after
    /// [`MAX_RETUNE_STALLS`] consecutive steps that produce no movement.
    ///
    /// Returns the verified frequency.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RetuneOffStep`] when `target` is not a whole number
    /// of tuning steps from either the current or saved frequency, and
    /// [`Error::RetuneSpanTooLarge`] when it is more than
    /// [`MAX_RETUNE_STEPS`] steps from either one; neither changes any radio
    /// state. Checking the saved frequency prevents cumulative retunes from
    /// exceeding the restore walk's bound.
    /// Returns [`Error::RetuneNotVerified`] when the walk cannot make
    /// verified progress, and [`Error::IfTapNotEngaged`] when re-engaging the
    /// tap does not prove out. After any error past the preflight checks the
    /// USB audio output may be left on the audio path.
    pub async fn retune_if_tap(
        &mut self,
        saved: &IfTapSavedState,
        target: Frequency,
        engaged_output: UsbAudioOutput,
    ) -> Result<Frequency, Error> {
        let current = self.get_frequency(Band::B).await?;
        let step = self.get_step_size(Band::B).await?;
        preflight_walk(current, target, step)?;
        preflight_walk(saved.band_b_frequency, target, saved.step)?;
        if current == target {
            engage_output(self, engaged_output).await?;
            return Ok(current);
        }

        // Frequency stepping is rejected while the tap is engaged; drop to
        // the audio path first.
        engage_output(self, UsbAudioOutput::Audio).await?;
        let landed = self
            .verified_step_walk(Band::B, current, target, step)
            .await?;
        engage_output(self, engaged_output).await?;
        Ok(landed)
    }

    /// Restore an IF-tap snapshot in the hardware-required order.
    ///
    /// The output selection is forced to the audio path first (frequency and
    /// band writes are rejected while the tap is engaged), the saved tuning
    /// step is restored before the saved Band B frequency is restored and
    /// verified through the qualified UP/DW walk,
    /// the saved output selection is re-applied last, and every saved setting
    /// in between is written and independently read back. Restoration is best
    /// effort: a failed setter, getter, or comparison is recorded and the
    /// remaining steps still run, unless the failure reports link loss or
    /// leaves the CAT boundary ambiguous, in which case the remaining steps
    /// are reported as not attempted.
    pub async fn restore_if_tap(&mut self, saved: IfTapSavedState) -> IfTapRestoreReport {
        let mut report = IfTapRestoreReport::empty();
        let mut steps = vec![
            IfTapRestoreStep::ForceAudioOutput,
            IfTapRestoreStep::StepSize,
        ];
        steps.push(IfTapRestoreStep::Frequency);
        steps.push(IfTapRestoreStep::Squelch);
        steps.push(IfTapRestoreStep::OperatingMode);
        steps.push(IfTapRestoreStep::BandMode);
        steps.push(IfTapRestoreStep::ActiveBand);
        if saved.output != UsbAudioOutput::Audio {
            steps.push(IfTapRestoreStep::SavedOutput);
        }

        let mut remaining = steps.into_iter();
        for step in remaining.by_ref() {
            let outcome = match step {
                IfTapRestoreStep::ForceAudioOutput => {
                    engage_output(self, UsbAudioOutput::Audio).await
                }
                IfTapRestoreStep::StepSize => self.write_and_verify_if_tap_step(saved.step).await,
                IfTapRestoreStep::Frequency => {
                    self.restore_if_tap_frequency(saved.band_b_frequency).await
                }
                IfTapRestoreStep::Squelch => {
                    self.write_and_verify_if_tap_squelch(saved.squelch).await
                }
                IfTapRestoreStep::OperatingMode => {
                    self.write_and_verify_if_tap_operating_mode(saved.operating_mode)
                        .await
                }
                IfTapRestoreStep::BandMode => {
                    self.write_and_verify_if_tap_band_mode(saved.band_mode)
                        .await
                }
                IfTapRestoreStep::ActiveBand => {
                    self.write_and_verify_if_tap_band(saved.active_band).await
                }
                IfTapRestoreStep::SavedOutput => engage_output(self, saved.output).await,
            };
            if let Err(error) = outcome {
                let recovery_required = error.requires_recovery() || self.cat_recovery_required();
                report.failures.push((step, error));
                if recovery_required {
                    report.not_attempted.extend(remaining);
                    break;
                }
            }
        }
        report
    }

    /// Restore Band B through the currently configured step while the IF
    /// output is on the normal audio path.
    async fn restore_if_tap_frequency(&mut self, target: Frequency) -> Result<(), Error> {
        let current = self.get_frequency(Band::B).await?;
        if current == target {
            return Ok(());
        }
        // UP/DW always acts on the active band. A detached caller can change
        // that selection while the tap is active, so prove Band B again before
        // issuing the first step. The final ActiveBand restore below still
        // returns the operator to the original selection.
        if self.get_band().await? != Band::B {
            self.write_and_verify_if_tap_band(Band::B).await?;
        }
        let step = self.get_step_size(Band::B).await?;
        preflight_walk(current, target, step)?;
        let _landed = self
            .verified_step_walk(Band::B, current, target, step)
            .await?;
        Ok(())
    }

    async fn write_and_verify_if_tap_band(&mut self, requested: Band) -> Result<(), Error> {
        self.set_band(requested).await?;
        let actual = self.get_band().await?;
        verify_if_tap_value("active band", &requested, &actual)
    }

    async fn write_and_verify_if_tap_band_mode(
        &mut self,
        requested: BandMode,
    ) -> Result<(), Error> {
        self.set_band_mode(requested).await?;
        let actual = self.get_band_mode().await?;
        verify_if_tap_value("band mode", &requested, &actual)
    }

    async fn write_and_verify_if_tap_step(&mut self, requested: StepSize) -> Result<(), Error> {
        self.set_step_size(Band::B, requested).await?;
        let actual = self.get_step_size(Band::B).await?;
        verify_if_tap_value("Band B tuning step", &requested, &actual)
    }

    async fn write_and_verify_if_tap_operating_mode(
        &mut self,
        requested: OperatingMode,
    ) -> Result<(), Error> {
        self.set_operating_mode(Band::B, requested).await?;
        let actual = self.get_operating_mode(Band::B).await?;
        verify_if_tap_value("Band B operating mode", &requested, &actual)
    }

    async fn write_and_verify_if_tap_squelch(
        &mut self,
        requested: SquelchLevel,
    ) -> Result<(), Error> {
        self.set_squelch(Band::B, requested).await?;
        let actual = self.get_squelch(Band::B).await?;
        verify_if_tap_value("Band B squelch", &requested, &actual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Queue the full happy-path enter script: VFO guard, seven saves (step
    /// included), configure writes, and the engagement proof.
    fn queue_enter_script(mock: &mut MockTransport) {
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"DL\r", b"DL 1\r");
        mock.expect(b"SF 1,0\r", b"SF 1,0\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"MD 1,4\r", b"MD 1,4\r");
        mock.expect(b"MD 1\r", b"MD 1,4\r");
        mock.expect(b"SQ 1,0\r", b"SQ 1,0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,0\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
    }

    const fn usb_if_config() -> IfTapConfig {
        IfTapConfig::new(OperatingMode::Usb).with_step(StepSize::Hz5000)
    }

    fn detached_saved_state(frequency: Frequency) -> Result<IfTapSavedState, Error> {
        detached_saved_state_with_step(frequency, StepSize::Hz5000)
    }

    fn detached_saved_state_with_step(
        frequency: Frequency,
        step: StepSize,
    ) -> Result<IfTapSavedState, Error> {
        Ok(IfTapSavedState {
            active_band: Band::A,
            band_mode: BandMode::Dual,
            output: UsbAudioOutput::Audio,
            squelch: SquelchLevel::new(2)?,
            operating_mode: OperatingMode::Fm,
            band_b_frequency: frequency,
            step,
        })
    }

    #[tokio::test]
    async fn enter_saves_configures_and_proves_engagement() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        let mut radio = Radio::new(mock);

        let session = radio.enter_if_tap(usb_if_config()).await?;
        assert_eq!(
            session.saved_state(),
            &IfTapSavedState {
                active_band: Band::A,
                band_mode: BandMode::Dual,
                output: UsbAudioOutput::Audio,
                squelch: SquelchLevel::new(2)?,
                operating_mode: OperatingMode::Fm,
                band_b_frequency: Frequency::new(145_000_000),
                step: StepSize::Hz12500,
            }
        );
        let saved = session.into_saved_state();
        assert_eq!(saved.band_b_frequency().as_hz(), 145_000_000);
        assert_eq!(saved.step, StepSize::Hz12500);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn default_config_restores_step_mutated_through_the_live_session() -> TestResult {
        let mut mock = MockTransport::new();
        // Snapshot includes the original 12.5 kHz step, but default entry
        // deliberately sends no SF setter before configuring mode/squelch.
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"DL\r", b"DL 1\r");
        mock.expect(b"MD 1,4\r", b"MD 1,4\r");
        mock.expect(b"MD 1\r", b"MD 1,4\r");
        mock.expect(b"SQ 1,0\r", b"SQ 1,0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,0\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");

        // A caller is allowed to mutate the borrowed radio. Retuning uses the
        // new 5 kHz live raster while also bounding the target against the
        // saved 12.5 kHz restore raster.
        mock.expect(b"SF 1,0\r", b"SF 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        for frequency in [
            145_005_000,
            145_010_000,
            145_015_000,
            145_020_000,
            145_025_000,
        ] {
            mock.expect(b"UP\r", b"UP\r");
            mock.expect(b"FQ 1\r", format!("FQ 1,{frequency:010}\r").as_bytes());
        }
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");

        // Exit restores 12.5 kHz before walking back to the original
        // frequency, then restores every remaining setting.
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145012500\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");

        let mut radio = Radio::new(mock);
        let mut session = radio
            .enter_if_tap(IfTapConfig::new(OperatingMode::Usb))
            .await?;
        assert_eq!(session.saved_state().step, StepSize::Hz12500);

        session
            .radio()
            .set_step_size(Band::B, StepSize::Hz5000)
            .await?;
        let landed = session
            .step_to_frequency(Frequency::new(145_025_000))
            .await?;
        assert_eq!(landed.as_hz(), 145_025_000);

        let report = session.exit().await;
        assert!(
            report.is_complete(),
            "default entry must restore a caller-mutated step and frequency: {report:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn enter_refuses_non_vfo_tuning_before_any_mutation() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"VM 1\r", b"VM 1,1\r");
        let mut radio = Radio::new(mock);

        let result = radio.enter_if_tap(usb_if_config()).await;
        let Err(error) = result else {
            return Err("memory tuning mode must refuse the IF tap".into());
        };
        assert!(
            matches!(
                *error.source,
                Error::VfoTuningRequired {
                    band: Band::B,
                    current: TuningMode::Memory
                }
            ),
            "unexpected enter error: {error:?}"
        );
        assert!(
            error.rollback.is_complete(),
            "nothing was mutated, so the rollback must be trivially complete"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn enter_rolls_back_already_applied_settings_on_failure() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        // Single Band selection is rejected; the rollback must restore the
        // full snapshot in the documented order.
        mock.expect(b"DL 1\r", b"?\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let result = radio.enter_if_tap(usb_if_config()).await;
        let Err(error) = result else {
            return Err("a rejected configure write must fail enter".into());
        };
        assert!(
            matches!(*error.source, Error::CommandRejected { .. }),
            "unexpected enter error: {error:?}"
        );
        assert!(
            error.rollback.is_complete(),
            "rollback must restore every saved setting: {error:?}"
        );
        assert!(
            error.snapshot.is_none(),
            "a complete rollback must not retain the snapshot: {error:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn enter_rolls_back_when_setter_echoes_but_readback_disagrees() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");

        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 0\r");

        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let result = radio.enter_if_tap(usb_if_config()).await;
        let Err(error) = result else {
            return Err("an unconfirmed active-band write must fail enter".into());
        };
        assert!(
            matches!(
                *error.source,
                Error::Protocol(ProtocolError::UnexpectedResponse { .. })
            ),
            "an echoed setter must not substitute for independent readback: {error:?}"
        );
        assert!(
            error.rollback.is_complete(),
            "the independently detected mismatch must still roll back cleanly: {error:?}"
        );
        assert!(
            error.snapshot.is_none(),
            "complete rollback must consume the retained snapshot"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn incomplete_rollback_retains_the_snapshot_for_retry() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        // Configure fails at Single Band selection; during rollback the
        // squelch restore is also rejected, leaving the rollback incomplete.
        mock.expect(b"DL 1\r", b"?\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"?\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let result = radio.enter_if_tap(usb_if_config()).await;
        let Err(error) = result else {
            return Err("a rejected configure write must fail enter".into());
        };
        assert!(!error.rollback.is_complete());
        let snapshot = error
            .snapshot
            .ok_or("an incomplete rollback must retain the snapshot for retry")?;
        assert_eq!(snapshot.step, StepSize::Hz12500);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn exit_restores_in_the_hardware_required_order() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        // Restore order: audio path first, step, verified frequency, squelch,
        // mode, band mode, active band; the saved output was Audio, so no
        // final IO write.
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let session = radio.enter_if_tap(usb_if_config()).await?;
        let report = session.exit().await;
        assert!(report.is_complete(), "restore must complete: {report:?}");
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn restore_continues_past_a_rejected_step() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"?\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let session = radio.enter_if_tap(usb_if_config()).await?;
        let report = session.exit().await;
        assert!(!report.is_complete());
        assert!(
            matches!(
                report.failures(),
                [(IfTapRestoreStep::Squelch, Error::CommandRejected { .. })]
            ),
            "exactly the squelch step must fail: {report:?}"
        );
        assert!(
            report.not_attempted().is_empty(),
            "a semantic rejection must not abort the remaining steps: {report:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn restore_reports_echoed_write_when_independent_readback_disagrees() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let session = radio.enter_if_tap(usb_if_config()).await?;
        let report = session.exit().await;

        assert!(
            matches!(
                report.failures(),
                [(
                    IfTapRestoreStep::StepSize,
                    Error::Protocol(ProtocolError::UnexpectedResponse { .. })
                )]
            ),
            "an echoed but unapplied step write must be reported exactly: {report:?}"
        );
        assert!(
            report.not_attempted().is_empty(),
            "a semantic readback mismatch must not skip later restore steps: {report:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_to_frequency_drops_the_tap_steps_verifies_and_re_engages() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145010000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145015000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145020000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);

        let mut session = radio.enter_if_tap(usb_if_config()).await?;
        let landed = session
            .step_to_frequency(Frequency::new(145_025_000))
            .await?;
        assert_eq!(landed.as_hz(), 145_025_000);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_to_frequency_retries_a_swallowed_step() -> TestResult {
        // Hardware-observed: the radio can acknowledge a step command
        // without moving the VFO. The walk must notice the stall via the
        // per-step readback and try again.
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145010000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145015000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145020000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);

        let mut session = radio.enter_if_tap(usb_if_config()).await?;
        let landed = session
            .step_to_frequency(Frequency::new(145_025_000))
            .await?;
        assert_eq!(landed.as_hz(), 145_025_000);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_to_frequency_fails_closed_after_persistent_stalls() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        for _ in 0..MAX_RETUNE_STALLS {
            mock.expect(b"UP\r", b"UP\r");
            mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        }
        let mut radio = Radio::new(mock);

        let mut session = radio.enter_if_tap(usb_if_config()).await?;
        let result = session.step_to_frequency(Frequency::new(145_025_000)).await;
        assert!(
            matches!(
                result,
                Err(Error::RetuneNotVerified { actual, .. }) if actual.as_hz() == 145_000_000
            ),
            "a persistently stalled walk must fail closed: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn retune_if_tap_works_with_a_detached_snapshot() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0145010000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state(Frequency::new(145_000_000))?;

        let landed = radio
            .retune_if_tap(
                &saved,
                Frequency::new(145_005_000),
                UsbAudioOutput::IntermediateFrequency,
            )
            .await?;
        assert_eq!(landed.as_hz(), 145_005_000);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn same_frequency_retune_still_proves_if_output_engagement() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 0\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state(Frequency::new(145_000_000))?;

        let result = radio
            .retune_if_tap(
                &saved,
                Frequency::new(145_005_000),
                UsbAudioOutput::IntermediateFrequency,
            )
            .await;

        assert!(
            matches!(
                result,
                Err(Error::IfTapNotEngaged {
                    requested: UsbAudioOutput::IntermediateFrequency,
                    actual: UsbAudioOutput::Audio,
                })
            ),
            "same-frequency retunes must fail closed when IF readback disagrees: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn retune_rejects_a_target_that_cannot_be_restored_on_the_saved_raster() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state_with_step(Frequency::new(145_000_000), StepSize::Hz12500)?;

        let result = radio
            .retune_if_tap(
                &saved,
                Frequency::new(145_005_000),
                UsbAudioOutput::IntermediateFrequency,
            )
            .await;

        assert!(
            matches!(
                result,
                Err(Error::RetuneOffStep {
                    step: StepSize::Hz12500,
                    ..
                })
            ),
            "a target off the restore raster must be rejected before output changes: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn restore_applies_the_saved_step_before_walking_the_saved_frequency() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145012500\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state_with_step(Frequency::new(145_000_000), StepSize::Hz12500)?;

        let report = radio.restore_if_tap(saved).await;

        assert!(
            report.is_complete(),
            "frequency restore on the saved raster must complete: {report:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn restore_reselects_band_b_before_frequency_steps() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145012500\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state_with_step(Frequency::new(145_000_000), StepSize::Hz12500)?;

        let report = radio.restore_if_tap(saved).await;

        assert!(
            report.is_complete(),
            "restore must target Band B even when another band was active: {report:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn restore_stops_after_a_malformed_response_poisons_cat() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO invalid\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state(Frequency::new(145_000_000))?;

        let report = radio.restore_if_tap(saved).await;

        assert!(
            matches!(
                report.failures(),
                [(IfTapRestoreStep::ForceAudioOutput, Error::Protocol(_))]
            ),
            "the malformed matching response must be the sole attempted failure: {report:?}"
        );
        assert_eq!(
            report.not_attempted(),
            &[
                IfTapRestoreStep::StepSize,
                IfTapRestoreStep::Frequency,
                IfTapRestoreStep::Squelch,
                IfTapRestoreStep::OperatingMode,
                IfTapRestoreStep::BandMode,
                IfTapRestoreStep::ActiveBand,
            ],
            "no command may follow an ambiguous CAT boundary"
        );
        assert!(radio.cat_recovery_required());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn detached_repeated_retunes_cannot_outgrow_restore_bound() -> TestResult {
        let mut mock = MockTransport::new();

        // First move one step from the saved frequency.
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");

        // The next target is exactly 1,000 steps from the current frequency,
        // but 1,001 from the saved frequency. It must be rejected before the
        // output is changed, or a later restore could not use the bounded
        // verified walk.
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        let mut radio = Radio::new(mock);
        let saved = detached_saved_state(Frequency::new(145_000_000))?;

        let first = radio
            .retune_if_tap(
                &saved,
                Frequency::new(145_005_000),
                UsbAudioOutput::IntermediateFrequency,
            )
            .await?;
        assert_eq!(first.as_hz(), 145_005_000);

        let result = radio
            .retune_if_tap(
                &saved,
                Frequency::new(150_005_000),
                UsbAudioOutput::IntermediateFrequency,
            )
            .await;
        assert!(
            matches!(
                result,
                Err(Error::RetuneSpanTooLarge {
                    steps_required: 1_001,
                    maximum: MAX_RETUNE_STEPS,
                })
            ),
            "a short second walk must not strand restore beyond its bound: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn retuned_session_restores_original_frequency_with_verified_steps() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);

        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145005000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145010000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145015000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145020000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");

        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145025000\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145012500\r");
        mock.expect(b"DW\r", b"DW\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let mut session = radio.enter_if_tap(usb_if_config()).await?;
        let landed = session
            .step_to_frequency(Frequency::new(145_025_000))
            .await?;
        assert_eq!(landed.as_hz(), 145_025_000);

        let report = session.exit().await;
        assert!(
            report.is_complete(),
            "the original frequency and settings must all restore: {report:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_to_frequency_rejects_off_step_targets_before_any_write() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        let mut radio = Radio::new(mock);

        let mut session = radio.enter_if_tap(usb_if_config()).await?;
        let result = session.step_to_frequency(Frequency::new(145_012_300)).await;
        assert!(
            matches!(
                result,
                Err(Error::RetuneOffStep {
                    step: StepSize::Hz5000,
                    ..
                })
            ),
            "an off-step target must be refused: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn step_to_frequency_bounds_the_walk() -> TestResult {
        let mut mock = MockTransport::new();
        queue_enter_script(&mut mock);
        mock.expect(b"FQ 1\r", b"FQ 1,0145000000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        let mut radio = Radio::new(mock);

        let mut session = radio.enter_if_tap(usb_if_config()).await?;
        let result = session.step_to_frequency(Frequency::new(245_000_000)).await;
        assert!(
            matches!(
                result,
                Err(Error::RetuneSpanTooLarge {
                    steps_required: 20_000,
                    maximum: MAX_RETUNE_STEPS,
                })
            ),
            "a 20,000-step walk must be refused: {result:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }
}
