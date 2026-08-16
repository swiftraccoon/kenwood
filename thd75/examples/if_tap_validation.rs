//! Live validation of the typed IF-tap session lifecycle.
//!
//! Exercises [`Radio::enter_if_tap`] against the real radio: precondition
//! guard, snapshot, configure-with-engagement-proof, a bounded UP/DW retune
//! walk there and back, and the ordered restore. Every claim is verified by
//! an independent readback and printed as a CHECK line; the process exits
//! nonzero when any check fails.
//!
//! The walk steps Band B up two 5 kHz steps and back; the radio ends on its
//! starting frequency. Receive-only; nothing here transmits.
//!
//! Usage:
//! ```text
//! cargo run -p kenwood-thd75 --example if_tap_validation -- /dev/cu.usbmodem101
//! ```

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{
    Band, BandMode, Frequency, OperatingMode, SquelchLevel, StepSize, TuningMode, UsbAudioOutput,
};
use kenwood_thd75::{IfTapConfig, Radio};

/// One line of evidence; failures flip the process exit code.
fn check(passed: &mut bool, label: &str, ok: bool, detail: &str) {
    let verdict = if ok { "PASS" } else { "FAIL" };
    println!("CHECK {verdict}: {label} ({detail})");
    *passed &= ok;
}

/// Snapshot of everything the session touches, read directly.
struct Observed {
    band: Band,
    band_mode: BandMode,
    output: UsbAudioOutput,
    squelch: SquelchLevel,
    mode: OperatingMode,
    step: StepSize,
    frequency: Frequency,
}

async fn observe(
    radio: &mut Radio<SerialTransport>,
) -> Result<Observed, Box<dyn std::error::Error>> {
    Ok(Observed {
        band: radio.get_band().await?,
        band_mode: radio.get_band_mode().await?,
        output: radio.get_usb_audio_output().await?,
        squelch: radio.get_squelch(Band::B).await?,
        mode: radio.get_operating_mode(Band::B).await?,
        step: radio.get_step_size(Band::B).await?,
        frequency: radio.get_frequency(Band::B).await?,
    })
}

/// Read back the configured tap state and check every field.
async fn validate_engaged(
    radio: &mut Radio<SerialTransport>,
    passed: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let engaged = observe(radio).await?;
    check(
        passed,
        "active band is B",
        engaged.band == Band::B,
        &format!("{}", engaged.band),
    );
    check(
        passed,
        "Single Band mode engaged",
        engaged.band_mode == BandMode::Single,
        &format!("{:?}", engaged.band_mode),
    );
    check(
        passed,
        "IF output engaged",
        engaged.output == UsbAudioOutput::IntermediateFrequency,
        &format!("{:?}", engaged.output),
    );
    check(
        passed,
        "USB mode selected on Band B",
        engaged.mode == OperatingMode::Usb,
        &format!("{}", engaged.mode),
    );
    check(
        passed,
        "squelch open",
        engaged.squelch == SquelchLevel::OPEN,
        &format!("{}", engaged.squelch.as_raw()),
    );
    check(
        passed,
        "5 kHz step selected",
        engaged.step == StepSize::Hz5000,
        &format!("{}", engaged.step),
    );
    Ok(())
}

/// Step two 5 kHz steps up and walk back, verifying each landing.
async fn validate_retune_walk(
    radio: &mut Radio<SerialTransport>,
    passed: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let start = radio.get_frequency(Band::B).await?;
    let up_two = start
        .checked_add_hz(10_000)
        .ok_or("frequency walk would overflow")?;
    println!("Retuning {start} -> {up_two} via qualified stepping...");
    let there = radio
        .retune_if_tap(up_two, UsbAudioOutput::IntermediateFrequency)
        .await;
    match there {
        Ok(landed) => check(
            passed,
            "stepped retune lands and re-engages",
            landed == up_two,
            &format!("landed {landed}"),
        ),
        Err(error) => check(
            passed,
            "stepped retune lands and re-engages",
            false,
            &error.to_string(),
        ),
    }
    let back = radio
        .retune_if_tap(start, UsbAudioOutput::IntermediateFrequency)
        .await;
    match back {
        Ok(landed) => check(
            passed,
            "walk back to the starting frequency",
            landed == start,
            &format!("landed {landed}"),
        ),
        Err(error) => check(
            passed,
            "walk back to the starting frequency",
            false,
            &error.to_string(),
        ),
    }
    Ok(())
}

/// Compare the post-restore state against the pre-session observation.
async fn validate_restored(
    radio: &mut Radio<SerialTransport>,
    before: &Observed,
    passed: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let after = observe(radio).await?;
    check(
        passed,
        "active band restored",
        after.band == before.band,
        &format!("{} -> {}", before.band, after.band),
    );
    check(
        passed,
        "band mode restored",
        after.band_mode == before.band_mode,
        &format!("{:?} -> {:?}", before.band_mode, after.band_mode),
    );
    check(
        passed,
        "USB audio output restored",
        after.output == before.output,
        &format!("{:?} -> {:?}", before.output, after.output),
    );
    check(
        passed,
        "squelch restored",
        after.squelch == before.squelch,
        &format!("{} -> {}", before.squelch.as_raw(), after.squelch.as_raw()),
    );
    check(
        passed,
        "operating mode restored",
        after.mode == before.mode,
        &format!("{} -> {}", before.mode, after.mode),
    );
    check(
        passed,
        "tuning step restored (historical defect)",
        after.step == before.step,
        &format!("{} -> {}", before.step, after.step),
    );
    check(
        passed,
        "frequency back at the starting value",
        after.frequency == before.frequency,
        &format!("{} -> {}", before.frequency, after.frequency),
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem101".to_owned());
    let mut passed = true;

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port)?;
    let mut radio = Radio::connect_with_tnc_exit(transport).await?;
    let info = radio.identify().await?;
    let firmware = radio.get_firmware_version().await?;
    println!("Radio: {} firmware {firmware}", info.model);

    // Band B must be in VFO mode for the session; put it there if needed and
    // remember to restore the original tuning mode afterwards.
    let original_tuning = radio.get_tuning_mode(Band::B).await?;
    if original_tuning != TuningMode::Vfo {
        println!("Band B tuning mode is {original_tuning:?}; demonstrating the VFO guard...");
        let refused = radio
            .enter_if_tap(IfTapConfig::new(OperatingMode::Usb))
            .await;
        check(
            &mut passed,
            "non-VFO tuning is refused before any mutation",
            refused.is_err(),
            &format!("{:?}", refused.err().map(|error| error.to_string())),
        );
        radio.set_tuning_mode(Band::B, TuningMode::Vfo).await?;
    }

    let before = observe(&mut radio).await?;
    println!(
        "Pre-state: band {}, {:?}, output {:?}, squelch {}, mode {}, step {}, {}",
        before.band,
        before.band_mode,
        before.output,
        before.squelch.as_raw(),
        before.mode,
        before.step,
        before.frequency,
    );

    // Enter with an explicit step so the snapshot covers it (the historical
    // step-restore defect is exactly what this proves fixed).
    let config = IfTapConfig::new(OperatingMode::Usb).with_step(StepSize::Hz5000);
    let session = match radio.enter_if_tap(config).await {
        Ok(session) => session,
        Err(error) => {
            println!("FAIL: enter_if_tap: {error}");
            drop(radio.disconnect().await);
            return Err(error.to_string().into());
        }
    };
    let saved = session.into_saved_state();
    println!("Entered; snapshot taken: {saved:?}");

    validate_engaged(&mut radio, &mut passed).await?;
    validate_retune_walk(&mut radio, &mut passed).await?;

    // Ordered restore, then prove every saved value landed.
    let report = radio.restore_if_tap(saved).await;
    check(
        &mut passed,
        "restore completes every step",
        report.is_complete(),
        &format!("{report:?}"),
    );
    validate_restored(&mut radio, &before, &mut passed).await?;

    if original_tuning != TuningMode::Vfo {
        radio.set_tuning_mode(Band::B, original_tuning).await?;
        println!("Restored Band B tuning mode to {original_tuning:?}.");
    }
    drop(radio.disconnect().await);

    if passed {
        println!("ALL CHECKS PASSED");
        Ok(())
    } else {
        Err("one or more checks failed".into())
    }
}
