//! Pure format functions for every user-facing REPL string.
//!
//! Every function returns a `String` (or `&'static str`) - zero I/O,
//! zero async, zero radio access. Testing happens directly on the
//! returned strings plus the accessibility lint.
//!
//! ## Accessibility rules
//!
//! Strings are designed for screen-reader output (blind operators) and
//! fixed-width terminals:
//!
//! - Label-colon-value format (screen readers parse "Label: value" well)
//! - Natural-language units (megahertz, watts, hertz — not symbols)
//! - Booleans as words (on/off, not true/false or checkmarks)
//! - Lines under 80 characters (no wrapping in standard terminals)
//! - ASCII printable only (no box-drawing or symbols)

use kenwood_thd75::types::{Band, BatteryLevel, PowerLevel};

/// Human-readable band name. Matches the pre-extraction helper which
/// returned "A" for `Band::A` and "B" for every other variant.
#[must_use]
pub fn band_name(band: Band) -> &'static str {
    if band == Band::A { "A" } else { "B" }
}

/// Format a frequency in megahertz for natural speech output.
///
/// Trailing zeros are stripped, but a trailing decimal with a single
/// zero is kept so `146.5 megahertz` reads cleanly. Values below 1 Hz
/// render as `0 megahertz`.
#[must_use]
pub fn freq_mhz(hz: u32) -> String {
    let mhz = f64::from(hz) / 1_000_000.0;
    let s = format!("{mhz:.6}");
    let s = s.trim_end_matches('0');
    let s = if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_string()
    };
    format!("{s} megahertz")
}

/// `Band {A|B} frequency: {f} megahertz`.
#[must_use]
pub fn frequency(band: Band, hz: u32) -> String {
    format!("Band {} frequency: {}", band_name(band), freq_mhz(hz))
}

/// `Band {A|B} tuned to {f} megahertz`.
#[must_use]
pub fn tuned_to(band: Band, hz: u32) -> String {
    format!("Band {} tuned to {}", band_name(band), freq_mhz(hz))
}

/// `Band {A|B} stepped up to {f} megahertz`.
#[must_use]
pub fn stepped_up(band: Band, hz: u32) -> String {
    format!("Band {} stepped up to {}", band_name(band), freq_mhz(hz))
}

/// `Band {A|B} stepped down to {f} megahertz`.
#[must_use]
pub fn stepped_down(band: Band, hz: u32) -> String {
    format!("Band {} stepped down to {}", band_name(band), freq_mhz(hz))
}

/// `Band {A|B} step size: {step}`.
#[must_use]
pub fn step_size_read(band: Band, step_display: &str) -> String {
    format!("Band {} step size: {step_display}", band_name(band))
}

/// `Band {A|B} step size set to {step}`.
#[must_use]
pub fn step_size_set(band: Band, step_display: &str) -> String {
    format!("Band {} step size set to {step_display}", band_name(band))
}

/// `Band {A|B} transmit offset: {hz} hertz`.
#[must_use]
pub fn tx_offset(band: Band, hz: u32) -> String {
    format!("Band {} transmit offset: {hz} hertz", band_name(band))
}

/// `Band {A|B} mode: {mode}` (for VFO readout - the full mode name
/// comes from `kenwood_thd75::types::Mode::fmt`).
#[must_use]
pub fn mode_read(band: Band, mode_display: &str) -> String {
    format!("Band {} mode: {mode_display}", band_name(band))
}

/// `Error: {message}` - the canonical error prefix.
#[must_use]
pub fn error(e: impl std::fmt::Display) -> String {
    format!("Error: {e}")
}

/// `Warning: {message}` - the canonical warning prefix.
#[must_use]
pub fn warning(e: impl std::fmt::Display) -> String {
    format!("Warning: {e}")
}

/// `Band {A|B} mode set to {mode}`.
#[must_use]
pub fn mode_set(band: Band, mode_display: &str) -> String {
    format!("Band {} mode set to {mode_display}", band_name(band))
}

/// Human-readable power level name with watts in full.
#[must_use]
pub const fn power_level_display(level: PowerLevel) -> &'static str {
    match level {
        PowerLevel::High => "high, 5 watts",
        PowerLevel::Medium => "medium, 2 watts",
        PowerLevel::Low => "low, half watt",
        PowerLevel::ExtraLow => "extra-low, 50 milliwatts",
    }
}

/// `Band {A|B} power: {level}`.
#[must_use]
pub fn power_read(band: Band, level: PowerLevel) -> String {
    format!(
        "Band {} power: {}",
        band_name(band),
        power_level_display(level)
    )
}

/// `Band {A|B} power set to {level}`.
#[must_use]
pub fn power_set(band: Band, level: PowerLevel) -> String {
    format!(
        "Band {} power set to {}",
        band_name(band),
        power_level_display(level)
    )
}

/// `Band {A|B} squelch level: {level}` (0-5).
#[must_use]
pub fn squelch_read(band: Band, level: u8) -> String {
    format!("Band {} squelch level: {level}", band_name(band))
}

/// `Band {A|B} squelch set to {level}`.
#[must_use]
pub fn squelch_set(band: Band, level: u8) -> String {
    format!("Band {} squelch set to {level}", band_name(band))
}

/// `Band {A|B} S-meter: {reading}`.
#[must_use]
pub fn smeter(band: Band, reading_display: &str) -> String {
    format!("Band {} S-meter: {reading_display}", band_name(band))
}

/// Human-readable battery level for screen reader speech.
#[must_use]
pub const fn battery_level_display(level: BatteryLevel) -> &'static str {
    match level {
        BatteryLevel::Empty => "empty",
        BatteryLevel::OneThird => "one third",
        BatteryLevel::TwoThirds => "two thirds",
        BatteryLevel::Full => "full",
        BatteryLevel::Charging => "charging",
    }
}

/// Render an `on`/`off` word from a bool.
#[must_use]
pub const fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// `Radio model: {model}`.
#[must_use]
pub fn radio_model(model: impl std::fmt::Display) -> String {
    format!("Radio model: {model}")
}

/// `Firmware version: {version}`.
#[must_use]
pub fn firmware_version(version: impl std::fmt::Display) -> String {
    format!("Firmware version: {version}")
}

/// `Battery level: {level}`.
#[must_use]
pub fn battery(level: BatteryLevel) -> String {
    format!("Battery level: {}", battery_level_display(level))
}

/// `Radio clock: {time}`.
#[must_use]
pub fn clock(time: impl std::fmt::Display) -> String {
    format!("Radio clock: {time}")
}

/// `Key lock: {on|off}`.
#[must_use]
pub fn key_lock(locked: bool) -> String {
    format!("Key lock: {}", on_off(locked))
}

/// `Bluetooth: {on|off}`.
#[must_use]
pub fn bluetooth(enabled: bool) -> String {
    format!("Bluetooth: {}", on_off(enabled))
}

/// `Dual band: {on|off}`.
#[must_use]
pub fn dual_band(enabled: bool) -> String {
    format!("Dual band: {}", on_off(enabled))
}

/// `Band {A|B} attenuator: {on|off}`.
#[must_use]
pub fn attenuator(band: Band, enabled: bool) -> String {
    format!("Band {} attenuator: {}", band_name(band), on_off(enabled))
}

/// `VOX: {on|off}`.
#[must_use]
pub fn vox(enabled: bool) -> String {
    format!("VOX: {}", on_off(enabled))
}

/// `VOX set to {on|off}`.
#[must_use]
pub fn vox_set(enabled: bool) -> String {
    format!("VOX set to {}", on_off(enabled))
}

/// `VOX gain: {level}`.
#[must_use]
pub fn vox_gain_read(level: u8) -> String {
    format!("VOX gain: {level}")
}

/// `VOX gain set to {level}`.
#[must_use]
pub fn vox_gain_set(level: u8) -> String {
    format!("VOX gain set to {level}")
}

/// `VOX delay: {level}`.
#[must_use]
pub fn vox_delay_read(level: u8) -> String {
    format!("VOX delay: {level}")
}

/// `VOX delay set to {level}`.
#[must_use]
pub fn vox_delay_set(level: u8) -> String {
    format!("VOX delay set to {level}")
}

/// `FM radio: {on|off}`.
#[must_use]
pub fn fm_radio(enabled: bool) -> String {
    format!("FM radio: {}", on_off(enabled))
}

/// `FM radio set to {on|off}`.
#[must_use]
pub fn fm_radio_set(enabled: bool) -> String {
    format!("FM radio set to {}", on_off(enabled))
}

/// `Channel {n}: {f} megahertz`.
#[must_use]
pub fn channel_read(number: u16, hz: u32) -> String {
    format!("Channel {number}: {}", freq_mhz(hz))
}

/// `Reading channels {start} through {end}, please wait.`
#[must_use]
pub fn channels_reading(start: u16, end_inclusive: u16) -> String {
    format!("Reading channels {start} through {end_inclusive}, please wait.")
}

/// `{count} programmed channels found.` or `No programmed channels in
/// that range.`.
#[must_use]
pub fn channels_summary(count: usize) -> String {
    if count == 0 {
        "No programmed channels in that range.".to_string()
    } else {
        format!("{count} programmed channels found.")
    }
}

/// `GPS: {on|off}, PC output: {on|off}`.
#[must_use]
pub fn gps_config(gps_on: bool, pc_on: bool) -> String {
    format!("GPS: {}, PC output: {}", on_off(gps_on), on_off(pc_on))
}

/// `Destination callsign: {call}` or `... suffix {suffix}`.
#[must_use]
pub fn urcall_read(call: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        format!("Destination callsign: {call}")
    } else {
        format!("Destination callsign: {call} suffix {suffix}")
    }
}

/// `Destination callsign set to {call}`.
#[must_use]
pub fn urcall_set(call: &str) -> String {
    format!("Destination callsign set to {call}")
}

/// `Destination set to CQCQCQ`.
#[must_use]
pub const fn cq_set() -> &'static str {
    "Destination set to CQCQCQ"
}

/// `Connected to {name} module {module}`.
#[must_use]
pub fn reflector_connected(name: &str, module: char) -> String {
    format!("Connected to {name} module {module}")
}

/// `Disconnected from reflector`.
#[must_use]
pub const fn reflector_disconnected() -> &'static str {
    "Disconnected from reflector"
}

// ---------------------------------------------------------------------------
// APRS mode output
// ---------------------------------------------------------------------------

/// `APRS station heard: {callsign}.`
#[must_use]
pub fn aprs_station_heard(callsign: &str) -> String {
    format!("APRS station heard: {callsign}.")
}

/// `APRS message received for {addressee}: {text}.`
#[must_use]
pub fn aprs_message_received(addressee: &str, text: &str) -> String {
    format!("APRS message received for {addressee}: {text}.")
}

/// `APRS message delivered, ID {id}.`
#[must_use]
pub fn aprs_message_delivered(id: &str) -> String {
    format!("APRS message delivered, ID {id}.")
}

/// `APRS message rejected by remote station, ID {id}.`
#[must_use]
pub fn aprs_message_rejected(id: &str) -> String {
    format!("APRS message rejected by remote station, ID {id}.")
}

/// `APRS message expired after all retries, ID {id}.`
#[must_use]
pub fn aprs_message_expired(id: &str) -> String {
    format!("APRS message expired after all retries, ID {id}.")
}

/// `APRS position from {source}: latitude {lat}, longitude {lon}.`
#[must_use]
pub fn aprs_position(source: &str, lat: f64, lon: f64) -> String {
    format!("APRS position from {source}: latitude {lat:.4}, longitude {lon:.4}.")
}

/// `APRS weather report from {source}.`
#[must_use]
pub fn aprs_weather(source: &str) -> String {
    format!("APRS weather report from {source}.")
}

/// `APRS packet relayed from {source}.`
#[must_use]
pub fn aprs_digipeated(source: &str) -> String {
    format!("APRS packet relayed from {source}.")
}

/// `APRS position query from {to}, responded with beacon.`
///
/// Note: the previous version used an em dash (U+2014) between `{to}`
/// and `responded`. This ASCII-only rewrite replaces it with a comma
/// so screen readers render the line cleanly.
#[must_use]
pub fn aprs_query_responded(to: &str) -> String {
    format!("APRS position query from {to}, responded with beacon.")
}

/// `APRS packet from {source}.`
#[must_use]
pub fn aprs_raw_packet(source: &str) -> String {
    format!("APRS packet from {source}.")
}

/// `APRS mode active. Type aprs stop to exit.`
#[must_use]
pub const fn aprs_mode_active() -> &'static str {
    "APRS mode active. Type aprs stop to exit."
}

/// `APRS-IS connected. Forwarding RF to internet. Press Ctrl-C to stop.`
///
/// The original line used a Unicode left-right arrow between `RF` and
/// `internet`. The ASCII rewrite spells the relationship out so screen
/// readers can announce it.
#[must_use]
pub const fn aprs_is_connected() -> &'static str {
    "APRS-IS connected. Forwarding RF to internet. Press Ctrl-C to stop."
}

/// `APRS-IS incoming: {line}`.
///
/// Replaces the previous `IS -> {line}` (which used a Unicode right
/// arrow) with the spoken form `APRS-IS incoming:`.
#[must_use]
pub fn aprs_is_incoming(line: &str) -> String {
    format!("APRS-IS incoming: {line}")
}

/// `Station {call}{pos}, {n} packets, heard {elapsed} ago.`
#[must_use]
pub fn aprs_station_entry(
    callsign: &str,
    position: Option<(f64, f64)>,
    packet_count: u32,
    elapsed_display: &str,
) -> String {
    let pos = match position {
        Some((lat, lon)) => format!(" at {lat:.4}, {lon:.4}"),
        None => String::new(),
    };
    format!("Station {callsign}{pos}, {packet_count} packets, heard {elapsed_display} ago.")
}

/// `{count} stations heard.`
#[must_use]
pub fn aprs_stations_summary(count: usize) -> String {
    format!("{count} stations heard.")
}

// ---------------------------------------------------------------------------
// D-STAR mode output
// ---------------------------------------------------------------------------

/// `D-STAR voice from {call}{/suffix}, to {ur}.`
#[must_use]
pub fn dstar_voice_start(my_call: &str, my_suffix: &str, ur_call: &str) -> String {
    let suffix_part = if my_suffix.trim().is_empty() {
        String::new()
    } else {
        format!(" /{}", my_suffix.trim())
    };
    format!(
        "D-STAR voice from {}{suffix_part}, to {}.",
        my_call.trim(),
        ur_call.trim()
    )
}

/// `D-STAR voice transmission ended.`
#[must_use]
pub const fn dstar_voice_end() -> &'static str {
    "D-STAR voice transmission ended."
}

/// `D-STAR voice signal lost, no clean end of transmission.`
#[must_use]
pub const fn dstar_voice_lost() -> &'static str {
    "D-STAR voice signal lost, no clean end of transmission."
}

/// `D-STAR message: "{text}"`
#[must_use]
pub fn dstar_text_message(text: &str) -> String {
    format!("D-STAR message: \"{text}\"")
}

/// `D-STAR GPS data: {text}.` or `D-STAR GPS position data received.`
#[must_use]
pub fn dstar_gps(text: &str) -> String {
    if text.trim().is_empty() {
        "D-STAR GPS position data received.".to_string()
    } else {
        format!("D-STAR GPS data: {text}")
    }
}

/// `D-STAR station heard: {callsign}.`
#[must_use]
pub fn dstar_station_heard(callsign: &str) -> String {
    format!("D-STAR station heard: {callsign}.")
}

/// `D-STAR command: call CQ.`
#[must_use]
pub const fn dstar_command_cq() -> &'static str {
    "D-STAR command: call CQ."
}

/// `D-STAR command: echo test.`
#[must_use]
pub const fn dstar_command_echo() -> &'static str {
    "D-STAR command: echo test."
}

/// `D-STAR command: unlink reflector.`
#[must_use]
pub const fn dstar_command_unlink() -> &'static str {
    "D-STAR command: unlink reflector."
}

/// `D-STAR command: request info.`
#[must_use]
pub const fn dstar_command_info() -> &'static str {
    "D-STAR command: request info."
}

/// `D-STAR command: link to {reflector} module {module}.`
#[must_use]
pub fn dstar_command_link(reflector: &str, module: char) -> String {
    format!("D-STAR command: link to {reflector} module {module}.")
}

/// `D-STAR command: route to callsign {call}.`
#[must_use]
pub fn dstar_command_callsign(call: &str) -> String {
    format!("D-STAR command: route to callsign {call}.")
}

/// `D-STAR modem: buffer {n}, transmit {active|idle}.`
#[must_use]
pub fn dstar_modem_status(buffer: u8, tx_active: bool) -> String {
    format!(
        "D-STAR modem: buffer {buffer}, transmit {}.",
        if tx_active { "active" } else { "idle" }
    )
}

// Reflector events

/// `Reflector: connected.`
#[must_use]
pub const fn reflector_event_connected() -> &'static str {
    "Reflector: connected."
}

/// `Reflector: connection rejected.`
#[must_use]
pub const fn reflector_event_rejected() -> &'static str {
    "Reflector: connection rejected."
}

/// `Reflector: disconnected.`
#[must_use]
pub const fn reflector_event_disconnected() -> &'static str {
    "Reflector: disconnected."
}

/// `Reflector: voice from {call}{/suffix}, to {ur}.`
#[must_use]
pub fn reflector_event_voice_start(my_call: &str, my_suffix: &str, ur_call: &str) -> String {
    let suffix_part = if my_suffix.is_empty() {
        String::new()
    } else {
        format!(" /{my_suffix}")
    };
    format!("Reflector: voice from {my_call}{suffix_part}, to {ur_call}.")
}

/// `Reflector: voice transmission ended, {frames} frames in {seconds} seconds.`
///
/// When `frames == 0` (no voice frames tracked — e.g. a `VoiceEnd`
/// arrived without a preceding `VoiceStart`), falls back to the bare
/// `Reflector: voice transmission ended.` sentence so the message
/// never contains a misleading zero count.
///
/// The frame count helps blind and sighted operators alike distinguish
/// real voice traffic (tens to hundreds of frames, ~50 per second)
/// from dead-key carriers (a handful of frames), which otherwise look
/// identical in the console (announce → silence → ended).
#[must_use]
pub fn reflector_event_voice_end(frames: u32, duration_ms: u64) -> String {
    if frames == 0 {
        return "Reflector: voice transmission ended.".to_owned();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "duration is milliseconds, well under f64 precision for human-scale transmissions"
    )]
    let seconds = duration_ms as f64 / 1000.0;
    let frame_word = if frames == 1 { "frame" } else { "frames" };
    format!("Reflector: voice transmission ended, {frames} {frame_word} in {seconds:.2} seconds.")
}

// ---------------------------------------------------------------------------
// Startup / session
// ---------------------------------------------------------------------------

/// `Kenwood TH-D75 accessible radio control, version {version}.`
#[must_use]
pub fn startup_banner(version: &str) -> String {
    format!("Kenwood TH-D75 accessible radio control, version {version}.")
}

/// `Connected via {path}.`
#[must_use]
pub fn connected_via(path: &str) -> String {
    format!("Connected via {path}.")
}

/// `Goodbye.`
#[must_use]
pub const fn goodbye() -> &'static str {
    "Goodbye."
}

/// `Type help for a list of commands, or quit to exit.`
#[must_use]
pub const fn type_help_hint() -> &'static str {
    "Type help for a list of commands, or quit to exit."
}

/// `Radio model: {model}. Firmware version: {fw}.`
#[must_use]
pub fn startup_identified(model: &str, firmware: &str) -> String {
    format!("Radio model: {model}. Firmware version: {firmware}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint;
    use kenwood_thd75::types::Band;

    fn assert_lint(s: &str) {
        lint::check_output(s).unwrap_or_else(|v| {
            panic!("line {s:?} failed lint: {v:?}");
        });
    }

    #[test]
    fn freq_mhz_standard() {
        assert_eq!(freq_mhz(146_520_000), "146.52 megahertz");
        assert_eq!(freq_mhz(446_000_000), "446.0 megahertz");
        assert_eq!(freq_mhz(0), "0.0 megahertz");
    }

    #[test]
    fn freq_mhz_high_band() {
        assert_eq!(freq_mhz(1_200_000_000), "1200.0 megahertz");
        assert_eq!(freq_mhz(1_300_000_000), "1300.0 megahertz");
    }

    #[test]
    fn frequency_band_a() {
        let s = frequency(Band::A, 146_520_000);
        assert_eq!(s, "Band A frequency: 146.52 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn frequency_band_b() {
        let s = frequency(Band::B, 446_000_000);
        assert_eq!(s, "Band B frequency: 446.0 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn tuned_to_band_a() {
        let s = tuned_to(Band::A, 146_520_000);
        assert_eq!(s, "Band A tuned to 146.52 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn stepped_up_and_down() {
        let up = stepped_up(Band::A, 146_525_000);
        let down = stepped_down(Band::A, 146_515_000);
        assert_eq!(up, "Band A stepped up to 146.525 megahertz");
        assert_eq!(down, "Band A stepped down to 146.515 megahertz");
        assert_lint(&up);
        assert_lint(&down);
    }

    #[test]
    fn step_size_formats() {
        let s = step_size_read(Band::A, "25 kilohertz");
        assert_eq!(s, "Band A step size: 25 kilohertz");
        assert_lint(&s);
    }

    #[test]
    fn step_size_set_format() {
        let s = step_size_set(Band::B, "12.5 kilohertz");
        assert_eq!(s, "Band B step size set to 12.5 kilohertz");
        assert_lint(&s);
    }

    #[test]
    fn error_prefix() {
        let s = error("invalid frequency");
        assert_eq!(s, "Error: invalid frequency");
        assert_lint(&s);
    }

    #[test]
    fn warning_prefix() {
        let s = warning("could not detect time zone");
        assert_eq!(s, "Warning: could not detect time zone");
        assert_lint(&s);
    }

    #[test]
    fn tx_offset_formatted() {
        let s = tx_offset(Band::A, 600_000);
        assert_eq!(s, "Band A transmit offset: 600000 hertz");
        assert_lint(&s);
    }

    #[test]
    fn mode_read_basic() {
        let s = mode_read(Band::A, "FM");
        assert_eq!(s, "Band A mode: FM");
        assert_lint(&s);
    }

    #[test]
    fn mode_read_and_set() {
        let r = mode_read(Band::A, "FM");
        let s = mode_set(Band::B, "DV");
        assert_eq!(r, "Band A mode: FM");
        assert_eq!(s, "Band B mode set to DV");
        assert_lint(&r);
        assert_lint(&s);
    }

    #[test]
    fn power_read_all_levels() {
        use kenwood_thd75::types::PowerLevel;
        assert_eq!(
            power_read(Band::A, PowerLevel::High),
            "Band A power: high, 5 watts"
        );
        assert_eq!(
            power_read(Band::A, PowerLevel::Medium),
            "Band A power: medium, 2 watts"
        );
        assert_eq!(
            power_read(Band::A, PowerLevel::Low),
            "Band A power: low, half watt"
        );
        assert_eq!(
            power_read(Band::A, PowerLevel::ExtraLow),
            "Band A power: extra-low, 50 milliwatts"
        );
        assert_lint(&power_read(Band::A, PowerLevel::High));
        assert_lint(&power_read(Band::A, PowerLevel::ExtraLow));
    }

    #[test]
    fn power_set_lints() {
        use kenwood_thd75::types::PowerLevel;
        let s = power_set(Band::B, PowerLevel::Medium);
        assert_eq!(s, "Band B power set to medium, 2 watts");
        assert_lint(&s);
    }

    #[test]
    fn squelch_read_and_set() {
        let r = squelch_read(Band::A, 3);
        let s = squelch_set(Band::A, 5);
        assert_eq!(r, "Band A squelch level: 3");
        assert_eq!(s, "Band A squelch set to 5");
        assert_lint(&r);
        assert_lint(&s);
    }

    #[test]
    fn smeter_format() {
        let s = smeter(Band::A, "S0");
        assert_eq!(s, "Band A S-meter: S0");
        assert_lint(&s);
    }

    #[test]
    fn battery_all_levels() {
        use kenwood_thd75::types::BatteryLevel;
        let cases = [
            (BatteryLevel::Empty, "Battery level: empty"),
            (BatteryLevel::OneThird, "Battery level: one third"),
            (BatteryLevel::TwoThirds, "Battery level: two thirds"),
            (BatteryLevel::Full, "Battery level: full"),
            (BatteryLevel::Charging, "Battery level: charging"),
        ];
        for (level, expected) in cases {
            let s = battery(level);
            assert_eq!(s, expected);
            assert_lint(&s);
        }
    }

    #[test]
    fn radio_model_format() {
        let s = radio_model("TH-D75");
        assert_eq!(s, "Radio model: TH-D75");
        assert_lint(&s);
    }

    #[test]
    fn firmware_version_format() {
        let s = firmware_version("1.03");
        assert_eq!(s, "Firmware version: 1.03");
        assert_lint(&s);
    }

    #[test]
    fn clock_format() {
        let s = clock("2026-04-10 14:32:07");
        assert_eq!(s, "Radio clock: 2026-04-10 14:32:07");
        assert_lint(&s);
    }

    #[test]
    fn booleans_as_words() {
        assert_eq!(key_lock(true), "Key lock: on");
        assert_eq!(key_lock(false), "Key lock: off");
        assert_eq!(bluetooth(true), "Bluetooth: on");
        assert_eq!(dual_band(false), "Dual band: off");
        assert_lint(&key_lock(true));
        assert_lint(&bluetooth(false));
        assert_lint(&dual_band(true));
    }

    #[test]
    fn attenuator_format() {
        let s = attenuator(Band::A, true);
        assert_eq!(s, "Band A attenuator: on");
        assert_lint(&s);
        let s = attenuator(Band::B, false);
        assert_eq!(s, "Band B attenuator: off");
        assert_lint(&s);
    }

    #[test]
    fn vox_formats() {
        assert_eq!(vox(true), "VOX: on");
        assert_eq!(vox_set(false), "VOX set to off");
        assert_eq!(vox_gain_read(5), "VOX gain: 5");
        assert_eq!(vox_gain_set(7), "VOX gain set to 7");
        assert_eq!(vox_delay_read(3), "VOX delay: 3");
        assert_eq!(vox_delay_set(1), "VOX delay set to 1");
        assert_lint(&vox(true));
        assert_lint(&vox_set(true));
        assert_lint(&vox_gain_set(9));
        assert_lint(&vox_delay_read(6));
    }

    #[test]
    fn fm_radio_formats() {
        assert_eq!(fm_radio(true), "FM radio: on");
        assert_eq!(fm_radio_set(false), "FM radio set to off");
        assert_lint(&fm_radio(true));
        assert_lint(&fm_radio_set(false));
    }

    #[test]
    fn channel_read_format() {
        let s = channel_read(5, 146_520_000);
        assert_eq!(s, "Channel 5: 146.52 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn channels_reading_format() {
        let s = channels_reading(0, 19);
        assert_eq!(s, "Reading channels 0 through 19, please wait.");
        assert_lint(&s);
    }

    #[test]
    fn channels_summary_non_empty() {
        let s = channels_summary(3);
        assert_eq!(s, "3 programmed channels found.");
        assert_lint(&s);
    }

    #[test]
    fn channels_summary_empty() {
        let s = channels_summary(0);
        assert_eq!(s, "No programmed channels in that range.");
        assert_lint(&s);
    }

    #[test]
    fn gps_config_format() {
        assert_eq!(gps_config(true, true), "GPS: on, PC output: on");
        assert_eq!(gps_config(false, true), "GPS: off, PC output: on");
        assert_eq!(gps_config(true, false), "GPS: on, PC output: off");
        assert_eq!(gps_config(false, false), "GPS: off, PC output: off");
        assert_lint(&gps_config(true, true));
    }

    #[test]
    fn urcall_read_with_and_without_suffix() {
        assert_eq!(urcall_read("W1AW", ""), "Destination callsign: W1AW");
        assert_eq!(
            urcall_read("W1AW", "P"),
            "Destination callsign: W1AW suffix P"
        );
        assert_lint(&urcall_read("W1AW", ""));
        assert_lint(&urcall_read("W1AW", "P"));
    }

    #[test]
    fn urcall_set_format() {
        assert_eq!(urcall_set("W1AW"), "Destination callsign set to W1AW");
        assert_lint(&urcall_set("W1AW"));
    }

    #[test]
    fn cq_and_reflector_strings() {
        assert_eq!(cq_set(), "Destination set to CQCQCQ");
        assert_eq!(
            reflector_connected("REF030", 'C'),
            "Connected to REF030 module C"
        );
        assert_eq!(reflector_disconnected(), "Disconnected from reflector");
        assert_lint(cq_set());
        assert_lint(&reflector_connected("REF030", 'C'));
        assert_lint(reflector_disconnected());
    }

    #[test]
    fn aprs_events_all_pass_lint() {
        let cases = vec![
            aprs_station_heard("W1AW"),
            aprs_message_received("W1AW", "Hello there"),
            aprs_message_delivered("42"),
            aprs_message_rejected("43"),
            aprs_message_expired("44"),
            aprs_position("W1AW", 35.3, -82.46),
            aprs_weather("W1AW"),
            aprs_digipeated("W1AW"),
            aprs_query_responded("W1AW"),
            aprs_raw_packet("W1AW"),
            aprs_mode_active().to_string(),
            aprs_is_connected().to_string(),
            aprs_is_incoming("W1AW-7>APRS:hello"),
            aprs_station_entry("W1AW", Some((35.3, -82.46)), 12, "2 minutes"),
            aprs_station_entry("W1AW", None, 1, "15 seconds"),
            aprs_stations_summary(5),
        ];
        for s in &cases {
            assert_lint(s);
        }
    }

    #[test]
    fn aprs_position_format() {
        let s = aprs_position("W1AW", 35.3, -82.46);
        assert_eq!(
            s,
            "APRS position from W1AW: latitude 35.3000, longitude -82.4600."
        );
    }

    #[test]
    fn aprs_is_connected_has_no_unicode() {
        let s = aprs_is_connected();
        assert!(!s.contains('\u{2194}'), "must not contain left-right arrow");
        assert!(!s.contains('\u{2192}'), "must not contain right arrow");
        assert_lint(s);
    }

    #[test]
    fn aprs_query_responded_uses_comma_not_em_dash() {
        let s = aprs_query_responded("W1AW");
        assert!(!s.contains('\u{2014}'), "must not contain em dash");
        assert_eq!(s, "APRS position query from W1AW, responded with beacon.");
        assert_lint(&s);
    }

    #[test]
    fn aprs_is_incoming_format() {
        let s = aprs_is_incoming("W1AW>APRS:hello");
        assert_eq!(s, "APRS-IS incoming: W1AW>APRS:hello");
        assert_lint(&s);
    }

    #[test]
    fn aprs_station_entry_with_position() {
        let s = aprs_station_entry("W1AW", Some((35.3, -82.46)), 12, "2 minutes");
        assert_eq!(
            s,
            "Station W1AW at 35.3000, -82.4600, 12 packets, heard 2 minutes ago."
        );
    }

    #[test]
    fn aprs_station_entry_without_position() {
        let s = aprs_station_entry("W1AW", None, 1, "15 seconds");
        assert_eq!(s, "Station W1AW, 1 packets, heard 15 seconds ago.");
    }

    #[test]
    fn dstar_events_pass_lint() {
        let cases: Vec<String> = vec![
            dstar_voice_start("W1AW", "P", "CQCQCQ"),
            dstar_voice_start("W1AW", "", "W9ABC"),
            dstar_voice_end().to_string(),
            dstar_voice_lost().to_string(),
            dstar_text_message("Hello from W1AW"),
            dstar_gps(""),
            dstar_gps("$GPGGA,..."),
            dstar_station_heard("W1AW"),
            dstar_command_cq().to_string(),
            dstar_command_echo().to_string(),
            dstar_command_unlink().to_string(),
            dstar_command_info().to_string(),
            dstar_command_link("REF030", 'C'),
            dstar_command_callsign("W1AW"),
            dstar_modem_status(5, false),
            dstar_modem_status(0, true),
            reflector_event_connected().to_string(),
            reflector_event_rejected().to_string(),
            reflector_event_disconnected().to_string(),
            reflector_event_voice_start("W1AW", "", "CQCQCQ"),
            reflector_event_voice_start("W1AW", "P", "CQCQCQ"),
            reflector_event_voice_end(0, 0),
            reflector_event_voice_end(1, 20),
            reflector_event_voice_end(42, 840),
        ];
        for s in &cases {
            assert_lint(s);
        }
    }

    #[test]
    fn dstar_voice_start_with_suffix() {
        let s = dstar_voice_start("W1AW", "P", "CQCQCQ");
        assert_eq!(s, "D-STAR voice from W1AW /P, to CQCQCQ.");
    }

    #[test]
    fn dstar_voice_start_without_suffix() {
        let s = dstar_voice_start("W1AW", "", "CQCQCQ");
        assert_eq!(s, "D-STAR voice from W1AW, to CQCQCQ.");
    }

    #[test]
    fn dstar_gps_empty_and_with_text() {
        assert_eq!(dstar_gps(""), "D-STAR GPS position data received.");
        assert_eq!(dstar_gps("$GPGGA,..."), "D-STAR GPS data: $GPGGA,...");
    }

    #[test]
    fn dstar_modem_status_formats() {
        assert_eq!(
            dstar_modem_status(5, false),
            "D-STAR modem: buffer 5, transmit idle."
        );
        assert_eq!(
            dstar_modem_status(0, true),
            "D-STAR modem: buffer 0, transmit active."
        );
    }

    #[test]
    fn startup_strings_lint() {
        assert_lint(&startup_banner("0.1.0"));
        assert_lint(&connected_via("/dev/cu.usbmodem1234"));
        assert_lint(goodbye());
        assert_lint(type_help_hint());
        assert_lint(&startup_identified("TH-D75", "1.03"));
    }

    #[test]
    fn startup_banner_format() {
        assert_eq!(
            startup_banner("0.1.0"),
            "Kenwood TH-D75 accessible radio control, version 0.1.0."
        );
    }

    #[test]
    fn connected_via_format() {
        assert_eq!(
            connected_via("/dev/cu.usbmodem1234"),
            "Connected via /dev/cu.usbmodem1234."
        );
    }

    #[test]
    fn goodbye_and_hint_format() {
        assert_eq!(goodbye(), "Goodbye.");
        assert_eq!(
            type_help_hint(),
            "Type help for a list of commands, or quit to exit."
        );
    }

    #[test]
    fn startup_identified_format() {
        assert_eq!(
            startup_identified("TH-D75", "1.03"),
            "Radio model: TH-D75. Firmware version: 1.03."
        );
    }

    proptest::proptest! {
        #[test]
        fn frequency_always_lints(hz in 0u32..=1_300_000_000u32) {
            let s = frequency(Band::A, hz);
            lint::check_line(&s).unwrap_or_else(|v| panic!("{s:?}: {v:?}"));
            proptest::prop_assert!(s.chars().count() <= 80);
        }
    }
}
