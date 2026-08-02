//! Radio-state lifecycle for live TH-D75 USB IF capture.

use kenwood_thd75::Radio;
use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::{
    Band, DetectOutputMode, Frequency, Mode, SquelchLevel, StepSize, VfoMemoryMode,
};

/// Fixed center of the TH-D75 real low-IF USB stream.
pub(crate) const IF_CENTER_HZ: u32 = 12_000;

/// Complete snapshot of every radio value changed by IF capture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedIfDspRadioState {
    operation_band: Band,
    dual_band: bool,
    output_mode: DetectOutputMode,
    band_b_squelch: SquelchLevel,
    band_b_mode: Mode,
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
    let band_b_vfo_mode = radio
        .get_vfo_memory_mode(Band::B)
        .await
        .map_err(|error| format!("reading Band-B channel mode: {error}"))?;
    if band_b_vfo_mode != VfoMemoryMode::Vfo {
        return Err(format!(
            "Band B is in {band_b_vfo_mode} mode; IF-DSP requires Band B to already be in VFO mode so its selected memory/call/weather channel is never changed"
        ));
    }
    let dual_band = radio
        .get_dual_band()
        .await
        .map_err(|error| format!("reading dual-band state: {error}"))?;
    let output_mode = radio
        .get_io_port()
        .await
        .map_err(|error| format!("reading USB output mode: {error}"))?;
    let band_b_squelch = radio
        .get_squelch(Band::B)
        .await
        .map_err(|error| format!("reading Band-B squelch: {error}"))?;
    let band_b_mode = radio
        .get_mode(Band::B)
        .await
        .map_err(|error| format!("reading Band-B demodulation mode: {error}"))?;
    let (step_band, band_b_step) = radio
        .get_step_size(Band::B)
        .await
        .map_err(|error| format!("reading Band-B tuning step: {error}"))?;
    if step_band != Band::B {
        return Err(format!(
            "reading Band-B tuning step returned unexpected band {step_band}"
        ));
    }
    let band_b_frequency = radio
        .get_frequency(Band::B)
        .await
        .map_err(|error| format!("reading Band-B frequency: {error}"))?
        .rx_frequency;

    Ok(SavedIfDspRadioState {
        operation_band,
        dual_band,
        output_mode,
        band_b_squelch,
        band_b_mode,
        band_b_step,
        band_b_frequency,
    })
}

/// Configure and verify the radio's physical 12 kHz USB IF output.
pub(crate) async fn configure_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
) -> Result<(), String> {
    let band_b_vfo_mode = radio
        .get_vfo_memory_mode(Band::B)
        .await
        .map_err(|error| format!("rechecking Band-B channel mode: {error}"))?;
    if band_b_vfo_mode != VfoMemoryMode::Vfo {
        return Err(format!(
            "Band B changed to {band_b_vfo_mode} mode before IF-DSP setup; no radio setting was changed"
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
        .set_dual_band(false)
        .await
        .map_err(|error| format!("selecting Single Band mode: {error}"))?;
    let dual_band = radio
        .get_dual_band()
        .await
        .map_err(|error| format!("verifying Single Band mode: {error}"))?;
    if dual_band {
        return Err("the radio remained in Dual Band mode".to_owned());
    }

    radio
        .set_step_size(Band::B, StepSize::Hz5000)
        .await
        .map_err(|error| format!("setting the Band-B 5 kHz tuning step: {error}"))?;
    let (step_band, step) = radio
        .get_step_size(Band::B)
        .await
        .map_err(|error| format!("verifying the Band-B tuning step: {error}"))?;
    if step_band != Band::B || step != StepSize::Hz5000 {
        return Err(format!(
            "Band-B tuning-step readback was {step_band} {step}, not Band B 5 kHz"
        ));
    }

    radio
        .set_mode(Band::B, Mode::Usb)
        .await
        .map_err(|error| format!("setting Band B to USB mode: {error}"))?;
    let mode = radio
        .get_mode(Band::B)
        .await
        .map_err(|error| format!("verifying Band-B USB mode: {error}"))?;
    if mode != Mode::Usb {
        return Err(format!("Band-B mode readback was {mode}, not USB"));
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
            squelch.as_u8()
        ));
    }

    radio
        .set_io_port(DetectOutputMode::If)
        .await
        .map_err(|error| format!("enabling 12 kHz USB IF output: {error}"))?;
    let output_mode = radio
        .get_io_port()
        .await
        .map_err(|error| format!("verifying 12 kHz USB IF output: {error}"))?;
    if output_mode != DetectOutputMode::If {
        return Err(format!(
            "USB output readback was {output_mode}, not IF; IF requires Single Band mode on Band B"
        ));
    }
    Ok(())
}

/// Retune Band B while preserving the live IF-output requirement.
pub(crate) async fn retune_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
    frequency_hz: u32,
) -> Result<(), String> {
    let pause_write_result = radio
        .set_io_port(DetectOutputMode::Af)
        .await
        .map_err(|error| format!("pausing IF output before tuning: {error}"));
    let pause_result = match pause_write_result {
        Ok(()) => radio
            .get_io_port()
            .await
            .map_err(|error| format!("verifying paused IF output: {error}")),
        Err(detail) => Err(detail),
    };
    let tune_result = match pause_result {
        Ok(DetectOutputMode::Af) => radio
            .tune_frequency(Band::B, Frequency::new(frequency_hz))
            .await
            .map_err(|error| format!("tuning Band B to {frequency_hz} Hz: {error}")),
        Ok(value) => Err(format!(
            "USB output readback was {value}, not AF before tuning"
        )),
        Err(detail) => Err(detail),
    };
    let resume_result = radio.set_io_port(DetectOutputMode::If).await;
    let resumed = radio.get_io_port().await;

    let mut failures = Vec::new();
    if let Err(detail) = tune_result {
        failures.push(detail);
    }
    if let Err(error) = resume_result {
        failures.push(format!("re-enabling IF output after tuning: {error}"));
    }
    match resumed {
        Ok(DetectOutputMode::If) => {}
        Ok(value) => failures.push(format!(
            "USB output readback was {value}, not IF after tuning"
        )),
        Err(error) => failures.push(format!("verifying IF output after tuning: {error}")),
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Restore every saved value, continue after failures, and verify each result.
pub(crate) async fn restore_if_dsp_radio<T: Transport>(
    radio: &mut Radio<T>,
    saved: SavedIfDspRadioState,
) -> IfDspRestoreReport {
    let mut report = IfDspRestoreReport::default();

    let temporary_af_ok = radio.set_io_port(DetectOutputMode::Af).await.is_ok()
        && matches!(radio.get_io_port().await, Ok(DetectOutputMode::Af));
    if !temporary_af_ok {
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

    let mode_ok = radio.set_mode(Band::B, saved.band_b_mode).await.is_ok()
        && matches!(radio.get_mode(Band::B).await, Ok(value) if value == saved.band_b_mode);
    if !mode_ok {
        report
            .failed_steps
            .push("Band-B demodulation mode".to_owned());
    }

    let step_ok = radio
        .set_step_size(Band::B, saved.band_b_step)
        .await
        .is_ok()
        && matches!(radio.get_step_size(Band::B).await, Ok((Band::B, value)) if value == saved.band_b_step);
    if !step_ok {
        report.failed_steps.push("Band-B tuning step".to_owned());
    }

    if radio
        .tune_frequency(Band::B, saved.band_b_frequency)
        .await
        .is_err()
    {
        report.failed_steps.push("Band-B frequency".to_owned());
    }

    let dual_ok = radio.set_dual_band(saved.dual_band).await.is_ok()
        && matches!(radio.get_dual_band().await, Ok(value) if value == saved.dual_band);
    if !dual_ok {
        report.failed_steps.push("Dual/Single Band mode".to_owned());
    }

    let operation_band_ok = radio.set_band(saved.operation_band).await.is_ok()
        && matches!(radio.get_band().await, Ok(value) if value == saved.operation_band);
    if !operation_band_ok {
        report.failed_steps.push("operation band".to_owned());
    }

    let output_ok = radio.set_io_port(saved.output_mode).await.is_ok()
        && matches!(radio.get_io_port().await, Ok(value) if value == saved.output_mode);
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
        let mut radio = Radio::connect(mock).await?;

        let saved = save_if_dsp_radio_state(&mut radio).await?;

        assert_eq!(saved.operation_band, Band::A, "operation band");
        assert!(saved.dual_band, "dual-band state is wire-inverted");
        assert_eq!(saved.output_mode, DetectOutputMode::Af, "output mode");
        assert_eq!(saved.band_b_step, StepSize::Hz25000, "tuning step");
        assert_eq!(saved.band_b_frequency_hz(), 435_640_000, "frequency");
        Ok(())
    }

    #[tokio::test]
    async fn save_rejects_non_vfo_band_b_before_any_mutation() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"VM 1\r", b"VM 1,1\r");
        let mut radio = Radio::connect(mock).await?;

        let result = save_if_dsp_radio_state(&mut radio).await;

        assert!(
            matches!(result, Err(ref detail) if detail.contains("Memory mode") && detail.contains("VFO mode")),
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
        let mut radio = Radio::connect(mock).await?;

        configure_if_dsp_radio(&mut radio).await?;
        Ok(())
    }

    #[tokio::test]
    async fn restore_includes_and_verifies_the_original_tuning_step() -> TestResult {
        let saved = SavedIfDspRadioState {
            operation_band: Band::A,
            dual_band: true,
            output_mode: DetectOutputMode::Af,
            band_b_squelch: SquelchLevel::try_from(2)?,
            band_b_mode: Mode::Fm,
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
        mock.expect(b"VM 1\r", b"?\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        let mut radio = Radio::connect(mock).await?;

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
            dual_band: false,
            output_mode: DetectOutputMode::If,
            band_b_squelch: SquelchLevel::OPEN,
            band_b_mode: Mode::Usb,
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
        mock.expect(b"VM 1\r", b"?\r");
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"DL\r", b"DL 1\r");
        mock.expect(b"BC 1\r", b"BC 1\r");
        mock.expect(b"BC\r", b"BC 1\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::connect(mock).await?;

        let report = restore_if_dsp_radio(&mut radio, saved).await;

        assert_eq!(
            report.failed_steps,
            vec!["Band-B frequency"],
            "saved IF output must still be restored after an unanswered tune"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_attempts_if_resume_after_af_readback_mismatch() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 2\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::connect(mock).await?;

        let result = retune_if_dsp_radio(&mut radio, 14_074_000).await;

        assert!(
            matches!(result, Err(ref detail) if detail.contains("not AF before tuning")),
            "expected the AF readback failure after verified IF resume, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_attempts_if_resume_after_af_write_error() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"?\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::connect(mock).await?;

        let result = retune_if_dsp_radio(&mut radio, 14_074_000).await;

        assert!(
            matches!(result, Err(ref detail) if detail.contains("pausing IF output")),
            "expected the AF write failure after verified IF resume, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn retune_uses_af_tune_if_and_verifies_both_frequency_and_output() -> TestResult {
        const CURRENT: &[u8] =
            b"FO 1,0435640000,0005000000,0,0,0,0,0,1,1,1,0,0,1,14,14,023,0,REPEATER,1,05\r";
        const TARGET: &[u8] =
            b"FO 1,0014074000,0005000000,0,0,0,0,0,1,1,1,0,0,1,14,14,023,0,REPEATER,1,05\r";
        let mut mock = MockTransport::new();
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"VM 1\r", b"VM 1,0\r");
        mock.expect(b"FO 1\r", CURRENT);
        mock.expect(TARGET, TARGET);
        mock.expect(b"FQ 1\r", b"FQ 1,0014074000\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        mock.expect(b"IO\r", b"IO 1\r");
        let mut radio = Radio::connect(mock).await?;

        retune_if_dsp_radio(&mut radio, 14_074_000).await?;
        Ok(())
    }
}
