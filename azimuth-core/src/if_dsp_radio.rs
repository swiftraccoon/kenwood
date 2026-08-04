//! Radio-state lifecycle for live TH-D75 USB IF capture.

use kenwood_thd75::Radio;
use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::{
    Band, BandMode, Frequency, OperatingMode, SquelchLevel, StepSize, TuningMode, UsbAudioOutput,
};

/// Fixed center of the TH-D75 real low-IF USB stream.
pub(crate) const IF_CENTER_HZ: u32 = 12_000;

/// Complete snapshot of every radio value changed by IF capture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedIfDspRadioState {
    operation_band: Band,
    band_mode: BandMode,
    usb_audio_output: UsbAudioOutput,
    band_b_squelch: SquelchLevel,
    band_b_operating_mode: OperatingMode,
    band_b_step: StepSize,
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

/// Read every value before the first IF-mode mutation.
pub(crate) async fn save_if_dsp_radio_state<T: Transport>(
    radio: &mut Radio<T>,
) -> Result<SavedIfDspRadioState, String> {
    let operation_band = radio
        .get_band()
        .await
        .map_err(|error| format!("reading operation band: {error}"))?;
    let band_b_tuning_mode = radio
        .get_tuning_mode(Band::B)
        .await
        .map_err(|error| format!("reading Band-B tuning mode: {error}"))?;
    if band_b_tuning_mode != TuningMode::Vfo {
        return Err(format!(
            "Band B is in {band_b_tuning_mode} tuning mode; IF-DSP requires Band B to already be in VFO mode so its selected memory/call/weather channel is never changed"
        ));
    }
    let band_mode = radio
        .get_band_mode()
        .await
        .map_err(|error| format!("reading band presentation: {error}"))?;
    let usb_audio_output = radio
        .get_usb_audio_output()
        .await
        .map_err(|error| format!("reading USB output mode: {error}"))?;
    let band_b_squelch = radio
        .get_squelch(Band::B)
        .await
        .map_err(|error| format!("reading Band-B squelch: {error}"))?;
    let band_b_operating_mode = radio
        .get_operating_mode(Band::B)
        .await
        .map_err(|error| format!("reading Band-B demodulation mode: {error}"))?;
    let band_b_step = radio
        .get_step_size(Band::B)
        .await
        .map_err(|error| format!("reading Band-B tuning step: {error}"))?;
    let band_b_frequency = radio
        .get_frequency(Band::B)
        .await
        .map_err(|error| format!("reading Band-B frequency: {error}"))?;

    Ok(SavedIfDspRadioState {
        operation_band,
        band_mode,
        usb_audio_output,
        band_b_squelch,
        band_b_operating_mode,
        band_b_step,
        band_b_frequency,
    })
}

/// Configure and verify the radio's physical 12 kHz USB IF output.
pub(crate) async fn configure_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
) -> Result<(), String> {
    let band_b_tuning_mode = radio
        .get_tuning_mode(Band::B)
        .await
        .map_err(|error| format!("rechecking Band-B tuning mode: {error}"))?;
    if band_b_tuning_mode != TuningMode::Vfo {
        return Err(format!(
            "Band B changed to {band_b_tuning_mode} tuning mode before IF-DSP setup; no radio setting was changed"
        ));
    }

    radio
        .set_band(Band::B)
        .await
        .map_err(|error| format!("selecting operation Band B: {error}"))?;
    let operation_band = radio
        .get_band()
        .await
        .map_err(|error| format!("verifying operation Band B: {error}"))?;
    if operation_band != Band::B {
        return Err(format!(
            "operation-band readback was {operation_band}, not Band B"
        ));
    }

    radio
        .set_band_mode(BandMode::Single)
        .await
        .map_err(|error| format!("selecting Single Band mode: {error}"))?;
    let band_mode = radio
        .get_band_mode()
        .await
        .map_err(|error| format!("verifying Single Band mode: {error}"))?;
    if band_mode != BandMode::Single {
        return Err("the radio remained in Dual Band mode".to_owned());
    }

    radio
        .set_step_size(Band::B, StepSize::Hz5000)
        .await
        .map_err(|error| format!("setting the Band-B 5 kHz tuning step: {error}"))?;
    let step = radio
        .get_step_size(Band::B)
        .await
        .map_err(|error| format!("verifying the Band-B tuning step: {error}"))?;
    if step != StepSize::Hz5000 {
        return Err(format!("Band-B tuning-step readback was {step}, not 5 kHz"));
    }

    radio
        .set_operating_mode(Band::B, OperatingMode::Usb)
        .await
        .map_err(|error| format!("setting Band B to USB mode: {error}"))?;
    let operating_mode = radio
        .get_operating_mode(Band::B)
        .await
        .map_err(|error| format!("verifying Band-B USB mode: {error}"))?;
    if operating_mode != OperatingMode::Usb {
        return Err(format!(
            "Band-B operating-mode readback was {operating_mode}, not USB"
        ));
    }

    radio
        .set_squelch(Band::B, SquelchLevel::OPEN)
        .await
        .map_err(|error| format!("opening Band-B squelch: {error}"))?;
    let squelch = radio
        .get_squelch(Band::B)
        .await
        .map_err(|error| format!("verifying open Band-B squelch: {error}"))?;
    if squelch != SquelchLevel::OPEN {
        return Err(format!(
            "Band-B squelch readback was {}, not open",
            squelch.as_raw()
        ));
    }

    radio
        .set_usb_audio_output(UsbAudioOutput::IntermediateFrequency)
        .await
        .map_err(|error| format!("enabling 12 kHz USB IF output: {error}"))?;
    let usb_audio_output = radio
        .get_usb_audio_output()
        .await
        .map_err(|error| format!("verifying 12 kHz USB IF output: {error}"))?;
    if usb_audio_output != UsbAudioOutput::IntermediateFrequency {
        return Err(format!(
            "USB output readback was {usb_audio_output}, not Intermediate Frequency; that output requires Single Band mode on Band B"
        ));
    }
    Ok(())
}

/// Verify that Band B is already tuned and still supplying its USB IF output.
///
/// Arbitrary CAT frequency writes are not qualified. The operator must tune
/// the radio directly; this function only accepts exact frequency and IF-mode
/// readbacks.
pub(crate) async fn retune_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
    frequency_hz: u32,
) -> Result<(), String> {
    let current = radio
        .get_frequency(Band::B)
        .await
        .map_err(|error| format!("reading Band-B frequency: {error}"))?;
    if current.as_hz() == frequency_hz {
        let usb_audio_output = radio
            .get_usb_audio_output()
            .await
            .map_err(|error| format!("verifying 12 kHz USB IF output: {error}"))?;
        if usb_audio_output == UsbAudioOutput::IntermediateFrequency {
            Ok(())
        } else {
            Err(format!(
                "USB output readback was {usb_audio_output}, not Intermediate Frequency after verifying Band-B frequency"
            ))
        }
    } else {
        Err(format!(
            "Band B is at {} Hz, not {frequency_hz} Hz; arbitrary CAT frequency writes are not qualified, so tune the radio directly",
            current.as_hz()
        ))
    }
}

/// Attempt every saved-value restore, continue after failures, and verify each result.
pub(crate) async fn restore_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
    saved: SavedIfDspRadioState,
) -> IfDspRestoreReport {
    let mut report = IfDspRestoreReport::default();

    let temporary_audio_ok = radio
        .set_usb_audio_output(UsbAudioOutput::Audio)
        .await
        .is_ok()
        && matches!(
            radio.get_usb_audio_output().await,
            Ok(UsbAudioOutput::Audio)
        );
    if !temporary_audio_ok {
        report
            .failed_steps
            .push("temporary AF output for safe restoration".to_owned());
    }

    let squelch_ok = radio
        .set_squelch(Band::B, saved.band_b_squelch)
        .await
        .is_ok()
        && matches!(radio.get_squelch(Band::B).await, Ok(value) if value == saved.band_b_squelch);
    if !squelch_ok {
        report.failed_steps.push("Band-B squelch".to_owned());
    }

    let operating_mode_ok = radio
        .set_operating_mode(Band::B, saved.band_b_operating_mode)
        .await
        .is_ok()
        && matches!(
            radio.get_operating_mode(Band::B).await,
            Ok(value) if value == saved.band_b_operating_mode
        );
    if !operating_mode_ok {
        report
            .failed_steps
            .push("Band-B demodulation mode".to_owned());
    }

    let step_ok = radio
        .set_step_size(Band::B, saved.band_b_step)
        .await
        .is_ok()
        && matches!(radio.get_step_size(Band::B).await, Ok(value) if value == saved.band_b_step);
    if !step_ok {
        report.failed_steps.push("Band-B tuning step".to_owned());
    }

    if !matches!(
        radio.get_frequency(Band::B).await,
        Ok(value) if value == saved.band_b_frequency
    ) {
        report.failed_steps.push("Band-B frequency".to_owned());
    }

    let band_mode_ok = radio.set_band_mode(saved.band_mode).await.is_ok()
        && matches!(radio.get_band_mode().await, Ok(value) if value == saved.band_mode);
    if !band_mode_ok {
        report.failed_steps.push("Dual/Single Band mode".to_owned());
    }

    let operation_band_ok = radio.set_band(saved.operation_band).await.is_ok()
        && matches!(radio.get_band().await, Ok(value) if value == saved.operation_band);
    if !operation_band_ok {
        report.failed_steps.push("operation band".to_owned());
    }

    let output_ok = radio
        .set_usb_audio_output(saved.usb_audio_output)
        .await
        .is_ok()
        && matches!(radio.get_usb_audio_output().await, Ok(value) if value == saved.usb_audio_output);
    if !output_ok {
        report.failed_steps.push("USB output mode".to_owned());
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_thd75::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn save_reads_the_tuning_step_before_any_mutation() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"SF 1\r", b"SF 1,8\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0435640000\r");
        let mut radio = Radio::new(mock);

        let saved = save_if_dsp_radio_state(&mut radio).await?;

        assert_eq!(saved.operation_band, Band::A, "operation band");
        assert_eq!(saved.band_mode, BandMode::Dual);
        assert_eq!(saved.usb_audio_output, UsbAudioOutput::Audio);
        assert_eq!(saved.band_b_step, StepSize::Hz25000, "tuning step");
        assert_eq!(saved.band_b_frequency_hz(), 435_640_000, "frequency");
        Ok(())
    }

    #[tokio::test]
    async fn save_rejects_non_vfo_band_b_before_any_mutation() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"VM 1\r", b"VM 1,1\r");
        let mut radio = Radio::new(mock);

        let result = save_if_dsp_radio_state(&mut radio).await;

        assert!(
            matches!(result, Err(ref detail) if detail.contains("Memory tuning mode") && detail.contains("VFO mode")),
            "expected an explicit non-VFO refusal, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn configure_verifies_every_changed_value() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"VM 1\r", b"VM 1,0\r");
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
        let mut radio = Radio::new(mock);

        configure_if_dsp_radio(&mut radio).await?;
        Ok(())
    }

    #[tokio::test]
    async fn restore_includes_and_verifies_the_original_tuning_step() -> TestResult {
        let saved = SavedIfDspRadioState {
            operation_band: Band::A,
            band_mode: BandMode::Dual,
            usb_audio_output: UsbAudioOutput::Audio,
            band_b_squelch: SquelchLevel::try_from(2)?,
            band_b_operating_mode: OperatingMode::Fm,
            band_b_step: StepSize::Hz25000,
            band_b_frequency: Frequency::new(435_640_000),
        };
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"SF 1,8\r", b"SF 1,8\r");
        mock.expect(b"SF 1\r", b"SF 1,8\r");
        mock.expect(b"FQ 1\r", b"?\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        let mut radio = Radio::new(mock);

        let report = restore_if_dsp_radio(&mut radio, saved).await;

        assert_eq!(
            report.failed_steps,
            vec!["Band-B frequency"],
            "frequency traffic was deliberately left unanswered; all prior fields must restore"
        );
        Ok(())
    }

    #[tokio::test]
    async fn restore_keeps_if_off_until_frequency_then_restores_saved_if_last() -> TestResult {
        let saved = SavedIfDspRadioState {
            operation_band: Band::B,
            band_mode: BandMode::Single,
            usb_audio_output: UsbAudioOutput::IntermediateFrequency,
            band_b_squelch: SquelchLevel::OPEN,
            band_b_operating_mode: OperatingMode::Usb,
            band_b_step: StepSize::Hz5000,
            band_b_frequency: Frequency::new(14_074_000),
        };
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1,0\r", b"SQ 1,0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,0\r");
        mock.expect(b"MD 1,4\r", b"MD 1,4\r");
        mock.expect(b"MD 1\r", b"MD 1,4\r");
        mock.expect(b"SF 1,0\r", b"SF 1,0\r");
        mock.expect(b"SF 1\r", b"SF 1,0\r");
        mock.expect(b"FQ 1\r", b"?\r");
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"DL\r", b"DL 1\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);

        let report = restore_if_dsp_radio(&mut radio, saved).await;

        assert_eq!(
            report.failed_steps,
            vec!["Band-B frequency"],
            "saved IF output must still be restored after an unanswered tune"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_rejects_a_frequency_mismatch_without_mutating_the_radio() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0435640000\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);

        let result = retune_if_dsp_radio(&mut radio, 14_074_000).await;

        assert!(
            matches!(result, Err(ref detail) if detail.contains("435640000 Hz") && detail.contains("not 14074000 Hz") && detail.contains("not qualified")),
            "expected an explicit quarantined-write refusal, got {result:?}"
        );
        assert_eq!(
            radio.get_usb_audio_output().await?,
            UsbAudioOutput::IntermediateFrequency,
            "the sentinel IO query must remain untouched by the rejected retune"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_rejects_a_non_if_output_after_the_frequency_matches() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0014074000\r");
        mock.expect(b"IO\r", b"IO 2\r");
        let mut radio = Radio::new(mock);

        let result = retune_if_dsp_radio(&mut radio, 14_074_000).await;

        assert!(
            matches!(result, Err(ref detail) if detail.contains("Detect") && detail.contains("not Intermediate Frequency")),
            "expected an explicit IF-output readback failure, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_verifies_the_pretuned_frequency_and_if_output() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0014074000\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::new(mock);

        retune_if_dsp_radio(&mut radio, 14_074_000).await?;
        Ok(())
    }
}
