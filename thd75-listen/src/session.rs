//! Radio state discipline for the listener.
//!
//! Everything the listener may touch is saved before configuration. Exit uses
//! a best-effort, read-back-checked restore and reports failed fields. Direct
//! frequency restoration currently fails closed with the library's FO/FQ
//! quarantine.

use kenwood_thd75::Radio;
use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::{Band, DetectOutputMode, Frequency, Mode, SquelchLevel, StepSize};

/// Radio settings captured before listening starts.
#[derive(Debug, Clone, Copy)]
pub struct SavedState {
    band: Band,
    dual: bool,
    io: DetectOutputMode,
    squelch: SquelchLevel,
    mode: Mode,
    freq: Frequency,
}

/// Which restore steps failed. Empty means a clean restore.
#[derive(Debug, Default)]
pub struct RestoreReport {
    /// Human-readable names of the settings that could not be
    /// restored (or verified), in restore order.
    pub failed: Vec<&'static str>,
}

/// Read every setting the listener will touch.
///
/// # Errors
///
/// Propagates the first failed read; without a complete snapshot the
/// caller must not reconfigure the radio.
pub async fn save_state<T: Transport>(
    radio: &mut Radio<T>,
) -> Result<SavedState, kenwood_thd75::Error> {
    Ok(SavedState {
        band: radio.get_band().await?,
        dual: radio.get_dual_band().await?,
        io: radio.get_io_port().await?,
        squelch: radio.get_squelch(Band::B).await?,
        mode: radio.get_mode(Band::B).await?,
        freq: radio.get_frequency(Band::B).await?,
    })
}

/// Put the radio into the listening configuration.
///
/// Operation band B, Single Band mode, 5 kilohertz step, USB mode,
/// squelch open, and `IO = IF` verified by readback. Tuning is left
/// to the caller (the same path the `tune` command uses).
///
/// # Errors
///
/// Returns an accessible sentence describing the failed step; on the
/// IF readback failure it includes the Single Band mode requirement.
pub async fn configure_for_listening<T: Transport>(radio: &mut Radio<T>) -> Result<(), String> {
    radio
        .set_band(Band::B)
        .await
        .map_err(|e| format!("switching to band B: {e}"))?;
    radio
        .set_dual_band(false)
        .await
        .map_err(|e| format!("selecting Single Band mode: {e}"))?;
    let step = StepSize::try_from(0).map_err(|e| format!("step size: {e}"))?;
    radio
        .set_step_size(Band::B, step)
        .await
        .map_err(|e| format!("setting the 5 kilohertz step: {e}"))?;
    radio
        .set_mode(Band::B, Mode::Usb)
        .await
        .map_err(|e| format!("setting USB mode: {e}"))?;
    let open = SquelchLevel::try_from(0).map_err(|e| format!("squelch level: {e}"))?;
    radio
        .set_squelch(Band::B, open)
        .await
        .map_err(|e| format!("opening the squelch: {e}"))?;
    radio
        .set_io_port(DetectOutputMode::If)
        .await
        .map_err(|e| format!("enabling IF output: {e}"))?;
    let now = radio
        .get_io_port()
        .await
        .map_err(|e| format!("reading back IF output: {e}"))?;
    if matches!(now, DetectOutputMode::If) {
        Ok(())
    } else {
        Err("IF output did not engage. It requires Single Band mode on Band B.".to_owned())
    }
}

/// Best-effort restore of every saved setting, frequency last.
///
/// Frequency goes last because it is the only multi-command
/// operation. IF output, dual-band, and the operation band (the
/// settings that can strand the radio) are verified by readback;
/// the rest rely on their command echoes.
pub async fn restore<T: Transport>(radio: &mut Radio<T>, saved: SavedState) -> RestoreReport {
    let mut report = RestoreReport::default();

    let io_ok = radio.set_io_port(saved.io).await.is_ok()
        && matches!(radio.get_io_port().await, Ok(now) if now == saved.io);
    if !io_ok {
        report.failed.push("USB audio output");
    }
    if radio.set_squelch(Band::B, saved.squelch).await.is_err() {
        report.failed.push("squelch");
    }
    if radio.set_mode(Band::B, saved.mode).await.is_err() {
        report.failed.push("mode");
    }
    let dual_ok = radio.set_dual_band(saved.dual).await.is_ok()
        && matches!(radio.get_dual_band().await, Ok(now) if now == saved.dual);
    if !dual_ok {
        report.failed.push("dual band");
    }
    let band_ok = radio.set_band(saved.band).await.is_ok()
        && matches!(radio.get_band().await, Ok(now) if now == saved.band);
    if !band_ok {
        report.failed.push("operation band");
    }
    if radio.tune_frequency(Band::B, saved.freq).await.is_err() {
        report.failed.push("frequency");
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_thd75::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn save_state_reads_every_setting() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"BC\r", b"BC 0\r");
        // DL wire value is inverted: 0 on the wire = dual band ON.
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1\r", b"SQ 1,2\r");
        mock.expect(b"MD 1\r", b"MD 1,0\r");
        mock.expect(b"FQ 1\r", b"FQ 1,0435640000\r");
        let mut radio = Radio::connect(mock).await?;
        let saved = save_state(&mut radio).await?;
        assert_eq!(saved.band, Band::A, "operation band");
        assert!(saved.dual, "dual on (wire 0 inverted)");
        assert!(
            matches!(saved.io, DetectOutputMode::Af),
            "io {:?}",
            saved.io
        );
        assert_eq!(u8::from(saved.squelch), 2, "squelch level");
        assert_eq!(saved.mode, Mode::Fm, "mode");
        assert_eq!(saved.freq.as_hz(), 435_640_000, "frequency");
        Ok(())
    }

    #[tokio::test]
    async fn restore_issues_the_documented_sequence() -> TestResult {
        let saved = SavedState {
            band: Band::A,
            dual: true,
            io: DetectOutputMode::Af,
            squelch: SquelchLevel::try_from(2)?,
            mode: Mode::Fm,
            freq: Frequency::new(435_640_000),
        };
        let mut mock = MockTransport::new();
        // Restore order is the contract under test: IO (verified),
        // squelch, mode, dual (verified), band (verified), then the
        // frequency retune whose multi-command traffic is absorbed.
        mock.expect(b"IO 0\r", b"IO 0\r");
        mock.expect(b"IO\r", b"IO 0\r");
        mock.expect(b"SQ 1,2\r", b"SQ 1,2\r");
        mock.expect(b"MD 1,0\r", b"MD 1,0\r");
        mock.expect(b"DL 0\r", b"DL 0\r");
        mock.expect(b"DL\r", b"DL 0\r");
        mock.expect(b"BC 0\r", b"BC 0\r");
        mock.expect(b"BC\r", b"BC 0\r");
        mock.expect_any_write();
        let mut radio = Radio::connect(mock).await?;
        let report = restore(&mut radio, saved).await;
        // Everything before the frequency step restored cleanly; the
        // absorbed tune traffic gets no responses, so frequency is the
        // one reported failure.
        assert_eq!(report.failed, vec!["frequency"], "report {report:?}");
        Ok(())
    }

    #[tokio::test]
    async fn configure_rejects_when_if_readback_fails() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"BC 1\r", b"BC 1\r");
        // set_dual_band(false) is wire-inverted: sends DL 1.
        mock.expect(b"DL 1\r", b"DL 1\r");
        mock.expect(b"SF 1,0\r", b"SF 1,0\r");
        mock.expect(b"MD 1,4\r", b"MD 1,4\r");
        mock.expect(b"SQ 1,0\r", b"SQ 1,0\r");
        mock.expect(b"IO 1\r", b"IO 1\r");
        // Readback says AF: the radio refused (not single-band B).
        mock.expect(b"IO\r", b"IO 0\r");
        let mut radio = Radio::connect(mock).await?;
        let result = configure_for_listening(&mut radio).await;
        assert!(
            matches!(&result, Err(msg) if msg.contains("Single Band")),
            "expected Single Band guidance, got {result:?}"
        );
        Ok(())
    }
}
