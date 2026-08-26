//! Accessibility output self-check.
//!
//! The `check` subcommand runs without connecting to a radio. It
//! exercises every `output::*` formatter with representative inputs,
//! runs the accessibility lint on every result, and prints a report.
//!
//! A blind ham can run `thd75-repl check` and read the report via
//! their screen reader to verify the mechanically checked output contract.

use crate::{help_text, lint, output};
use dstar_gateway_core::{Callsign, Module, ReflectorCallsign};
use kenwood_thd75::types::{
    Band, BatteryLevel, BeaconMode, FirmwareIdentity, GpsSettings, PowerLevel, RadioClock,
    RadioDateTime, RadioModel, TncDataBand, TncMode, UsbAudioOutput,
};

/// One entry in the coverage table: a human-readable source name and
/// a generator that produces the representative outputs for that
/// source.
type Generator = fn() -> Vec<String>;

const COVERAGE: &[(&str, Generator)] = &[
    ("output::frequency", gen_frequency),
    ("output::tuned_to", gen_tuned_to),
    ("output::stepped_up", gen_stepped_up),
    ("output::stepped_down", gen_stepped_down),
    ("output::mode_read", gen_mode_read),
    ("output::mode_set", gen_mode_set),
    ("output::power_read", gen_power_read),
    ("output::power_set", gen_power_set),
    ("output::tnc_mode", gen_tnc_mode),
    ("output::beacon_mode", gen_beacon_mode),
    ("output::squelch_read", gen_squelch_read),
    ("output::squelch_set", gen_squelch_set),
    ("output::smeter", gen_smeter),
    ("output::battery", gen_battery),
    ("output::radio_model", gen_radio_model),
    ("output::firmware_version", gen_firmware_version),
    ("output::clock", gen_clock),
    ("output::key_lock", gen_key_lock),
    ("output::bluetooth", gen_bluetooth),
    ("output::dual_band", gen_dual_band),
    ("output::attenuator", gen_attenuator),
    ("output::vox", gen_vox),
    ("output::fm_radio", gen_fm_radio),
    ("output::channel_read", gen_channel_read),
    ("output::channels_reading", gen_channels_reading),
    ("output::channels_summary", gen_channels_summary),
    ("output::tx_offset", gen_tx_offset),
    ("output::step_size", gen_step_size),
    ("output::gps_settings", gen_gps_settings),
    ("output::urcall_read", gen_urcall_read),
    ("output::urcall_set", gen_urcall_set),
    ("output::reflector_connected", gen_reflector_connected),
    ("output::operation_band", gen_operation_band),
    ("output::usb_output", gen_usb_output),
    ("output::error", gen_error),
    ("output::warning", gen_warning),
    ("output::aprs_events", gen_aprs_events),
    ("output::dstar_events", gen_dstar_events),
    ("output::startup", gen_startup),
    ("help_text::for_command", gen_help_text),
    ("help_text::mode_help", gen_mode_help),
];

fn gen_frequency() -> Vec<String> {
    let mut v = Vec::new();
    for band in [Band::A, Band::B] {
        for hz in [0u32, 146_520_000, 446_000_000, 1_200_000_000, 1_300_000_000] {
            v.push(output::frequency(band, hz));
        }
    }
    v
}

fn gen_tuned_to() -> Vec<String> {
    vec![
        output::tuned_to(Band::A, 146_520_000),
        output::tuned_to(Band::B, 446_000_000),
    ]
}

fn gen_stepped_up() -> Vec<String> {
    vec![output::stepped_up(Band::A, 146_525_000)]
}

fn gen_stepped_down() -> Vec<String> {
    vec![output::stepped_down(Band::A, 146_515_000)]
}

fn gen_mode_read() -> Vec<String> {
    ["FM", "NFM", "AM", "DV", "LSB", "USB", "CW"]
        .iter()
        .map(|m| output::mode_read(Band::A, m))
        .collect()
}

fn gen_mode_set() -> Vec<String> {
    ["FM", "DV"]
        .iter()
        .map(|m| output::mode_set(Band::A, m))
        .collect()
}

fn gen_power_read() -> Vec<String> {
    [
        PowerLevel::High,
        PowerLevel::Medium,
        PowerLevel::Low,
        PowerLevel::ExtraLow,
    ]
    .iter()
    .map(|p| output::power_read(Band::A, *p))
    .collect()
}

fn gen_tnc_mode() -> Vec<String> {
    let mut v = Vec::new();
    for mode in [TncMode::Off, TncMode::Aprs, TncMode::Kiss] {
        for data_band in [TncDataBand::A, TncDataBand::B] {
            v.push(output::tnc_mode_read(mode, data_band));
            v.push(output::tnc_mode_set(mode, data_band));
        }
    }
    v
}

fn gen_beacon_mode() -> Vec<String> {
    let mut v = Vec::new();
    for mode in [
        BeaconMode::Manual,
        BeaconMode::Ptt,
        BeaconMode::Auto,
        BeaconMode::SmartBeaconing,
    ] {
        v.push(output::beacon_mode_read(mode));
        v.push(output::beacon_mode_set(mode));
    }
    v
}

fn gen_power_set() -> Vec<String> {
    [PowerLevel::High, PowerLevel::Medium]
        .iter()
        .map(|p| output::power_set(Band::B, *p))
        .collect()
}

fn gen_squelch_read() -> Vec<String> {
    (0u8..=5)
        .map(|l| output::squelch_read(Band::A, l))
        .collect()
}

fn gen_squelch_set() -> Vec<String> {
    (0u8..=5).map(|l| output::squelch_set(Band::A, l)).collect()
}

fn gen_smeter() -> Vec<String> {
    ["S0", "S1", "S3", "S5", "S7", "S9"]
        .iter()
        .map(|r| output::smeter(Band::A, r))
        .collect()
}

fn gen_battery() -> Vec<String> {
    [
        BatteryLevel::Empty,
        BatteryLevel::OneThird,
        BatteryLevel::TwoThirds,
        BatteryLevel::Full,
        BatteryLevel::Charging,
        BatteryLevel::Unidentified5,
    ]
    .iter()
    .map(|l| output::battery(*l))
    .collect()
}

fn gen_radio_model() -> Vec<String> {
    vec![output::radio_model(RadioModel::ThD75)]
}

fn gen_firmware_version() -> Vec<String> {
    let firmware = checked_standard_firmware();
    vec![output::firmware_version(&firmware)]
}

fn checked_standard_firmware() -> FirmwareIdentity {
    FirmwareIdentity::new("1.03")
        .unwrap_or_else(|error| unreachable!("static firmware identity is valid: {error}"))
}

fn gen_clock() -> Vec<String> {
    let Ok(value) = RadioDateTime::new(2026, 4, 10, 14, 32, 7) else {
        return vec![output::clock(RadioClock::Unavailable)];
    };
    vec![
        output::clock(value.into()),
        output::clock(RadioClock::Unavailable),
    ]
}

fn gen_key_lock() -> Vec<String> {
    vec![output::key_lock(true), output::key_lock(false)]
}

fn gen_bluetooth() -> Vec<String> {
    vec![output::bluetooth(true), output::bluetooth(false)]
}

fn gen_dual_band() -> Vec<String> {
    vec![output::dual_band(true), output::dual_band(false)]
}

fn gen_attenuator() -> Vec<String> {
    vec![
        output::attenuator(Band::A, true),
        output::attenuator(Band::B, false),
    ]
}

fn gen_vox() -> Vec<String> {
    vec![
        output::vox(true),
        output::vox(false),
        output::vox_set(true),
        output::vox_gain_read(5),
        output::vox_gain_set(7),
        output::vox_delay_read(3),
        output::vox_delay_set(4),
    ]
}

fn gen_fm_radio() -> Vec<String> {
    vec![
        output::fm_radio(true),
        output::fm_radio(false),
        output::fm_radio_set(true),
    ]
}

fn gen_channel_read() -> Vec<String> {
    vec![
        output::channel_read(0, 146_520_000),
        output::channel_read(999, 446_000_000),
    ]
}

fn gen_channels_reading() -> Vec<String> {
    vec![
        output::channels_reading(0, 19),
        output::channels_reading(100, 199),
    ]
}

fn gen_channels_summary() -> Vec<String> {
    vec![
        output::channels_summary(0),
        output::channels_summary(1),
        output::channels_summary(42),
    ]
}

fn gen_tx_offset() -> Vec<String> {
    vec![
        output::tx_offset(Band::A, 600_000),
        output::tx_offset(Band::B, 5_000_000),
    ]
}

fn gen_step_size() -> Vec<String> {
    vec![
        output::step_size_read(Band::A, "25 kilohertz"),
        output::step_size_set(Band::B, "12.5 kilohertz"),
    ]
}

fn gen_gps_settings() -> Vec<String> {
    vec![
        output::gps_settings(GpsSettings::new(true, true)),
        output::gps_settings(GpsSettings::new(false, false)),
        output::gps_settings(GpsSettings::new(true, false)),
        output::gps_settings(GpsSettings::new(false, true)),
    ]
}

fn gen_urcall_read() -> Vec<String> {
    vec![
        output::urcall_read("W1AW", ""),
        output::urcall_read("W1AW", "P"),
    ]
}

fn gen_urcall_set() -> Vec<String> {
    vec![output::urcall_set("W1AW")]
}

fn gen_reflector_connected() -> Vec<String> {
    vec![
        output::reflector_connected("REF030", 'C'),
        output::reflector_disconnected().to_string(),
    ]
}

fn gen_operation_band() -> Vec<String> {
    vec![
        output::operation_band_read(Band::A),
        output::operation_band_read(Band::B),
        output::operation_band_set(Band::A),
        output::operation_band_set(Band::B),
    ]
}

fn gen_usb_output() -> Vec<String> {
    vec![
        output::usb_output_read(UsbAudioOutput::Audio),
        output::usb_output_read(UsbAudioOutput::IntermediateFrequency),
        output::usb_output_read(UsbAudioOutput::Detect),
        output::usb_output_set(UsbAudioOutput::Audio),
        output::usb_output_set(UsbAudioOutput::IntermediateFrequency),
        output::usb_output_set(UsbAudioOutput::Detect),
    ]
}

fn gen_error() -> Vec<String> {
    vec![
        output::error("invalid frequency"),
        output::error("connection lost"),
    ]
}

fn gen_warning() -> Vec<String> {
    vec![output::warning("could not detect local time zone")]
}

fn gen_aprs_events() -> Vec<String> {
    vec![
        output::aprs_station_heard("W1AW"),
        output::aprs_message_received("W1AW", "hi"),
        output::aprs_message_delivered("1"),
        output::aprs_message_rejected("2"),
        output::aprs_message_expired("3"),
        output::aprs_position("W1AW", 35.3, -82.46),
        output::aprs_weather("W1AW"),
        output::aprs_digipeated("W1AW"),
        output::aprs_query_responded("W1AW"),
        output::aprs_raw_packet("W1AW"),
        output::aprs_mode_active().to_string(),
        output::aprs_is_connected().to_string(),
        output::aprs_is_incoming("W1AW>APRS:hello"),
        output::aprs_station_entry("W1AW", Some((35.3, -82.46)), 12, "2 minutes"),
        output::aprs_station_entry("W1AW", None, 1, "15 seconds"),
        output::stations_summary(1),
        output::stations_summary(5),
    ]
}

fn gen_dstar_events() -> Vec<String> {
    let reflector = ReflectorCallsign::try_from_str("REF030")
        .unwrap_or_else(|error| unreachable!("static reflector is valid: {error}"));
    let destination = Callsign::from_wire_bytes(*b"W1AW    ");
    vec![
        output::dstar_voice_start("W1AW", "P", "CQCQCQ"),
        output::dstar_voice_start("W1AW", "", "W9ABC"),
        output::dstar_voice_end().to_string(),
        output::dstar_voice_lost().to_string(),
        output::dstar_text_message("hello"),
        output::dstar_gps(""),
        output::dstar_gps("$GPGGA,123519,4807.038,N"),
        output::dstar_station_heard("W1AW"),
        output::dstar_command_cq().to_string(),
        output::dstar_command_echo().to_string(),
        output::dstar_command_unlink().to_string(),
        output::dstar_command_info().to_string(),
        output::dstar_command_link(&reflector, Module::C),
        output::dstar_command_callsign(&destination),
        output::dstar_modem_status(5, false),
        output::dstar_modem_status(0, true),
        output::reflector_event_connected().to_string(),
        output::reflector_event_rejected().to_string(),
        output::reflector_event_disconnected().to_string(),
        output::reflector_event_voice_start("W1AW", "", "CQCQCQ"),
        output::reflector_event_voice_start("W1AW", "P", "CQCQCQ"),
        output::reflector_event_voice_end(0, 0),
        output::reflector_event_voice_end(1, 20),
        output::reflector_event_voice_end(42, 840),
    ]
}

fn gen_startup() -> Vec<String> {
    let firmware = checked_standard_firmware();
    vec![
        output::startup_banner("0.1.0"),
        output::connected_via("/dev/cu.usbmodem1234"),
        output::startup_identified(RadioModel::ThD75, &firmware),
        output::type_help_hint().to_string(),
        output::goodbye().to_string(),
    ]
}

fn gen_mode_help() -> Vec<String> {
    let mut v = Vec::new();
    for blob in [
        help_text::CAT_MODE_HELP,
        help_text::MMDVM_MODE_HELP,
        help_text::APRS_MODE_HELP,
        help_text::DSTAR_MODE_HELP,
    ] {
        for line in blob.lines() {
            v.push(line.to_string());
        }
    }
    v
}

fn gen_help_text() -> Vec<String> {
    let mut v = Vec::new();
    for cmd in help_text::ALL_COMMANDS {
        if let Some(text) = help_text::for_command(cmd) {
            for line in text.lines() {
                v.push(line.to_string());
            }
        }
    }
    v
}

/// Run the output check, print the report, return the exit code.
///
/// Returns 0 if all rules pass, 1 if any violation is found. Prints
/// the full report to stdout.
#[must_use]
pub fn run() -> i32 {
    println!(
        "Accessibility output check, thd75-repl version {}.",
        env!("CARGO_PKG_VERSION")
    );
    println!("Reference: WCAG 2.1 Level AA.");
    println!("Reference: Section 508 of the US Rehabilitation Act.");
    println!("Reference: CHI 2021 CLI accessibility paper.");
    println!("Reference: EN 301 549 version 3.2.1.");
    println!("Reference: ISO 9241-171.");
    println!("Reference: ITU-T Recommendation F.790.");
    println!("Reference: BRLTTY compatibility.");
    println!("Reference: Handi-Ham Program recommendations.");
    println!("Reference: ARRL accessibility resources.");
    println!();

    // Collect per-rule counts and violations.
    let mut rule_violations: std::collections::BTreeMap<lint::Rule, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    let mut rule_string_counts: std::collections::BTreeMap<lint::Rule, usize> =
        std::collections::BTreeMap::new();
    let mut total_strings: usize = 0;

    for (source, generator) in COVERAGE {
        let outputs = generator();
        total_strings += outputs.len();
        for s in &outputs {
            if let Err(violations) = lint::check_output(s) {
                for v in violations {
                    rule_violations
                        .entry(v.rule)
                        .or_default()
                        .push(((*source).to_string(), v.message));
                }
            }
        }
    }

    let all_rules = [
        lint::Rule::AsciiOnly,
        lint::Rule::LineLength,
        lint::Rule::NoAnsi,
        lint::Rule::ErrorPrefix,
        lint::Rule::WarningPrefix,
        lint::Rule::ConfirmOnSet,
        lint::Rule::ListCountSummary,
        lint::Rule::LabelColonValue,
        lint::Rule::BooleanWords,
        lint::Rule::NoNakedPrint,
        lint::Rule::NoCursorMoves,
        lint::Rule::UnitsSpelledOut,
        lint::Rule::NoAdHocTimestamps,
        lint::Rule::StdoutStderrSeparation,
    ];
    for rule in all_rules {
        let _ = rule_string_counts.insert(rule, total_strings);
    }

    let mut failed = 0usize;
    for rule in all_rules {
        let count = rule_string_counts.get(&rule).copied().unwrap_or(0);
        if let Some(vs) = rule_violations.get(&rule) {
            failed += 1;
            println!(
                "Rule {} {}: FAILED, {} violations, {count} strings checked.",
                rule.id(),
                rule.description(),
                vs.len()
            );
            for (source, message) in vs.iter().take(5) {
                println!("  Source: {source}. {message}");
            }
        } else {
            println!(
                "Rule {} {}: passed, {count} strings checked.",
                rule.id(),
                rule.description()
            );
        }
    }

    println!();
    if failed == 0 {
        println!(
            "All 14 rules passed. {total_strings} strings checked from {} sources.",
            COVERAGE.len()
        );
        0
    } else {
        println!(
            "{failed} of 14 rules failed. {total_strings} strings checked from {} sources.",
            COVERAGE.len()
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_exits_zero_on_clean_build() {
        // This test calls `run()` which prints to stdout as a side
        // effect. We only check the return code, not the output.
        let code = run();
        assert_eq!(code, 0, "check command reported violations");
    }

    #[test]
    fn every_generator_produces_at_least_one_string() {
        for (name, generator) in COVERAGE {
            let outputs = generator();
            assert!(!outputs.is_empty(), "generator {name} produced no outputs");
        }
    }
}
