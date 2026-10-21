//! Pure message formatting for checked CAT and APRS values.
//!
//! These functions perform no I/O. Units are spelled out and state labels use
//! plain words. The shared presentation layer escapes dynamic text and wraps
//! physical lines before writing them; a returned message can contain arbitrary
//! path, firmware or error text before that rendering step.
//!
//! # Checked formatting
//!
//! ```
//! use kenwood_thd75::types::{Band, Frequency, SquelchLevel, VoxDelay};
//! use thd75_repl::output;
//!
//! let frequency = Frequency::new(146_520_000);
//! assert_eq!(output::frequency(Band::A, frequency),
//!            "Band A frequency: 146.52 megahertz");
//! let maximum = SquelchLevel::new(6)?;
//! assert_eq!(output::squelch_set(Band::B, maximum),
//!            "Band B squelch set to 6");
//! assert_eq!(output::vox_delay_read(VoxDelay::MS_1000),
//!            "VOX delay: 1000 milliseconds");
//! # Ok::<(), kenwood_thd75::error::ValidationError>(())
//! ```

use std::ops::RangeInclusive;

use kenwood_thd75::types::{
    Band, BatteryLevel, BeaconMode, DstarCallsign, DstarCallsignEntry, FirmwareIdentity, Frequency,
    GpsSettings, Module, OperatingMode, PowerLevel, RadioClock, RadioModel, ReflectorCallsign,
    RegularChannel, SMeterReading, SquelchLevel, StepSize, TncDataBand, TncMode, UsbAudioOutput,
    VoxDelay, VoxGain,
};
use kenwood_thd75::{AprsMessage, Ax25Address, MessageId};

use crate::aprs_args::LatLon;

/// Human-readable band name.
#[must_use]
pub const fn band_name(band: Band) -> &'static str {
    match band {
        Band::A => "A",
        Band::B => "B",
    }
}

/// Format a frequency in megahertz for natural speech output.
///
/// Trailing fractional zeros are stripped; an integer megahertz value retains
/// one fractional zero. Zero renders as `0.0 megahertz`.
/// See the [checked formatting example](crate::output).
#[must_use]
pub fn freq_mhz(frequency: Frequency) -> String {
    let mhz = frequency.as_mhz();
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
pub fn frequency(band: Band, frequency: Frequency) -> String {
    format!(
        "Band {} frequency: {}",
        band_name(band),
        freq_mhz(frequency)
    )
}

/// `Band {A|B} tuned to {f} megahertz`.
#[must_use]
pub fn tuned_to(band: Band, frequency: Frequency) -> String {
    format!("Band {} tuned to {}", band_name(band), freq_mhz(frequency))
}

/// `Band {A|B} stepped up to {f} megahertz`.
#[must_use]
pub fn stepped_up(band: Band, frequency: Frequency) -> String {
    format!(
        "Band {} stepped up to {}",
        band_name(band),
        freq_mhz(frequency)
    )
}

/// `Band {A|B} stepped down to {f} megahertz`.
#[must_use]
pub fn stepped_down(band: Band, frequency: Frequency) -> String {
    format!(
        "Band {} stepped down to {}",
        band_name(band),
        freq_mhz(frequency)
    )
}

/// `Band {A|B} step size: {step}`.
#[must_use]
pub fn step_size_read(band: Band, step: StepSize) -> String {
    format!(
        "Band {} step size: {} kilohertz",
        band_name(band),
        step.as_khz_str()
    )
}

/// `Band {A|B} step size set to {step}`.
#[must_use]
pub fn step_size_set(band: Band, step: StepSize) -> String {
    format!(
        "Band {} step size set to {} kilohertz",
        band_name(band),
        step.as_khz_str()
    )
}

/// `Band {A|B} transmit offset: {hz} hertz`.
#[must_use]
pub fn tx_offset(band: Band, offset: Frequency) -> String {
    format!(
        "Band {} transmit offset: {} hertz",
        band_name(band),
        offset.as_hz()
    )
}

/// `Band {A|B} mode: {mode}`, using the model's full mode name.
#[must_use]
pub fn mode_read(band: Band, mode: OperatingMode) -> String {
    format!("Band {} mode: {mode}", band_name(band))
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
pub fn mode_set(band: Band, mode: OperatingMode) -> String {
    format!("Band {} mode set to {mode}", band_name(band))
}

/// Format a TNC mode readout.
#[must_use]
pub fn tnc_mode_read(mode: TncMode, data_band: TncDataBand) -> String {
    format!("TNC mode: {mode} on Band {}", tnc_data_band_name(data_band))
}

/// Format a TNC mode change confirmation.
#[must_use]
pub fn tnc_mode_set(mode: TncMode, data_band: TncDataBand) -> String {
    format!(
        "TNC mode set to {mode} on Band {}",
        tnc_data_band_name(data_band)
    )
}

const fn tnc_data_band_name(data_band: TncDataBand) -> &'static str {
    match data_band {
        TncDataBand::A => "A",
        TncDataBand::B => "B",
    }
}

/// Format a firmware beacon mode readout.
#[must_use]
pub fn beacon_mode_read(mode: BeaconMode) -> String {
    format!("Firmware beacon mode: {mode}")
}

/// Format a firmware beacon mode change confirmation.
#[must_use]
pub fn beacon_mode_set(mode: BeaconMode) -> String {
    format!("Firmware beacon mode set to {mode}")
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

/// `Band {A|B} squelch level: {level}`, for the checked range 0 through 6.
/// See the [checked formatting example](crate::output).
#[must_use]
pub fn squelch_read(band: Band, level: SquelchLevel) -> String {
    format!("Band {} squelch level: {}", band_name(band), level.as_raw())
}

/// `Band {A|B} squelch set to {level}`.
#[must_use]
pub fn squelch_set(band: Band, level: SquelchLevel) -> String {
    format!("Band {} squelch set to {}", band_name(band), level.as_raw())
}

/// `Band {A|B} S-meter: {reading}`.
#[must_use]
pub fn smeter(band: Band, reading: SMeterReading) -> String {
    format!("Band {} S-meter: {reading}", band_name(band))
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
        BatteryLevel::Unidentified5 => "state 5, meaning not yet qualified",
    }
}

/// Render an `on`/`off` word from a bool.
#[must_use]
pub const fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// `Radio model: {model}`.
#[must_use]
pub fn radio_model(model: RadioModel) -> String {
    format!("Radio model: {model}")
}

/// `Firmware version: {version}`.
#[must_use]
pub fn firmware_version(version: &FirmwareIdentity) -> String {
    format!("Firmware version: {version}")
}

/// `Battery level: {level}`.
#[must_use]
pub fn battery(level: BatteryLevel) -> String {
    format!("Battery level: {}", battery_level_display(level))
}

/// `Radio clock: {time}`.
#[must_use]
pub fn clock(time: RadioClock) -> String {
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
pub fn vox_gain_read(level: VoxGain) -> String {
    format!("VOX gain: {level}")
}

/// `VOX gain set to {level}`.
#[must_use]
pub fn vox_gain_set(level: VoxGain) -> String {
    format!("VOX gain set to {level}")
}

/// `VOX delay: {milliseconds} milliseconds`.
/// See the [checked formatting example](crate::output).
#[must_use]
pub fn vox_delay_read(delay: VoxDelay) -> String {
    format!("VOX delay: {} milliseconds", delay.as_milliseconds())
}

/// `VOX delay set to {milliseconds} milliseconds`.
#[must_use]
pub fn vox_delay_set(delay: VoxDelay) -> String {
    format!("VOX delay set to {} milliseconds", delay.as_milliseconds())
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
pub fn channel_read(number: RegularChannel, frequency: Frequency) -> String {
    format!("Channel {number}: {}", freq_mhz(frequency))
}

/// `Reading channels {start} through {end}, please wait.`
/// An empty inclusive range renders `No channels in selected range.`.
#[must_use]
pub fn channels_reading(range: RangeInclusive<RegularChannel>) -> String {
    if range.is_empty() {
        return "No channels in selected range.".to_owned();
    }
    format!(
        "Reading channels {} through {}, please wait.",
        range.start(),
        range.end()
    )
}

/// `{count} programmed channels found.` or `No programmed channels in
/// that range.`.
#[must_use]
pub fn channels_summary(count: usize) -> String {
    if count == 0 {
        "No programmed channels in that range.".to_string()
    } else {
        let word = if count == 1 { "channel" } else { "channels" };
        format!("{count} programmed {word} found.")
    }
}

/// `GPS: {on|off}, PC output: {on|off}`.
#[must_use]
pub fn gps_settings(settings: GpsSettings) -> String {
    format!(
        "GPS: {}, PC output: {}",
        on_off(settings.enabled()),
        on_off(settings.pc_output())
    )
}

/// `Destination callsign: {call}` or `... suffix {suffix}`.
#[must_use]
pub fn urcall_read(entry: &DstarCallsignEntry) -> String {
    let call = entry.callsign.as_str();
    let suffix = entry.suffix.as_str();
    if suffix.is_empty() {
        format!("Destination callsign: {call}")
    } else {
        format!("Destination callsign: {call} suffix {suffix}")
    }
}

/// `Destination callsign set to {call}`.
#[must_use]
pub fn urcall_set(call: &DstarCallsign) -> String {
    format!("Destination callsign set to {}", call.as_str())
}

/// `Destination set to CQCQCQ`.
#[must_use]
pub const fn cq_set() -> &'static str {
    "Destination set to CQCQCQ"
}

/// Report the completed local CAT destination update for a link request.
/// The remote reflector's response is not observed by this operation.
#[must_use]
pub fn reflector_link_requested(name: ReflectorCallsign, module: Module) -> String {
    format!("Link request destination set to {name} module {module}")
}

/// Report the completed local CAT destination update for an unlink request.
#[must_use]
pub const fn reflector_unlink_requested() -> &'static str {
    "Unlink request destination set"
}

/// `Operation band: {A|B}`.
#[must_use]
pub fn operation_band_read(band: Band) -> String {
    format!("Operation band: {}", band_name(band))
}

/// `Operation band set to {A|B}`.
#[must_use]
pub fn operation_band_set(band: Band) -> String {
    format!("Operation band set to {}", band_name(band))
}

/// `USB audio output: {AF|IF|Detect}` (CAT IO, radio Menu 102).
#[must_use]
pub fn usb_output_read(mode: UsbAudioOutput) -> String {
    format!("USB audio output: {mode}")
}

/// `USB audio output set to {AF|IF|Detect}`.
#[must_use]
pub fn usb_output_set(mode: UsbAudioOutput) -> String {
    format!("USB audio output set to {mode}")
}

// ---------------------------------------------------------------------------
// APRS mode output
// ---------------------------------------------------------------------------

/// `APRS station heard: {callsign}.`
#[must_use]
pub fn aprs_station_heard(callsign: &Ax25Address) -> String {
    format!("APRS station heard: {callsign}.")
}

/// `APRS message received for {addressee}: {text}.`
#[must_use]
pub fn aprs_message_received(message: &AprsMessage) -> String {
    format!(
        "APRS message received for {}: {}.",
        message.addressee(),
        message.text()
    )
}

/// `APRS message delivered, ID {id}.`
#[must_use]
pub fn aprs_message_delivered(id: &MessageId) -> String {
    format!("APRS message delivered, ID {id}.")
}

/// `APRS message rejected by remote station, ID {id}.`
#[must_use]
pub fn aprs_message_rejected(id: &MessageId) -> String {
    format!("APRS message rejected by remote station, ID {id}.")
}

/// `APRS message expired after all retries, ID {id}.`
#[must_use]
pub fn aprs_message_expired(id: &MessageId) -> String {
    format!("APRS message expired after all retries, ID {id}.")
}

/// `APRS position from {source}: latitude {lat}, longitude {lon}.`
#[must_use]
pub fn aprs_position(source: &Ax25Address, position: LatLon) -> String {
    let lat = position.latitude.as_degrees();
    let lon = position.longitude.as_degrees();
    format!("APRS position from {source}: latitude {lat:.4}, longitude {lon:.4}.")
}

/// `APRS weather report from {source}.`
#[must_use]
pub fn aprs_weather(source: &Ax25Address) -> String {
    format!("APRS weather report from {source}.")
}

/// `APRS packet relayed from {source}.`
#[must_use]
pub fn aprs_digipeated(source: &Ax25Address) -> String {
    format!("APRS packet relayed from {source}.")
}

/// `APRS position query from {to}, responded with beacon.`
#[must_use]
pub fn aprs_query_responded(to: &Ax25Address) -> String {
    format!("APRS position query from {to}, responded with beacon.")
}

/// `APRS packet from {source}.`
#[must_use]
pub fn aprs_raw_packet(source: &Ax25Address) -> String {
    format!("APRS packet from {source}.")
}

/// `APRS mode active. Type aprs stop to exit.`
#[must_use]
pub const fn aprs_mode_active() -> &'static str {
    "APRS mode active. Type aprs stop to exit."
}

/// Connected TCP state while the server's verified-login response is pending.
#[must_use]
pub const fn aprs_is_connected() -> &'static str {
    "APRS-IS connected. Awaiting verified login. Press Ctrl-C to stop."
}

/// `APRS-IS incoming: {line}`.
#[must_use]
pub fn aprs_is_incoming(line: &str) -> String {
    format!("APRS-IS incoming: {line}")
}

/// `Station {call}{pos}, {n} packets, heard {elapsed} ago.`
#[must_use]
pub fn aprs_station_entry(
    callsign: &Ax25Address,
    position: Option<LatLon>,
    packet_count: u32,
    elapsed_display: &str,
) -> String {
    let pos = position.map_or_else(String::new, |position| {
        format!(
            " at {:.4}, {:.4}",
            position.latitude.as_degrees(),
            position.longitude.as_degrees()
        )
    });
    let packet_word = if packet_count == 1 {
        "packet"
    } else {
        "packets"
    };
    format!("Station {callsign}{pos}, {packet_count} {packet_word}, heard {elapsed_display} ago.")
}

/// `{count} stations heard.` (singular `station` for a count of 1).
#[must_use]
pub fn stations_summary(count: usize) -> String {
    let word = if count == 1 { "station" } else { "stations" };
    format!("{count} {word} heard.")
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
pub fn startup_identified(model: RadioModel, firmware: &FirmwareIdentity) -> String {
    format!("Radio model: {model}. Firmware version: {firmware}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint;
    use kenwood_thd75::types::{Band, RadioDateTime};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn assert_lint(s: &str) {
        let result = lint::check_output(s);
        assert!(result.is_ok(), "line {s:?} failed lint: {result:?}");
    }

    #[test]
    fn freq_mhz_standard() {
        assert_eq!(freq_mhz(Frequency::new(146_520_000)), "146.52 megahertz");
        assert_eq!(freq_mhz(Frequency::new(446_000_000)), "446.0 megahertz");
        assert_eq!(freq_mhz(Frequency::new(0)), "0.0 megahertz");
    }

    #[test]
    fn freq_mhz_high_band() {
        assert_eq!(freq_mhz(Frequency::new(1_200_000_000)), "1200.0 megahertz");
        assert_eq!(freq_mhz(Frequency::new(1_300_000_000)), "1300.0 megahertz");
    }

    #[test]
    fn frequency_band_a() {
        let s = frequency(Band::A, Frequency::new(146_520_000));
        assert_eq!(s, "Band A frequency: 146.52 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn frequency_band_b() {
        let s = frequency(Band::B, Frequency::new(446_000_000));
        assert_eq!(s, "Band B frequency: 446.0 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn tuned_to_band_a() {
        let s = tuned_to(Band::A, Frequency::new(146_520_000));
        assert_eq!(s, "Band A tuned to 146.52 megahertz");
        assert_lint(&s);
    }

    #[test]
    fn stepped_up_and_down() {
        let up = stepped_up(Band::A, Frequency::new(146_525_000));
        let down = stepped_down(Band::A, Frequency::new(146_515_000));
        assert_eq!(up, "Band A stepped up to 146.525 megahertz");
        assert_eq!(down, "Band A stepped down to 146.515 megahertz");
        assert_lint(&up);
        assert_lint(&down);
    }

    #[test]
    fn step_size_formats() {
        let s = step_size_read(Band::A, StepSize::Hz25000);
        assert_eq!(s, "Band A step size: 25.0 kilohertz");
        assert_lint(&s);
    }

    #[test]
    fn step_size_set_format() {
        let s = step_size_set(Band::B, StepSize::Hz12500);
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
        let s = tx_offset(Band::A, Frequency::new(600_000));
        assert_eq!(s, "Band A transmit offset: 600000 hertz");
        assert_lint(&s);
    }

    #[test]
    fn mode_read_basic() {
        let s = mode_read(Band::A, OperatingMode::Fm);
        assert_eq!(s, "Band A mode: FM");
        assert_lint(&s);
    }

    #[test]
    fn mode_read_and_set() {
        let r = mode_read(Band::A, OperatingMode::Fm);
        let s = mode_set(Band::B, OperatingMode::Dv);
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
    fn squelch_read_and_set() -> TestResult {
        let r = squelch_read(Band::A, SquelchLevel::new(3)?);
        let s = squelch_set(Band::A, SquelchLevel::new(6)?);
        assert_eq!(r, "Band A squelch level: 3");
        assert_eq!(s, "Band A squelch set to 6");
        assert_lint(&r);
        assert_lint(&s);
        Ok(())
    }

    #[test]
    fn smeter_format() {
        let s = smeter(Band::A, SMeterReading::ZERO);
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
            (
                BatteryLevel::Unidentified5,
                "Battery level: state 5, meaning not yet qualified",
            ),
        ];
        for (level, expected) in cases {
            let s = battery(level);
            assert_eq!(s, expected);
            assert_lint(&s);
        }
    }

    #[test]
    fn radio_model_format() {
        let s = radio_model(RadioModel::ThD75);
        assert_eq!(s, "Radio model: TH-D75");
        assert_lint(&s);
    }

    #[test]
    fn firmware_version_format() -> Result<(), kenwood_thd75::error::ValidationError> {
        let version = FirmwareIdentity::new("1.03")?;
        let s = firmware_version(&version);
        assert_eq!(s, "Firmware version: 1.03");
        assert_lint(&s);
        Ok(())
    }

    #[test]
    fn clock_format() -> Result<(), kenwood_thd75::error::ValidationError> {
        let value = RadioDateTime::new(2026, 4, 10, 14, 32, 7)?;
        let s = clock(value.into());
        assert_eq!(s, "Radio clock: 2026-04-10 14:32:07");
        assert_lint(&s);
        assert_eq!(clock(RadioClock::Unavailable), "Radio clock: unavailable");
        Ok(())
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
    fn vox_formats() -> TestResult {
        assert_eq!(vox(true), "VOX: on");
        assert_eq!(vox_set(false), "VOX set to off");
        assert_eq!(vox_gain_read(VoxGain::new(5)?), "VOX gain: 5");
        assert_eq!(vox_gain_set(VoxGain::new(7)?), "VOX gain set to 7");
        assert_eq!(
            vox_delay_read(VoxDelay::MS_1000),
            "VOX delay: 1000 milliseconds"
        );
        assert_eq!(
            vox_delay_set(VoxDelay::MS_500),
            "VOX delay set to 500 milliseconds"
        );
        assert_lint(&vox(true));
        assert_lint(&vox_set(true));
        assert_lint(&vox_gain_set(VoxGain::new(9)?));
        assert_lint(&vox_delay_read(VoxDelay::MS_3000));
        Ok(())
    }

    #[test]
    fn fm_radio_formats() {
        assert_eq!(fm_radio(true), "FM radio: on");
        assert_eq!(fm_radio_set(false), "FM radio set to off");
        assert_lint(&fm_radio(true));
        assert_lint(&fm_radio_set(false));
    }

    #[test]
    fn channel_read_format() -> TestResult {
        let s = channel_read(RegularChannel::new(5)?, Frequency::new(146_520_000));
        assert_eq!(s, "Channel 5: 146.52 megahertz");
        assert_lint(&s);
        Ok(())
    }

    #[test]
    fn channels_reading_format() -> TestResult {
        let first = RegularChannel::new(0)?;
        let last = RegularChannel::new(19)?;
        let s = channels_reading(first..=last);
        assert_eq!(s, "Reading channels 0 through 19, please wait.");
        assert_lint(&s);
        assert_eq!(
            channels_reading(last..=first),
            "No channels in selected range."
        );
        Ok(())
    }

    #[test]
    fn channels_summary_non_empty() {
        let s = channels_summary(3);
        assert_eq!(s, "3 programmed channels found.");
        assert_lint(&s);
        let s = channels_summary(1);
        assert_eq!(s, "1 programmed channel found.");
        assert_lint(&s);
    }

    #[test]
    fn channels_summary_empty() {
        let s = channels_summary(0);
        assert_eq!(s, "No programmed channels in that range.");
        assert_lint(&s);
    }

    #[test]
    fn gps_settings_format() {
        assert_eq!(
            gps_settings(GpsSettings::new().with_enabled(true).with_pc_output(true)),
            "GPS: on, PC output: on"
        );
        assert_eq!(
            gps_settings(GpsSettings::new().with_pc_output(true)),
            "GPS: off, PC output: on"
        );
        assert_eq!(
            gps_settings(GpsSettings::new().with_enabled(true)),
            "GPS: on, PC output: off"
        );
        assert_eq!(gps_settings(GpsSettings::new()), "GPS: off, PC output: off");
        assert_lint(&gps_settings(
            GpsSettings::new().with_enabled(true).with_pc_output(true),
        ));
    }

    #[test]
    fn urcall_read_with_and_without_suffix() -> TestResult {
        use kenwood_thd75::types::DstarSuffix;
        let mut entry = DstarCallsignEntry::new(DstarCallsign::new("W1AW")?, DstarSuffix::new("")?);
        assert_eq!(urcall_read(&entry), "Destination callsign: W1AW");
        assert_lint(&urcall_read(&entry));
        entry.suffix = DstarSuffix::new("P")?;
        assert_eq!(urcall_read(&entry), "Destination callsign: W1AW suffix P");
        assert_lint(&urcall_read(&entry));
        Ok(())
    }

    #[test]
    fn urcall_set_format() -> TestResult {
        let callsign = DstarCallsign::new("W1AW")?;
        assert_eq!(urcall_set(&callsign), "Destination callsign set to W1AW");
        assert_lint(&urcall_set(&callsign));
        Ok(())
    }

    #[test]
    fn cq_and_reflector_strings() -> TestResult {
        let reflector = ReflectorCallsign::try_from_str("REF030")?;
        assert_eq!(cq_set(), "Destination set to CQCQCQ");
        assert_eq!(
            reflector_link_requested(reflector, Module::C),
            "Link request destination set to REF030 module C"
        );
        assert_eq!(
            reflector_unlink_requested(),
            "Unlink request destination set"
        );
        assert_lint(cq_set());
        assert_lint(&reflector_link_requested(reflector, Module::C));
        assert_lint(reflector_unlink_requested());
        Ok(())
    }

    #[test]
    fn operation_band_formats() {
        assert_eq!(operation_band_read(Band::A), "Operation band: A");
        assert_eq!(operation_band_set(Band::B), "Operation band set to B");
        assert_lint(&operation_band_read(Band::A));
        assert_lint(&operation_band_set(Band::B));
    }

    #[test]
    fn usb_output_formats() {
        use kenwood_thd75::types::UsbAudioOutput;
        assert_eq!(
            usb_output_read(UsbAudioOutput::Audio),
            "USB audio output: Audio"
        );
        assert_eq!(
            usb_output_read(UsbAudioOutput::IntermediateFrequency),
            "USB audio output: Intermediate Frequency"
        );
        assert_eq!(
            usb_output_read(UsbAudioOutput::Detect),
            "USB audio output: Detect"
        );
        assert_eq!(
            usb_output_set(UsbAudioOutput::IntermediateFrequency),
            "USB audio output set to Intermediate Frequency"
        );
        assert_lint(&usb_output_read(UsbAudioOutput::Audio));
        assert_lint(&usb_output_set(UsbAudioOutput::Detect));
    }

    #[test]
    fn aprs_events_all_pass_lint() -> TestResult {
        let source = Ax25Address::new("W1AW", 0)?;
        let position = LatLon::parse("35.3", "-82.46")?;
        let message = AprsMessage::from_bytes(b":W1AW     :Hello there")?;
        let cases = vec![
            aprs_station_heard(&source),
            aprs_message_received(&message),
            aprs_message_delivered(&MessageId::new("42")?),
            aprs_message_rejected(&MessageId::new("43")?),
            aprs_message_expired(&MessageId::new("44")?),
            aprs_position(&source, position),
            aprs_weather(&source),
            aprs_digipeated(&source),
            aprs_query_responded(&source),
            aprs_raw_packet(&source),
            aprs_mode_active().to_string(),
            aprs_is_connected().to_string(),
            aprs_is_incoming("W1AW-7>APRS:hello"),
            aprs_station_entry(&source, Some(position), 12, "2 minutes"),
            aprs_station_entry(&source, None, 1, "15 seconds"),
            stations_summary(5),
            stations_summary(1),
        ];
        for s in &cases {
            assert_lint(s);
        }
        Ok(())
    }

    #[test]
    fn aprs_position_format() -> TestResult {
        let s = aprs_position(
            &Ax25Address::new("W1AW", 0)?,
            LatLon::parse("35.3", "-82.46")?,
        );
        assert_eq!(
            s,
            "APRS position from W1AW: latitude 35.3000, longitude -82.4600."
        );
        Ok(())
    }

    #[test]
    fn aprs_is_connected_has_no_unicode() {
        let s = aprs_is_connected();
        assert!(
            s.contains("Awaiting verified login"),
            "a TCP connection must not announce forwarding: {s}"
        );
        assert!(!s.contains('\u{2194}'), "must not contain left-right arrow");
        assert!(!s.contains('\u{2192}'), "must not contain right arrow");
        assert_lint(s);
    }

    #[test]
    fn aprs_query_responded_preserves_the_complete_source() -> TestResult {
        let s = aprs_query_responded(&Ax25Address::new("W1AW", 11)?);
        assert!(!s.contains('\u{2014}'), "must not contain em dash");
        assert_eq!(
            s,
            "APRS position query from W1AW-11, responded with beacon."
        );
        assert_lint(&s);
        Ok(())
    }

    #[test]
    fn aprs_is_incoming_format() {
        let s = aprs_is_incoming("W1AW>APRS:hello");
        assert_eq!(s, "APRS-IS incoming: W1AW>APRS:hello");
        assert_lint(&s);
    }

    #[test]
    fn aprs_station_entry_with_position() -> TestResult {
        let source = Ax25Address::new("W1AW", 0)?;
        let s = aprs_station_entry(
            &source,
            Some(LatLon::parse("35.3", "-82.46")?),
            12,
            "2 minutes",
        );
        assert_eq!(
            s,
            "Station W1AW at 35.3000, -82.4600, 12 packets, heard 2 minutes ago."
        );
        Ok(())
    }

    #[test]
    fn aprs_station_entry_without_position() -> TestResult {
        let s = aprs_station_entry(&Ax25Address::new("W1AW", 0)?, None, 1, "15 seconds");
        assert_eq!(s, "Station W1AW, 1 packet, heard 15 seconds ago.");
        Ok(())
    }

    #[test]
    fn stations_summary_singular_and_plural() {
        assert_eq!(stations_summary(1), "1 station heard.");
        assert_eq!(stations_summary(5), "5 stations heard.");
    }

    #[test]
    fn startup_strings_lint() -> Result<(), kenwood_thd75::error::ValidationError> {
        let firmware = FirmwareIdentity::new("1.03")?;
        assert_lint(&startup_banner("0.1.0"));
        assert_lint(&connected_via("/dev/cu.usbmodem1234"));
        assert_lint(goodbye());
        assert_lint(type_help_hint());
        assert_lint(&startup_identified(RadioModel::ThD75, &firmware));
        Ok(())
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
    fn startup_identified_format() -> Result<(), kenwood_thd75::error::ValidationError> {
        let firmware = FirmwareIdentity::new("1.03")?;
        assert_eq!(
            startup_identified(RadioModel::ThD75, &firmware),
            "Radio model: TH-D75. Firmware version: 1.03."
        );
        Ok(())
    }

    proptest::proptest! {
        #[test]
        fn frequency_always_lints(hz in 0u32..=1_300_000_000u32) {
            let s = frequency(Band::A, Frequency::new(hz));
            let lint_result = lint::check_line(&s);
            proptest::prop_assert!(lint_result.is_ok(), "{s:?}: {lint_result:?}");
            proptest::prop_assert!(s.chars().count() <= 80);
        }
    }
}
