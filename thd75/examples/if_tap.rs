//! Capture the TH-D75's AF, IF, and detector outputs, then restore the settings
//! changed for the capture. The typed IF-tap session owns the whole lifecycle:
//! it guards Band B VFO mode, snapshots every touched setting, proves
//! engagement by readback, retunes to the requested frequency with the
//! verified UP/DW walk, and restores in the hardware-required order on exit.
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
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::error::Error;

use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Frequency, OperatingMode, UsbAudioOutput};
use kenwood_thd75::{IfTapConfig, IfTapSession, Radio};

/// Default capture frequency: a 70 cm slot on the 5 kHz raster, proving
/// IF output works on VHF/UHF (the per-sub-band default mode table is
/// only a default).
const DEFAULT_FREQUENCY: Frequency = Frequency::new(435_640_000);

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

fn parse_mhz(arg: Option<String>) -> Result<Frequency, Box<dyn Error>> {
    arg.map_or_else(
        || Ok(DEFAULT_FREQUENCY),
        |s| Frequency::from_mhz_str(&s).map_err(|e| format!("invalid frequency {s:?}: {e}").into()),
    )
}

async fn run_captures(
    session: &mut IfTapSession<'_, SerialTransport>,
    freq: Frequency,
    idx: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    // Land Band B on the requested frequency under the engaged tap: the
    // verified UP/DW walk refuses off-step or distant targets.
    let landed = session.step_to_frequency(freq).await?;
    println!(
        "tuned: single-band B, {} Hz, USB, squelch open, IF engaged",
        landed.as_hz()
    );
    settle().await;
    if let Some(idx) = idx {
        capture(idx, "if_tap.wav")?;
    }

    let radio = session.radio();
    radio.set_usb_audio_output(UsbAudioOutput::Detect).await?;
    let now = radio.get_usb_audio_output().await?;
    if matches!(now, UsbAudioOutput::Detect) {
        println!("IO = Detect engaged (readback confirmed)");
        settle().await;
        if let Some(idx) = idx {
            capture(idx, "if_detect_tap.wav")?;
        }
    } else {
        println!("warning: Detect readback gave {now}; skipping detect step");
    }

    // AF reference last: drop to the audio path (exit re-forces it anyway).
    let radio = session.radio();
    radio.set_usb_audio_output(UsbAudioOutput::Audio).await?;
    let now = radio.get_usb_audio_output().await?;
    if matches!(now, UsbAudioOutput::Audio) {
        settle().await;
        if let Some(idx) = idx {
            capture(idx, "if_af_ref.wav")?;
        }
    } else {
        println!("warning: Audio readback gave {now}; skipping AF reference");
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

    let transport = SerialTransport::open(&port)?;
    let mut radio = Radio::connect_with_tnc_exit(transport).await?;
    let info = radio.identify().await?;
    println!("connected: {}", info.model);

    // The session guards Band B VFO mode, saves every touched setting,
    // applies single-band B / USB / squelch-open, and proves IF engaged.
    // A mid-configure failure rolls the radio back before returning.
    let mut session = match radio
        .enter_if_tap(IfTapConfig::new(OperatingMode::Usb))
        .await
    {
        Ok(session) => session,
        Err(error) => {
            for (step, step_error) in error.rollback.failures() {
                println!("rollback failure: {step}: {step_error}");
            }
            drop(radio.disconnect().await);
            return Err(error.to_string().into());
        }
    };
    println!("IF tap engaged; snapshot: {:?}", session.saved_state());

    let result = run_captures(&mut session, freq, idx.as_deref()).await;
    if let Err(e) = &result {
        println!("TEST ERROR (restoring anyway): {e}");
    }

    // Ordered restore with per-step reporting, including the original Band B
    // frequency through the saved tuning raster.
    println!("restoring saved state...");
    let report = session.exit().await;
    if report.is_complete() {
        println!("restored: every saved setting confirmed");
    } else {
        for (step, error) in report.failures() {
            println!("restore failure: {step}: {error}");
        }
        for step in report.not_attempted() {
            println!("restore skipped: {step}");
        }
    }

    drop(radio.disconnect().await);
    result.map(|()| println!("IF tap example complete"))
}
