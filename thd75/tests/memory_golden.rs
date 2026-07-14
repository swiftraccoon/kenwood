// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Hardware ground-truth pin for the MCP memory-image layout.
//!
//! Every other memory test in this crate builds a synthetic image,
//! writes fields into it, and reads them back. That round-trip is
//! blind to the failure mode that actually matters here: the channel
//! and settings decoders are hand-maintained BITFIELD MAPS (mode
//! nibble in byte 0x09, tone/CTCSS/DCS/shift bits in byte 0x0A,
//! region offsets 0x2000/0x4000/0x10000, the settings cells spanning
//! 0x1000..0x1A10 plus the D-STAR MY-callsign list at 0x1CA8). A
//! symmetric mistake — writing and reading the same wrong bit —
//! passes every round-trip test while corrupting the radio.
//!
//! This file decodes a real 500,480-byte MCP dump read off a physical
//! TH-D75 and asserts values known to be true of that radio. Nothing
//! here is re-derived from our own encoder, so a layout regression has
//! nowhere to hide.
//!
//! The fixtures are committed, so this runs in CI like any other test.

use kenwood_thd75::memory::MemoryImage;
// `MemoryMode`, NOT `Mode`: flash records use the MCP/SD-card mode
// table (FM=0, DV=1, AM=2, …), which differs from the CAT wire table
// for the same byte. Mixing them is the crate's canonical footgun.
use kenwood_thd75::types::{MemoryMode, StepSize};

// Deps visible to this compilation unit but unused here.
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
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Expected size of a full MCP dump (all four regions).
const DUMP_LEN: usize = 500_480;

fn load_dump() -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let raw = std::fs::read("tests/fixtures/memory_dump.bin")?;
    assert_eq!(raw.len(), DUMP_LEN, "fixture is not a full MCP dump");
    Ok(MemoryImage::from_raw(raw)?)
}

/// The real dump parses, and every channel slot the radio marks as
/// used decodes into a valid `FlashChannel`.
///
/// Corrupt records are hard errors in the channel decoder, so a
/// layout regression that shifts a record boundary shows up here as
/// an unparseable slot rather than as silently wrong data.
///
/// The counts also pin the regular/special split: `count()` covers
/// only the regular bank (0..=999), while the flag region spans all
/// 1,200 entries — the extra used slots on this radio are its special
/// channels (call/scan-edge/weather). A regression that moved the
/// regular-bank boundary would change one count without the other.
#[test]
fn real_dump_parses_and_every_used_channel_decodes() -> TestResult {
    let image = load_dump()?;
    let channels = image.channels();

    assert_eq!(
        channels.count(),
        140,
        "the source radio has 140 regular channels programmed (bank 0..=999)"
    );

    let mut used_regular = 0usize;
    let mut used_special = 0usize;
    for number in 0..1200u16 {
        if channels.is_used(number) {
            if number <= 999 {
                used_regular += 1;
            } else {
                used_special += 1;
            }
            assert!(
                channels.flash(number).is_some(),
                "channel {number} is marked used but its flash record does not decode"
            );
        }
    }
    assert_eq!(
        used_regular,
        channels.count(),
        "the regular-bank scan must agree with count()"
    );
    assert_eq!(
        used_special, 16,
        "the source radio has 16 special channels programmed (bank 1000..=1199)"
    );
    Ok(())
}

/// Known-true channel contents, read off the physical radio.
///
/// These are public-safety memories programmed into the source
/// handheld — frequency, name, mode, step and shift all come from the
/// hardware, not from our encoder. A wrong mode nibble, a shifted
/// frequency word, or an off-by-one in the 32-byte record stride
/// fails here.
#[test]
fn channel_zero_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let channels = image.channels();

    let ch0 = channels
        .get(0)
        .ok_or("channel 0 must be present in the dump")?;
    assert_eq!(ch0.name, "RCOFIRETAC", "channel 0 name");
    assert_eq!(
        ch0.flash.rx_frequency.as_hz(),
        154_205_000,
        "channel 0 RX frequency (154.205 MHz)"
    );
    assert_eq!(ch0.flash.mode, MemoryMode::Fm, "channel 0 is an FM memory");
    assert_eq!(
        ch0.flash.step_size,
        StepSize::Hz5000,
        "channel 0 tuning step"
    );
    assert!(!ch0.flash.narrow, "channel 0 is wide FM");
    assert!(ch0.used, "channel 0 is a used slot");
    assert!(!ch0.lockout, "channel 0 is not locked out");

    // Two more memories fix the record stride: a boundary error that
    // still produced a plausible channel 0 cannot also produce these.
    let ch1 = channels.get(1).ok_or("channel 1 must be present")?;
    assert_eq!(ch1.name, "RCOEMSTAC1", "channel 1 name");
    assert_eq!(
        ch1.flash.rx_frequency.as_hz(),
        155_220_000,
        "channel 1 RX frequency (155.220 MHz)"
    );

    let ch2 = channels.get(2).ok_or("channel 2 must be present")?;
    assert_eq!(ch2.name, "RCOEMSTAC2", "channel 2 name");
    assert_eq!(
        ch2.flash.rx_frequency.as_hz(),
        155_280_000,
        "channel 2 RX frequency (155.280 MHz)"
    );
    Ok(())
}

/// Known-true settings bytes, read off the physical radio.
///
/// The settings layer is a flat offset map — every accessor is a
/// hand-written byte/bit index sourced from the MCP-D75 field
/// registry. Decoding the real dump pins each index to hardware, so
/// an accessor that reads the wrong cell (the historical failure mode
/// of this layer) cannot return the value the radio actually holds.
#[test]
fn settings_block_matches_the_radio() -> TestResult {
    use kenwood_thd75::types::{
        AltitudeRainUnit, AutoPowerOff, BeatShift, Language, SpeedDistanceUnit, TemperatureUnit,
    };

    let image = load_dump()?;
    let settings = image.settings();

    // Beep / voice guidance.
    assert!(settings.key_beep(), "key beep was enabled on the radio");
    assert_eq!(settings.beep_volume(), 0, "beep volume is VOL Link (0)");
    assert_eq!(settings.announce(), 0, "voice announce is Off");
    assert_eq!(
        settings.voice_volume(),
        0,
        "voice announce volume is VOL Link (0)"
    );
    assert_eq!(settings.voice_speed(), 0, "voice guidance speed is Speed 1");

    // Backlight.
    assert_eq!(settings.backlight_control(), 1, "backlight control is On");
    assert_eq!(settings.backlight_timer(), 13, "backlight timer is 13 s");

    // Lock configuration (shared bit byte 0x1084 holds 0x03).
    assert!(settings.key_lock(), "key-lock checkbox was ticked");
    assert!(
        settings.frequency_lock(),
        "frequency-lock checkbox was ticked"
    );
    assert!(!settings.volume_lock(), "volume lock was off");
    assert!(!settings.aprs_lock_frequency(), "APRS frequency lock off");
    assert!(!settings.aprs_lock_ptt(), "APRS PTT lock off");
    assert!(!settings.aprs_lock_key(), "APRS key lock off");

    // TX.
    assert!(!settings.tx_inhibit(), "TX inhibit was off");
    assert_eq!(
        settings.timeout_timer(),
        10,
        "TX timeout index 10 = 10.0 minutes"
    );
    assert_eq!(settings.beat_shift(), BeatShift::Type1, "beat shift Type 1");
    assert_eq!(
        settings.mic_sensitivity(),
        1,
        "mic sensitivity Medium (0=High on the D75)"
    );

    // RX filters.
    assert_eq!(settings.ssb_high_cut(), 1, "SSB high cut 2.4 kHz");
    assert_eq!(settings.cw_width(), 2, "CW width 1.0 kHz");
    assert_eq!(settings.am_high_cut(), 2, "AM high cut 6.0 kHz");
    assert_eq!(settings.cw_pitch(), 4, "CW pitch index");

    // Scan.
    assert_eq!(settings.scan_resume(), 1, "analog scan resume Carrier");
    assert_eq!(
        settings.digital_scan_resume(),
        2,
        "digital scan resume Seek"
    );
    assert_eq!(settings.scan_restart_time(), 8, "time restart 8 s");
    assert_eq!(settings.scan_restart_carrier(), 4, "carrier restart 4 s");

    // VOX.
    assert!(!settings.vox_enabled(), "VOX was off");
    assert_eq!(settings.vox_gain(), 4, "VOX gain");
    assert_eq!(settings.vox_delay(), 1, "VOX delay index 1 = 500 ms");
    assert!(!settings.vox_tx_on_busy(), "VOX TX-on-busy was off");

    // DTMF.
    assert_eq!(settings.dtmf_speed(), 1, "DTMF speed 100 ms");
    assert_eq!(settings.dtmf_pause_time(), 2, "DTMF pause 500 ms");
    assert!(!settings.dtmf_tx_hold(), "DTMF TX hold was off");

    // Repeater.
    assert!(settings.repeater_auto_offset(), "auto offset was on");
    assert_eq!(settings.repeater_call_key(), 0, "CALL key function CALL");

    // PF keys.
    assert_eq!(settings.pf_key1(), 10, "PF1 assigned to Balance");
    assert_eq!(settings.pf_key2(), 11, "PF2 assigned to GPS");

    // Audio routing.
    assert_eq!(settings.emr_volume_level(), 25, "EMR volume level 25");
    assert_eq!(settings.auto_mute_return_time(), 3, "auto mute return 3");

    // Interfaces.
    assert_eq!(settings.gps_bt_interface(), 1, "GPS PC output Bluetooth");
    assert_eq!(settings.aprs_usb_mode(), 1, "APRS PC output Bluetooth");

    // Bluetooth.
    assert!(settings.bluetooth(), "Bluetooth was on");
    assert!(settings.bt_auto_connect(), "BT auto-connect was on");

    // Power.
    assert_eq!(settings.battery_saver(), 0, "battery saver Off");
    assert_eq!(
        settings.auto_power_off(),
        AutoPowerOff::Off,
        "auto power off disabled"
    );

    // Units / language / message.
    let units = settings.display_units();
    assert_eq!(
        units.speed_distance,
        SpeedDistanceUnit::MilesPerHour,
        "US units: speed in mph"
    );
    assert_eq!(
        units.altitude_rain,
        AltitudeRainUnit::FeetInch,
        "US units: altitude in feet"
    );
    assert_eq!(
        units.temperature,
        TemperatureUnit::Fahrenheit,
        "US units: temperature in F"
    );
    assert_eq!(settings.language(), Language::English, "display language");
    assert_eq!(
        settings.power_on_message(),
        "",
        "no power-on message programmed"
    );
    Ok(())
}

/// Known-true D-STAR MY-callsign record, read off the physical radio.
///
/// The MY callsign lives in `dv.MyCallsignDvGatewayList` at 0x1CA8
/// (8-byte space-padded callsign + 4-byte memo, stride 12). The
/// source radio has its owner's callsign and a "D75A" memo in record
/// 0, with the selector pointing at that record.
#[test]
fn dstar_my_callsign_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let dstar = image.dstar();

    assert_eq!(dstar.my_callsign_select(), 0, "record 0 is active");
    assert_eq!(dstar.my_callsign(), "KQ4NIT", "MY callsign");
    assert_eq!(
        dstar.my_callsign_memo(0).as_deref(),
        Some("D75A"),
        "record 0 memo"
    );
    assert_eq!(
        dstar.my_callsign_entry(1).as_deref(),
        Some(""),
        "record 1 is unprogrammed"
    );
    Ok(())
}
