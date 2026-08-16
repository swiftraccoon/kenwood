//! Radio-state lifecycle adapter for live TH-D75 USB IF capture.
//!
//! The lifecycle itself is library-owned: `Radio::enter_if_tap` guards
//! Band-B VFO mode, snapshots every touched setting (including the tuning
//! step), configures single-band B / USB / squelch-open / 5 kHz step,
//! proves IF engagement by readback, and rolls back on a mid-configure
//! failure; `Radio::restore_if_tap` restores in the hardware-required
//! order; `Radio::retune_if_tap` steps with the verified UP/DW walk. This
//! module adapts those calls to the actor's stored-state shape and the
//! Swift-facing report strings.

use kenwood_thd75::radio::if_tap::IfTapRestoreReport;
use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::{Band, Frequency, OperatingMode, StepSize, UsbAudioOutput};
use kenwood_thd75::{IfTapConfig, IfTapSavedState, Radio};

/// Fixed center of the TH-D75 real low-IF USB stream.
pub(crate) const IF_CENTER_HZ: u32 = 12_000;

/// Complete snapshot of every radio value changed by IF capture, plus the
/// Band-B frequency observed before engagement.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedIfDspRadioState {
    if_tap: IfTapSavedState,
    band_b_frequency: Frequency,
}

impl SavedIfDspRadioState {
    /// Band-B frequency which was active before IF capture began.
    pub(crate) const fn band_b_frequency_hz(self) -> u32 {
        self.band_b_frequency.as_hz()
    }
}

/// Best-effort restoration result. An empty list is the only exact success.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct IfDspRestoreReport {
    pub(crate) failed_steps: Vec<String>,
}

impl IfDspRestoreReport {
    pub(crate) fn is_exact(&self) -> bool {
        self.failed_steps.is_empty()
    }

    pub(crate) fn summary(&self) -> String {
        if self.is_exact() {
            "all saved radio values were restored and verified".to_owned()
        } else {
            format!(
                "could not restore and verify {}",
                self.failed_steps.join(", ")
            )
        }
    }
}

/// How an IF-tap engagement failed.
#[derive(Debug)]
pub(crate) enum EngageIfDspError {
    /// Nothing was left changed on the radio (preflight refusal, or the
    /// library rolled every applied setting back).
    Clean(String),
    /// The rollback did not complete; the retained snapshot allows a later
    /// restore retry against the still-dirty radio.
    Dirty {
        detail: String,
        saved: SavedIfDspRadioState,
    },
}

/// Engage the 12 kHz USB IF output through the library session.
///
/// Snapshot, configuration, engagement proof, and failure rollback are one
/// atomic library operation; this adapter additionally records the Band-B
/// frequency observed before engagement for status reporting.
pub(crate) async fn engage_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
) -> Result<SavedIfDspRadioState, EngageIfDspError> {
    let band_b_frequency = radio
        .get_frequency(Band::B)
        .await
        .map_err(|error| EngageIfDspError::Clean(format!("reading Band-B frequency: {error}")))?;
    let config = IfTapConfig::new(OperatingMode::Usb).with_step(StepSize::Hz5000);
    match radio.enter_if_tap(config).await {
        Ok(session) => Ok(SavedIfDspRadioState {
            if_tap: session.into_saved_state(),
            band_b_frequency,
        }),
        Err(error) => {
            let detail = if error.rollback.is_complete() {
                format!(
                    "engaging the 12 kHz USB IF output: {error}; the complete pre-session \
                     state was restored"
                )
            } else {
                format!(
                    "engaging the 12 kHz USB IF output: {error}; {}",
                    map_report(&error.rollback).summary()
                )
            };
            match error.snapshot {
                Some(if_tap) => Err(EngageIfDspError::Dirty {
                    detail,
                    saved: SavedIfDspRadioState {
                        if_tap,
                        band_b_frequency,
                    },
                }),
                None => Err(EngageIfDspError::Clean(detail)),
            }
        }
    }
}

/// Step Band B to `frequency_hz` with the library's verified UP/DW walk.
///
/// Each step is confirmed by a frequency readback (the radio can swallow
/// rapid consecutive steps), the tap drops to the audio path during the
/// walk, and IF output is re-engaged with a readback proof afterwards.
pub(crate) async fn retune_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
    frequency_hz: u32,
) -> Result<(), String> {
    radio
        .retune_if_tap(
            Frequency::new(frequency_hz),
            UsbAudioOutput::IntermediateFrequency,
        )
        .await
        .map(|_landed| ())
        .map_err(|error| format!("retuning Band B: {error}"))
}

/// Restore every saved value in the hardware-required order.
///
/// Frequency is deliberately not restored: tuning is user-directed through
/// [`retune_if_dsp_radio`].
pub(crate) async fn restore_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
    saved: SavedIfDspRadioState,
) -> IfDspRestoreReport {
    map_report(&radio.restore_if_tap(saved.if_tap).await)
}

/// Convert the library's typed restore report into Swift-facing strings.
fn map_report(report: &IfTapRestoreReport) -> IfDspRestoreReport {
    let mut failed_steps: Vec<String> = report
        .failures()
        .iter()
        .map(|(step, error)| format!("{step} ({error})"))
        .collect();
    failed_steps.extend(
        report
            .not_attempted()
            .iter()
            .map(|step| format!("{step} (not attempted after link loss)")),
    );
    IfDspRestoreReport { failed_steps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_thd75::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The library enter script for this adapter's configuration (USB mode,
    /// squelch open, 5 kHz step), preceded by the adapter's frequency read.
    fn queue_engage_script(mock: &mut MockTransport) {
        mock.expect(b"FQ 1\r", b"FQ 1,0145240000\r");
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"SF 1,0\r", b"SF 1,0\r");
        mock.expect(b"MD 1,4\r", b"MD 1,4\r");
        mock.expect(b"SQ 1,0\r", b"SQ 1,0\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
    }

    #[tokio::test]
    async fn engage_snapshots_and_records_the_prior_frequency() -> TestResult {
        let mut mock = MockTransport::new();
        queue_engage_script(&mut mock);
        let mut radio = Radio::new(mock);

        let saved = engage_if_dsp_radio(&mut radio)
            .await
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(saved.band_b_frequency_hz(), 145_240_000);
        Ok(())
    }

    #[tokio::test]
    async fn engage_reports_clean_when_the_library_rolls_back() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0145240000\r");
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"SF 1\r", b"SF 1,5\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"DL 1\r", b"?\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let result = engage_if_dsp_radio(&mut radio).await;
        assert!(
            matches!(
                &result,
                Err(EngageIfDspError::Clean(detail))
                    if detail.contains("the complete pre-session state was restored")
            ),
            "a fully rolled-back failure must be clean: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn restore_maps_the_typed_report_into_step_strings() -> TestResult {
        let mut mock = MockTransport::new();
        queue_engage_script(&mut mock);
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"SQ 1,2\r", b"?\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"SF 1,5\r", b"SF 1,5\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        let mut radio = Radio::new(mock);

        let saved = engage_if_dsp_radio(&mut radio)
            .await
            .map_err(|error| format!("{error:?}"))?;
        let report = restore_if_dsp_radio(&mut radio, saved).await;
        assert!(!report.is_exact());
        assert_eq!(report.failed_steps.len(), 1);
        assert!(
            report.summary().contains("Band B squelch level"),
            "the failed step must be named: {}",
            report.summary()
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_delegates_to_the_verified_walk() -> TestResult {
        let mut mock = MockTransport::new();
        queue_engage_script(&mut mock);
        mock.expect(b"FQ 1\r", b"FQ 1,0145240000\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0145245000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);

        let _saved = engage_if_dsp_radio(&mut radio)
            .await
            .map_err(|error| format!("{error:?}"))?;
        retune_if_dsp_radio(&mut radio, 145_245_000).await?;
        Ok(())
    }
}
