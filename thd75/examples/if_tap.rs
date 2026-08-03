//! Engage the TH-D75's IF tap (CAT `IO`, radio Menu 102) and verify it
//! end to end, restoring every touched setting afterwards.
//!
//! The radio's USB interface carries a capture-only audio function whose
//! device name is "ADC stream IN" (mono 48 kHz), not "TH-D75". With
//! `IO 1` the radio streams the Band B receiver's 12 kHz-centered IF
//! (about 15 kHz passband, per Kenwood's TH-D75 Operating Tips section
//! 5.10.5) on that device instead of received audio; `IO 2` streams the
//! detection signal. Both require Single Band mode on Band B and a
//! non-DV mode.
//!
//! When `ffmpeg` is installed, each output mode is recorded for three
//! seconds into the current directory (`if_af_ref.wav`, `if_tap.wav`,
//! `if_detect_tap.wav`); without it the engage/readback/restore cycle
//! still runs and manual capture instructions are printed.
//!
//! Usage:
//! ```text
//! cargo run -p kenwood-thd75 --example if_tap -- /dev/cu.usbmodem101
//! cargo run -p kenwood-thd75 --example if_tap -- /dev/cu.usbmodem101 435.640
//! ```

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
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

use std::error::Error;

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, DetectOutputMode, Frequency, Mode, SquelchLevel};

/// Default capture frequency: a 70 cm slot on the 5 kHz raster, proving
/// IF output works on VHF/UHF (the per-sub-band default mode table is
/// only a default).
const DEFAULT_MHZ: f64 = 435.640;

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
}

/// Locate the radio's audio interface in ffmpeg's avfoundation device
/// list. The interface's USB string descriptor is "ADC stream IN".
fn find_adc_index() -> Option<String> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if let Some(head) = line.find("] ADC stream IN").map(|p| line.get(..p)) {
            let head = head?;
            if let Some(bracket) = head.rfind('[') {
                let idx: String = head
                    .get(bracket + 1..)?
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if !idx.is_empty() {
                    return Some(idx);
                }
            }
        }
    }
    None
}

/// Record three seconds from the radio's audio interface. Blocking is
/// fine here: nothing else needs to run while a capture is in flight.
fn capture(idx: &str, name: &str) -> Result<(), Box<dyn Error>> {
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "avfoundation",
            "-i",
            &format!(":{idx}"),
            "-t",
            "3",
            "-y",
            name,
        ])
        .status()?;
    if !status.success() {
        return Err(format!("ffmpeg capture of {name} failed").into());
    }
    println!("captured {name}");
    Ok(())
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "megahertz input is validated finite and positive below; multiplied by 1e6 it \
              stays well inside u32 for every frequency the radio accepts"
)]
fn parse_mhz(arg: Option<String>) -> Result<Frequency, Box<dyn Error>> {
    let mhz: f64 = match arg {
        None => DEFAULT_MHZ,
        Some(s) => s
            .parse()
            .map_err(|e| format!("invalid frequency {s:?}: {e}"))?,
    };
    if !mhz.is_finite() || mhz <= 0.0 {
        return Err("frequency must be a positive number in megahertz".into());
    }
    Ok(Frequency::new((mhz * 1_000_000.0) as u32))
}

async fn run_test(
    radio: &mut Radio<SerialTransport>,
    freq: Frequency,
    idx: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    // Preconditions for IF/Detect: operation band B, Single Band mode,
    // non-DV mode.
    radio.set_band(Band::B).await?;
    radio.set_dual_band(false).await?;
    radio.tune_frequency(Band::B, freq).await?;
    radio.set_mode(Band::B, Mode::Usb).await?;
    radio
        .set_squelch(Band::B, SquelchLevel::try_from(0)?)
        .await?;
    println!(
        "configured: single-band B, {} Hz, USB, squelch open",
        freq.as_hz()
    );
    settle().await;

    if let Some(idx) = idx {
        capture(idx, "if_af_ref.wav")?;
    }

    radio.set_io_port(DetectOutputMode::If).await?;
    let now = radio.get_io_port().await?;
    if !matches!(now, DetectOutputMode::If) {
        return Err(format!("IO readback after set If: {now} - IF not engaged").into());
    }
    println!("IO = IF engaged (readback confirmed)");
    settle().await;
    if let Some(idx) = idx {
        capture(idx, "if_tap.wav")?;
    }

    radio.set_io_port(DetectOutputMode::Detect).await?;
    let now = radio.get_io_port().await?;
    if matches!(now, DetectOutputMode::Detect) {
        println!("IO = Detect engaged (readback confirmed)");
        settle().await;
        if let Some(idx) = idx {
            capture(idx, "if_detect_tap.wav")?;
        }
    } else {
        println!("warning: Detect readback gave {now}; skipping detect step");
    }

    if idx.is_none() {
        println!("ffmpeg not found: skipped captures.");
        println!("To capture manually, record the audio input device named");
        println!("\"ADC stream IN\" while this example holds each mode.");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let port = args
        .next()
        .ok_or("usage: if_tap <serial-port> [freq-mhz]")?;
    let freq = parse_mhz(args.next())?;

    let idx = if ffmpeg_available() {
        find_adc_index()
    } else {
        None
    };
    match &idx {
        Some(i) => println!("audio device 'ADC stream IN' at avfoundation index {i}"),
        None => println!("no ffmpeg / audio device found; running without captures"),
    }

    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect_safe(transport).await?;
    let info = radio.identify().await?;
    println!("connected: {}", info.model);

    // Save every setting the test touches.
    let band0 = radio.get_band().await?;
    let dual0 = radio.get_dual_band().await?;
    let io0 = radio.get_io_port().await?;
    let sq0 = radio.get_squelch(Band::B).await?;
    let mode0 = radio.get_mode(Band::B).await?;
    let freq0 = radio.get_frequency(Band::B).await?;
    println!(
        "saved state: band={band0:?} dual={dual0} io={io0} squelch={} mode={mode0} freq={} Hz",
        u8::from(sq0),
        freq0.as_hz()
    );

    let result = run_test(&mut radio, freq, idx.as_deref()).await;
    if let Err(e) = &result {
        println!("TEST ERROR (restoring anyway): {e}");
    }

    // Best-effort restore, then verify it landed.
    println!("restoring saved state...");
    drop(radio.set_io_port(io0).await);
    drop(radio.set_squelch(Band::B, sq0).await);
    drop(radio.set_mode(Band::B, mode0).await);
    drop(radio.tune_frequency(Band::B, freq0).await);
    drop(radio.set_dual_band(dual0).await);
    drop(radio.set_band(band0).await);
    let io_v = radio.get_io_port().await?;
    let dual_v = radio.get_dual_band().await?;
    let band_v = radio.get_band().await?;
    println!("restored: io={io_v} dual={dual_v} band={band_v:?}");

    drop(radio.disconnect().await);
    result.map(|()| println!("IF tap example complete"))
}
