//! Execute one exact guarded key tap and retain before/after screen evidence.
//!
//! This is the minimal recovery and experimentation companion to
//! `automation_probe`: it does not infer a prior UI state and does not attempt
//! to undo the key. The optional text argument requires one exact, confident
//! OCR result on the resulting screen.
//!
//! Usage:
//! ```text
//! cargo run -p kenwood-thd75 --release --example automation_tap -- \
//!   KEY_HEX [EXPECTED_TEXT] [OUTPUT_DIR] [DEVICE_NAME]
//! ```

// Keep every workspace example dependency represented under the workspace's
// strict unused-dependency lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

#[cfg(target_os = "macos")]
fn differing_pixels(
    left: &kenwood_thd75::screen::ScreenFrame,
    right: &kenwood_thd75::screen::ScreenFrame,
) -> usize {
    left.rgb565_le()
        .chunks_exact(2)
        .zip(right.rgb565_le().chunks_exact(2))
        .filter(|(left_pixel, right_pixel)| left_pixel != right_pixel)
        .count()
}

#[cfg(target_os = "macos")]
#[tokio::main(flavor = "current_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "The single-key evidence transaction is intentionally linear so qualification, \
              control, capture, OCR, and retained output stay in execution order."
)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use kenwood_thd75::Radio;
    use kenwood_thd75::radio::automation::FrontPanelKey;
    use kenwood_thd75::screen::vision::{NormalizedBounds, require_unique_text};
    use kenwood_thd75::transport::BluetoothTransport;

    let mut arguments = std::env::args().skip(1);
    let key_text = arguments
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "KEY_HEX is required"))?;
    let expected_text = arguments.next();
    let output_dir = if let Some(path) = arguments.next() {
        PathBuf::from(path)
    } else {
        let epoch_millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        PathBuf::from("automation-evidence").join(format!("{epoch_millis}-tap-{key_text}"))
    };
    let device_name = arguments.next().unwrap_or_else(|| "TH-D75".to_owned());
    if let Some(extra) = arguments.next() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unexpected extra argument: {extra}"),
        )
        .into());
    }

    let hexadecimal = key_text
        .strip_prefix("0x")
        .or_else(|| key_text.strip_prefix("0X"))
        .unwrap_or(&key_text);
    let raw_key = u8::from_str_radix(hexadecimal, 16).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid hexadecimal key ID {key_text:?}: {error}"),
        )
    })?;
    let key = FrontPanelKey::try_from(raw_key)?;

    let transport = BluetoothTransport::open(Some(&device_name))?;
    let mut radio = Radio::connect(transport).await?;
    let qualification_started = Instant::now();
    let (before, key_metadata, after, key_elapsed) = {
        let mut session = radio.qualify_automation().await?;
        println!(
            "qualification_ms={:.3}",
            qualification_started.elapsed().as_secs_f64() * 1_000.0
        );
        let before = session.capture_screen().await?;
        let key_started = Instant::now();
        let key_metadata = session.tap_key(key).await?;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let after = session.capture_screen().await?;
        (before, key_metadata, after, key_started.elapsed())
    };
    radio.disconnect().await?;

    fs::create_dir_all(&output_dir)?;
    fs::write(output_dir.join("before.bmp"), before.frame.to_stock_bmp())?;
    fs::write(output_dir.join("after.bmp"), after.frame.to_stock_bmp())?;
    let changed_pixels = differing_pixels(&before.frame, &after.frame);
    if changed_pixels == 0 {
        return Err(io::Error::other(format!(
            "key 0x{raw_key:02X} produced no framebuffer change"
        ))
        .into());
    }
    println!(
        "key=0x{raw_key:02X},host_sequence:{:02X},command_count:{},result:{},elapsed_ms:{:.3}",
        key_metadata.last_host_sequence,
        key_metadata.command_count,
        key_metadata.last_key_result,
        key_elapsed.as_secs_f64() * 1_000.0
    );
    println!(
        "before=generation:{},crc32:{:08X}",
        before.metadata.generation, before.metadata.crc32
    );
    println!(
        "after=generation:{},crc32:{:08X},changed_pixels:{changed_pixels}",
        after.metadata.generation, after.metadata.crc32
    );

    let observations = after.frame.recognize_text()?;
    for observation in &observations {
        println!(
            "text={:?},confidence:{:.4}",
            observation.text(),
            observation.confidence()
        );
    }
    if let Some(expected) = expected_text.as_deref() {
        let matched = require_unique_text(
            &observations,
            expected,
            0.90,
            NormalizedBounds::FULL_SCREEN,
            1.0,
        )?;
        println!(
            "asserted_text={:?},confidence:{:.4}",
            matched.text(),
            matched.confidence()
        );
    }
    println!("evidence_dir={}", output_dir.display());
    println!("result=PASS");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    use kenwood_thd75 as _;
    use tokio as _;

    eprintln!("automation_tap requires macOS native Bluetooth");
    std::process::exit(2);
}
