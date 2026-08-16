//! REPL command implementations.
//!
//! Each command function takes a `&mut Radio<impl Transport>` and an
//! argument slice, performs the operation, and prints the result to
//! stdout. All output is plain text, one self-contained line per
//! datum, designed for screen-reader accessibility.
//!
//! # Accessibility standards (WCAG 2.1 + CHI 2021 CLI study)
//!
//! These rules are mandatory for all output in this module:
//!
//! - **One self-contained line per datum.** Screen readers navigate
//!   line-by-line; each line must make sense without context from
//!   adjacent lines. No indented sub-items; repeat the label.
//! - **Label-colon-value format.** Every response starts with a label
//!   (e.g. "Band A frequency: 146.52 megahertz"). WCAG 1.3.1.
//! - **Natural language units.** Say "megahertz" not "MHz", "on"/"off"
//!   not "true"/"false" or "1"/"0". WCAG 3.1.2.
//! - **"Error:" prefix on all errors.** Screen reader users search
//!   for this keyword. WCAG 3.3.1.
//! - **Explicit confirmation after mutations.** Never return silently
//!   after a set command. WCAG 3.3.1.
//! - **Count summary after lists.** "5 programmed channels found."
//!   tells the user the list is done.
//! - **No box drawing, ASCII art, Unicode symbols, or spinners.**
//!   Screen readers read these character-by-character. WCAG 1.1.1.
//! - **No ANSI color/escape sequences.** They are invisible to screen
//!   readers. If added later, gate behind `NO_COLOR` and `TERM=dumb`.
//!   WCAG 1.4.1.
//! - **No cursor repositioning or line overwriting (`\r`).** Causes
//!   screen readers to re-announce partial lines.
//! - **Lines under 80 characters.** Long lines require horizontal
//!   scrolling, which is painful with character-by-character review.
//! - **Diagnostics to stderr, user output to stdout.** Separation lets
//!   users pipe stdout to speech tools or scripts.

use kenwood_thd75::Radio;
use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::{
    Band, BandMode, DstarCallsign, DstarSuffix, Frequency, GpsSettings, Module, OperatingMode,
    ReflectorCallsign, RegularChannel, UsbAudioOutput,
};
use thd75_repl::aprintln;

/// Parse a band argument ("a" or "b"), defaulting to A.
fn parse_band(s: Option<&&str>) -> Band {
    match s.map(|s| s.to_lowercase()).as_deref() {
        Some("b" | "1") => Band::B,
        _ => Band::A,
    }
}

/// Human-readable band name.
const fn band_name(band: Band) -> &'static str {
    match band {
        Band::A => "A",
        Band::B => "B",
    }
}

/// Format a duration for screen reader speech.
pub(crate) fn fmt_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs} {}", if secs == 1 { "second" } else { "seconds" })
    } else if secs < 3600 {
        let minutes = secs / 60;
        format!(
            "{minutes} {}",
            if minutes == 1 { "minute" } else { "minutes" }
        )
    } else {
        let hours = secs / 3600;
        format!("{hours} {}", if hours == 1 { "hour" } else { "hours" })
    }
}

/// Parse a boolean argument (on/off/true/false/1/0).
fn parse_bool(s: &str) -> Option<bool> {
    match s.to_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Info
// ---------------------------------------------------------------------------

/// Print the radio model identification (ID command).
pub(crate) async fn identify<T: Transport>(radio: &mut Radio<T>) {
    match radio.identify().await {
        Ok(info) => aprintln!("{}", thd75_repl::output::radio_model(info.model)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Print the battery charge level (BL command).
pub(crate) async fn battery<T: Transport>(radio: &mut Radio<T>) {
    match radio.get_battery_level().await {
        Ok(level) => aprintln!("{}", thd75_repl::output::battery(level)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Print the radio's real-time clock (RT command).
pub(crate) async fn clock<T: Transport>(radio: &mut Radio<T>) {
    match radio.get_real_time_clock().await {
        Ok(time) => aprintln!("{}", thd75_repl::output::clock(time)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Frequency / Mode / Squelch / Power
// ---------------------------------------------------------------------------

/// Read the current frequency on a band. Args: `[a|b]`, default A.
pub(crate) async fn frequency<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.get_frequency(band).await {
        Ok(frequency) => aprintln!("{}", thd75_repl::output::frequency(band, frequency.as_hz())),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Read or set the CAT-safe TNC protocol mode. Args: `[off|aprs] [1200|9600]`.
///
/// With no arguments, reads the current mode and speed. Use `aprs start` for
/// an owned KISS session. APRS mode hands packet operation to the radio's own
/// firmware.
pub(crate) async fn tnc_mode<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let Some(&mode_arg) = args.first() else {
        match radio.get_tnc_mode().await {
            Ok(state) => aprintln!(
                "{}",
                thd75_repl::output::tnc_mode_read(state.mode, state.data_rate)
            ),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    };
    let mode = match mode_arg.to_lowercase().as_str() {
        "off" => kenwood_thd75::types::TncControlMode::Off,
        "aprs" => kenwood_thd75::types::TncControlMode::Aprs,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown TNC mode: {other}"))
            );
            aprintln!("Valid modes: off, aprs. Use 'aprs start' for KISS.");
            return;
        }
    };
    let data_rate = match args.get(1) {
        None | Some(&"1200") => kenwood_thd75::types::PacketDataRate::Bps1200,
        Some(&"9600") => kenwood_thd75::types::PacketDataRate::Bps9600,
        Some(other) => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown speed {other}. Use 1200 or 9600."))
            );
            return;
        }
    };
    match radio.set_tnc_mode(mode, data_rate).await {
        Ok(()) => aprintln!(
            "{}",
            thd75_repl::output::tnc_mode_set(mode.into(), data_rate)
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Read or set the firmware beacon mode. Args: `[manual|ptt|auto|smart]`.
///
/// Auto, smart, and PTT make the radio transmit BY ITSELF while its
/// TNC is in APRS mode, so those settings go through the transmit
/// confirmation gate.
pub(crate) async fn beacon_mode<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let Some(&arg) = args.first() else {
        match radio.get_beacon_mode().await {
            Ok(mode) => aprintln!("{}", thd75_repl::output::beacon_mode_read(mode)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    };
    let mode = match arg.to_lowercase().as_str() {
        "manual" => kenwood_thd75::types::BeaconMode::Manual,
        "ptt" => kenwood_thd75::types::BeaconMode::Ptt,
        "auto" => kenwood_thd75::types::BeaconMode::Auto,
        "smart" | "smartbeaconing" => kenwood_thd75::types::BeaconMode::SmartBeaconing,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown beacon mode: {other}"))
            );
            aprintln!("Valid modes: manual, ptt, auto, smart");
            return;
        }
    };
    let transmits = matches!(
        mode,
        kenwood_thd75::types::BeaconMode::Ptt
            | kenwood_thd75::types::BeaconMode::Auto
            | kenwood_thd75::types::BeaconMode::SmartBeaconing
    );
    if transmits && !thd75_repl::confirm::tx_confirm() {
        return;
    }
    match radio.set_beacon_mode(mode).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::beacon_mode_set(mode)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Read or set the squelch level. Args: `[a|b] [level]`.
/// With one arg, reads. With two, sets (level 0-5).
pub(crate) async fn squelch<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());

    // Second arg present and numeric = set squelch.
    if let Some(Ok(level)) = args.get(1).map(|s| s.parse::<u8>()) {
        match kenwood_thd75::types::SquelchLevel::try_from(level) {
            Ok(sq) => match radio.set_squelch(band, sq).await {
                Ok(()) => aprintln!("{}", thd75_repl::output::squelch_set(band, level)),
                Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
            },
            Err(e) => aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("invalid squelch level: {e}"))
            ),
        }
        return;
    }

    match radio.get_squelch(band).await {
        Ok(sq) => aprintln!("{}", thd75_repl::output::squelch_read(band, u8::from(sq))),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Read the signal strength meter on a band. Args: `[a|b]`.
pub(crate) async fn smeter<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.get_smeter(band).await {
        Ok(reading) => aprintln!("{}", thd75_repl::output::smeter(band, &reading.to_string())),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Lock / Bluetooth / Attenuator
// ---------------------------------------------------------------------------

/// Report that key lock has no verified CAT operation.
pub(crate) fn lock<T: Transport>(_radio: &mut Radio<T>, _args: &[&str]) {
    aprintln!(
        "{}",
        thd75_repl::output::error(
            "Key lock has no verified CAT operation; LC controls the LCD backlight"
        )
    );
}

/// Read or set Bluetooth state. Args: `[on|off]`.
pub(crate) async fn bluetooth<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        match radio.set_bluetooth(val).await {
            Ok(()) => aprintln!("{}", thd75_repl::output::bluetooth(val)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    } else {
        match radio.get_bluetooth().await {
            Ok(enabled) => aprintln!("{}", thd75_repl::output::bluetooth(enabled)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    }
}

/// Read or set the attenuator on a band. Args: `[a|b] [on|off]`.
pub(crate) async fn attenuator<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());

    if let Some(val) = args.get(1).and_then(|s| parse_bool(s)) {
        match radio.set_attenuator(band, val).await {
            Ok(()) => aprintln!("{}", thd75_repl::output::attenuator(band, val)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    }

    match radio.get_attenuator(band).await {
        Ok(on) => aprintln!("{}", thd75_repl::output::attenuator(band, on)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Frequency stepping
// ---------------------------------------------------------------------------

/// Step frequency up by one increment, then read back. Args: `[a|b]`.
pub(crate) async fn step_up<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    let result = async {
        radio.set_band(band).await?;
        radio.frequency_up().await
    }
    .await;
    match result {
        Ok(frequency) => aprintln!(
            "{}",
            thd75_repl::output::stepped_up(band, frequency.as_hz())
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Step frequency down by one increment and read back. Args: `[a|b]`.
pub(crate) async fn step_down<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    let result = async {
        radio.set_band(band).await?;
        radio.frequency_down().await
    }
    .await;
    match result {
        Ok(frequency) => aprintln!(
            "{}",
            thd75_repl::output::stepped_down(band, frequency.as_hz())
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Tuning
// ---------------------------------------------------------------------------

/// Parse a direct-frequency request and report the current frequency.
///
/// Step-tune a band's VFO with individually verified UP/DW steps.
/// Args: `<a|b> <mhz>`. Direct FO/FQ frequency writes remain quarantined.
pub(crate) async fn tune<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        aprintln!("Usage: tune <a or b> <frequency in megahertz>");
        aprintln!("Example: tune a 146.520");
        return;
    }

    let band = parse_band(args.first());
    let Some(&freq_str) = args.get(1) else {
        return;
    };

    let target = match Frequency::from_mhz_str(freq_str) {
        Ok(target) => target,
        Err(e) => {
            aprintln!("{}", thd75_repl::output::error(e));
            return;
        }
    };

    match radio.step_tune(band, target).await {
        Ok(landed) => aprintln!(
            "Tuned band {} to {}.",
            band_name(band),
            thd75_repl::output::freq_mhz(landed.as_hz())
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// Read a single memory channel by number. Args: `<number>`.
pub(crate) async fn channel<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let Some(ch_str) = args.first() else {
        aprintln!("Usage: ch <channel number>");
        return;
    };
    let Ok(raw_channel) = ch_str.parse::<u16>() else {
        aprintln!(
            "{}",
            thd75_repl::output::error(format_args!("invalid channel number: {ch_str}"))
        );
        return;
    };
    let Ok(channel) = RegularChannel::new(raw_channel) else {
        aprintln!(
            "{}",
            thd75_repl::output::error(format_args!(
                "channel number must be {} through {}: {ch_str}",
                RegularChannel::MIN,
                RegularChannel::MAX
            ))
        );
        return;
    };

    match radio.get_regular_channel_record(channel).await {
        Ok(ch) => aprintln!(
            "{}",
            thd75_repl::output::channel_read(
                channel.as_raw(),
                ch.channel.receive_frequency.as_hz(),
            )
        ),
        Err(e) => aprintln!(
            "{}",
            thd75_repl::output::error(format_args!("reading channel {channel}: {e}"))
        ),
    }
}

/// List programmed memory channels in a range. Args: `[start] [end]`.
/// Default range is 0 through 19.
pub(crate) async fn channels<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let start = if let Some(value) = args.first() {
        let Ok(start) = value.parse::<u16>() else {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("invalid start channel number: {value}"))
            );
            return;
        };
        start
    } else {
        RegularChannel::MIN
    };
    let Ok(first) = RegularChannel::new(start) else {
        aprintln!(
            "{}",
            thd75_repl::output::error(format_args!(
                "start channel must be {} through {}: {start}",
                RegularChannel::MIN,
                RegularChannel::MAX
            ))
        );
        return;
    };

    let exclusive_limit = RegularChannel::MAX + 1;
    let end = if let Some(value) = args.get(1) {
        let Ok(end) = value.parse::<u16>() else {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("invalid end channel number: {value}"))
            );
            return;
        };
        end
    } else {
        start.saturating_add(20).min(exclusive_limit)
    };
    if end <= start {
        aprintln!(
            "{}",
            thd75_repl::output::error(format_args!(
                "end channel must be greater than start channel"
            ))
        );
        return;
    }
    if end > exclusive_limit {
        aprintln!(
            "{}",
            thd75_repl::output::error(format_args!(
                "exclusive end channel must be 1 through {exclusive_limit}: {end}"
            ))
        );
        return;
    }

    let last_channel = RegularChannel::new(end - 1)
        .unwrap_or_else(|_| unreachable!("validated exclusive channel range"));

    aprintln!("{}", thd75_repl::output::channels_reading(start, end - 1));
    match radio
        .read_regular_channel_records(RegularChannel::range_inclusive(first, last_channel))
        .await
    {
        Ok(channel_entries) => {
            if channel_entries.is_empty() {
                aprintln!("{}", thd75_repl::output::channels_summary(0));
            } else {
                for (num, ch) in &channel_entries {
                    aprintln!(
                        "{}",
                        thd75_repl::output::channel_read(
                            num.as_raw(),
                            ch.channel.receive_frequency.as_hz()
                        )
                    );
                }
                aprintln!(
                    "{}",
                    thd75_repl::output::channels_summary(channel_entries.len())
                );
            }
        }
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// VFO
// ---------------------------------------------------------------------------

/// Read the full VFO (variable frequency oscillator) state. Args: `[a|b]`.
/// Reports frequency, step size, transmit offset, and operating mode.
pub(crate) async fn vfo<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());

    match radio.get_frequency_full(band).await {
        Ok(ch) => {
            aprintln!(
                "{}",
                thd75_repl::output::frequency(band, ch.receive_frequency.as_hz())
            );
            aprintln!(
                "{}",
                thd75_repl::output::step_size_read(band, &ch.receive_step.to_string())
            );
            if ch.transmit_offset_or_frequency.as_hz() != 0 {
                aprintln!(
                    "{}",
                    thd75_repl::output::tx_offset(band, ch.transmit_offset_or_frequency.as_hz())
                );
            }
            aprintln!(
                "{}",
                thd75_repl::output::mode_read(band, &ch.mode.to_string())
            );
        }
        Err(e) => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("reading VFO: {e}"))
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Mode set
// ---------------------------------------------------------------------------

/// Read or set the operating mode on a band. Args: `[a|b] [mode_name]`.
/// Valid modes: fm, nfm, am, dv, lsb, usb, cw, dr, wfm.
pub(crate) async fn set_operating_mode<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        // Read mode.
        let band = parse_band(args.first());
        match radio.get_operating_mode(band).await {
            Ok(m) => aprintln!("{}", thd75_repl::output::mode_read(band, &m.to_string())),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    }

    let band = parse_band(args.first());
    let Some(&mode_arg) = args.get(1) else {
        return;
    };
    let mode = match mode_arg.to_lowercase().as_str() {
        "fm" => OperatingMode::Fm,
        "nfm" => OperatingMode::Nfm,
        "am" => OperatingMode::Am,
        "dv" => OperatingMode::Dv,
        "lsb" => OperatingMode::Lsb,
        "usb" => OperatingMode::Usb,
        "cw" => OperatingMode::Cw,
        "dr" => OperatingMode::Dr,
        "wfm" => OperatingMode::Wfm,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown mode: {other}"))
            );
            aprintln!("Valid modes: fm, nfm, am, dv, lsb, usb, cw, dr, wfm");
            return;
        }
    };

    match radio.set_operating_mode(band, mode).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::mode_set(band, &mode.to_string())),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Power set
// ---------------------------------------------------------------------------

/// Read or set the transmit power level. Args: `[a|b] [level]`.
/// Valid levels: high (5W), medium (2W), low (0.5W), extra-low (50mW).
pub(crate) async fn set_power<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        let band = parse_band(args.first());
        match radio.get_power_level(band).await {
            Ok(level) => aprintln!("{}", thd75_repl::output::power_read(band, level)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    }

    let band = parse_band(args.first());
    let Some(&level_arg) = args.get(1) else {
        return;
    };
    let level = match level_arg.to_lowercase().as_str() {
        "high" | "h" => kenwood_thd75::types::PowerLevel::High,
        "medium" | "med" | "m" => kenwood_thd75::types::PowerLevel::Medium,
        "low" | "l" => kenwood_thd75::types::PowerLevel::Low,
        "extra-low" | "el" | "elow" => kenwood_thd75::types::PowerLevel::ExtraLow,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown power level: {other}"))
            );
            aprintln!("Valid levels: high, medium, low, extra-low");
            return;
        }
    };

    match radio.set_power_level(band, level).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::power_set(band, level)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// VOX
// ---------------------------------------------------------------------------

/// Read or set voice-operated transmit (VOX) settings.
/// Args: `[on|off]`, `gain [0-9]`, or `delay [0-6]`.
#[expect(
    clippy::cognitive_complexity,
    reason = "Dispatch for the `vox` command: four sub-command arms (`gain`, `delay`, on/off, \
              read) each fork into parse-success/parse-failure plus radio-Ok/Err branches. \
              Splitting into helpers would require passing `&mut Radio<T>` around and multiply \
              lifetime noise without clarifying the straightforward command structure."
)]
pub(crate) async fn vox<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    match args.first().map(|s| s.to_lowercase()).as_deref() {
        Some("gain") => {
            if let Some(Ok(g)) = args.get(1).map(|s| s.parse::<u8>()) {
                match kenwood_thd75::types::VoxGain::try_from(g) {
                    Ok(gain) => match radio.set_vox_gain(gain).await {
                        Ok(()) => aprintln!("{}", thd75_repl::output::vox_gain_set(g)),
                        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
                    },
                    Err(e) => aprintln!(
                        "{}",
                        thd75_repl::output::error(format_args!("invalid VOX gain: {e}"))
                    ),
                }
            } else {
                match radio.get_vox_gain().await {
                    Ok(gain) => aprintln!("{}", thd75_repl::output::vox_gain_read(u8::from(gain))),
                    Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
                }
            }
        }
        Some("delay") => {
            if let Some(Ok(d)) = args.get(1).map(|s| s.parse::<u8>()) {
                match kenwood_thd75::types::VoxDelay::try_from(d) {
                    Ok(delay) => match radio.set_vox_delay(delay).await {
                        Ok(()) => aprintln!("{}", thd75_repl::output::vox_delay_set(d)),
                        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
                    },
                    Err(e) => aprintln!(
                        "{}",
                        thd75_repl::output::error(format_args!("invalid VOX delay: {e}"))
                    ),
                }
            } else {
                match radio.get_vox_delay().await {
                    Ok(delay) => {
                        aprintln!("{}", thd75_repl::output::vox_delay_read(u8::from(delay)));
                    }
                    Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
                }
            }
        }
        Some(s) => {
            if let Some(val) = parse_bool(s) {
                match radio.set_vox(val).await {
                    Ok(()) => aprintln!("{}", thd75_repl::output::vox(val)),
                    Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
                }
            } else {
                aprintln!("Usage: vox on|off, vox gain 0-9, vox delay 0-6");
            }
        }
        None => match radio.get_vox().await {
            Ok(on) => aprintln!("{}", thd75_repl::output::vox(on)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        },
    }
}

// ---------------------------------------------------------------------------
// Dual band
// ---------------------------------------------------------------------------

/// Read or set dual-band display mode. Args: `[on|off]`.
pub(crate) async fn dual_band<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        let mode = if val {
            BandMode::Dual
        } else {
            BandMode::Single
        };
        match radio.set_band_mode(mode).await {
            Ok(()) => aprintln!("{}", thd75_repl::output::dual_band(val)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    } else {
        match radio.get_band_mode().await {
            Ok(mode) => aprintln!(
                "{}",
                thd75_repl::output::dual_band(matches!(mode, BandMode::Dual))
            ),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// FM broadcast radio
// ---------------------------------------------------------------------------

/// Read the FM broadcast radio receiver state.
///
/// A supplied `on`/`off` argument updates Menu 700 through verified MCP
/// read-modify-write and reconnects to CAT before returning.
pub(crate) async fn fm_radio<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        match radio.set_fm_radio_via_mcp(val).await {
            Ok(()) => aprintln!("{}", thd75_repl::output::fm_radio(val)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    } else {
        match radio.get_fm_radio().await {
            Ok(on) => aprintln!("{}", thd75_repl::output::fm_radio(on)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Step size
// ---------------------------------------------------------------------------

/// Read or set the frequency step size. Args: `[a|b] [index]`.
/// Index 0-11 maps to 5, 6.25, 8.33, 9, 10, 12.5, 15, 20, 25, 30, 50, 100 kHz.
pub(crate) async fn step_size<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());

    if let Some(Ok(idx)) = args.get(1).map(|s| s.parse::<u8>()) {
        match kenwood_thd75::types::StepSize::try_from(idx) {
            Ok(step) => match radio.set_step_size(band, step).await {
                Ok(()) => aprintln!(
                    "{}",
                    thd75_repl::output::step_size_set(band, &step.to_string())
                ),
                Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
            },
            Err(e) => aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("invalid step index: {e}"))
            ),
        }
    } else {
        match radio.get_step_size(band).await {
            Ok(step) => aprintln!(
                "{}",
                thd75_repl::output::step_size_read(band, &step.to_string())
            ),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Recall channel
// ---------------------------------------------------------------------------

/// Recall a memory channel on a band. Args: `<a|b> <channel_number>`.
pub(crate) async fn recall<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        aprintln!("Usage: recall <a or b> <channel number>");
        return;
    }

    let band = parse_band(args.first());
    let Some(&ch_str) = args.get(1) else {
        return;
    };
    let Ok(raw_channel) = ch_str.parse::<u16>() else {
        aprintln!("Error: invalid channel number: {ch_str}");
        return;
    };
    let Ok(channel) = RegularChannel::new(raw_channel) else {
        aprintln!(
            "Error: channel number must be {} through {}: {ch_str}",
            RegularChannel::MIN,
            RegularChannel::MAX
        );
        return;
    };

    match radio.tune_channel(band, channel).await {
        Ok(()) => aprintln!("Band {} recalled channel {channel}", band_name(band)),
        Err(e) => aprintln!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// GPS settings
// ---------------------------------------------------------------------------

/// Set GPS receiver and PC output settings. Args: `<on|off> <on|off>`.
/// First argument controls the GPS receiver, second controls PC serial output.
pub(crate) async fn gps<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        aprintln!("Usage: gps <on|off> <on|off>");
        aprintln!("  First argument: GPS receiver on or off");
        aprintln!("  Second argument: PC output on or off");
        return;
    }

    let Some(gps_on) = args.first().and_then(|s| parse_bool(s)) else {
        aprintln!("Error: first argument must be on or off");
        return;
    };
    let Some(pc_on) = args.get(1).and_then(|s| parse_bool(s)) else {
        aprintln!("Error: second argument must be on or off");
        return;
    };

    let settings = GpsSettings::new(gps_on, pc_on);
    match radio.set_gps_settings(settings).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::gps_settings(settings)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// D-STAR commands
// ---------------------------------------------------------------------------

/// Read or set the D-STAR destination callsign (URCALL / "your call" field).
/// With no args, reads the current destination. With args, sets it.
pub(crate) async fn urcall<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.is_empty() {
        match radio.get_urcall().await {
            Ok(entry) => {
                aprintln!(
                    "{}",
                    thd75_repl::output::urcall_read(entry.callsign.as_str(), entry.suffix.as_str())
                );
            }
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    }

    let Some(&callsign) = args.first() else {
        return;
    };
    let suffix = args.get(1).copied().unwrap_or("");
    let callsign = match DstarCallsign::new(callsign) {
        Ok(callsign) => callsign,
        Err(e) => {
            aprintln!("{}", thd75_repl::output::error(e));
            return;
        }
    };
    let suffix = match DstarSuffix::new(suffix) {
        Ok(suffix) => suffix,
        Err(e) => {
            aprintln!("{}", thd75_repl::output::error(e));
            return;
        }
    };
    let callsign_display = callsign.as_str().to_owned();
    match radio.set_urcall(callsign, suffix).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::urcall_set(&callsign_display)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Set the D-STAR destination to CQCQCQ (general call to all stations).
pub(crate) async fn cq<T: Transport>(radio: &mut Radio<T>) {
    if !thd75_repl::confirm::tx_confirm() {
        return;
    }
    match radio.set_cq().await {
        Ok(()) => aprintln!("{}", thd75_repl::output::cq_set()),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Connect to a D-STAR reflector. Args: `<name> <module>`.
/// Example: `reflector REF030 C` connects to REF030 module C.
pub(crate) async fn reflector<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        aprintln!("Usage: reflector <name> <module>");
        aprintln!("Example: reflector REF030 C");
        return;
    }

    let (Some(&name), Some(&module_arg)) = (args.first(), args.get(1)) else {
        return;
    };
    let name_typed = match ReflectorCallsign::try_from_str(name) {
        Ok(name) => name,
        Err(e) => {
            aprintln!("{}", thd75_repl::output::error(e));
            return;
        }
    };
    let mut module_chars = module_arg.chars();
    let module = if let (Some(module), None) = (module_chars.next(), module_chars.next()) {
        match Module::try_from_char(module) {
            Ok(module) => module,
            Err(e) => {
                aprintln!("{}", thd75_repl::output::error(e));
                return;
            }
        }
    } else {
        aprintln!(
            "{}",
            thd75_repl::output::error("reflector module must be exactly one uppercase letter")
        );
        return;
    };
    match radio.prepare_reflector_link(name_typed, module).await {
        Ok(()) => aprintln!(
            "{}",
            thd75_repl::output::reflector_connected(name, module.as_char())
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Disconnect from the currently linked D-STAR reflector.
pub(crate) async fn unreflector<T: Transport>(radio: &mut Radio<T>) {
    match radio.prepare_reflector_unlink().await {
        Ok(()) => aprintln!("{}", thd75_repl::output::reflector_disconnected()),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Operation band
// ---------------------------------------------------------------------------

/// Read or set the operation band. Args: `[a|b]`.
///
/// The operation band is the band the radio's own controls act on
/// (BC command). With no argument, reads. Parsing is strict: a setter
/// must not guess, so anything other than `a` or `b` prints an error
/// instead of defaulting like the read-path `parse_band` does.
pub(crate) async fn band<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let Some(arg) = args.first() else {
        match radio.get_band().await {
            Ok(b) => aprintln!("{}", thd75_repl::output::operation_band_read(b)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    };
    let target = match arg.to_lowercase().as_str() {
        "a" => Band::A,
        "b" => Band::B,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown band {other:?}. Use a or b."))
            );
            return;
        }
    };
    match radio.set_band(target).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::operation_band_set(target)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Read or set the USB audio output source (IO command, radio Menu 102).
/// Args: `[af|if|detect]`.
///
/// `if` and `detect` stream the Band B IF or detection signal to a
/// connected computer instead of received audio. The radio accepts
/// them only in Single Band mode on Band B; on refusal a hint with
/// the required steps is printed after the error.
pub(crate) async fn ifout<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let Some(arg) = args.first() else {
        match radio.get_usb_audio_output().await {
            Ok(mode) => aprintln!("{}", thd75_repl::output::usb_output_read(mode)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    };
    let target = match arg.to_lowercase().as_str() {
        "audio" | "af" => UsbAudioOutput::Audio,
        "intermediate-frequency" | "if" => UsbAudioOutput::IntermediateFrequency,
        "detect" => UsbAudioOutput::Detect,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!(
                    "unknown output {other:?}. Use af, if, or detect."
                ))
            );
            return;
        }
    };
    match radio.set_usb_audio_output(target).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::usb_output_set(target)),
        Err(e) => {
            aprintln!("{}", thd75_repl::output::error(e));
            if !matches!(target, UsbAudioOutput::Audio) {
                aprintln!("IF and Detect require Single Band mode on Band B.");
                aprintln!("Type band b, then dualband off, and try again.");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Status dump
// ---------------------------------------------------------------------------

/// Dump a full snapshot of the radio's current state in a single
/// labelled block.
///
/// Covers the most common readouts blind operators ask for at the
/// start of a session: model, firmware, battery, clock, key lock,
/// dual-band, bluetooth, VOX, and for each band the frequency, mode,
/// transmit power, squelch, attenuator, and signal meter. Each read
/// is independent: a failing call prints a `"not available"` line
/// and the dump continues with the next field instead of aborting.
///
/// S-meter polls are defensive: the D75 firmware occasionally
/// returns spurious values on Band B while squelch is open (the
/// hardware-correct pattern is to gate SM reads on AI-pushed BY
/// events, not to poll them directly), so we accept whatever the
/// radio gives us and only fall through to `"not available"` on
/// an actual transport error. This keeps the command useful as a
/// one-shot snapshot.
#[expect(
    clippy::cognitive_complexity,
    reason = "`status` intentionally enumerates every per-field fetch so a single flaky CAT \
              command can't prevent the rest of the snapshot from printing. The linear list of \
              if-let branches is the simplest way to encode that independence; extracting \
              helpers would only hide the structure behind noise."
)]
pub(crate) async fn status<T: Transport>(radio: &mut Radio<T>) {
    aprintln!("Reading radio status, please wait.");

    if let Ok(info) = radio.identify().await {
        aprintln!("{}", thd75_repl::output::radio_model(info.model));
    } else {
        aprintln!("Radio model: not available");
    }
    if let Ok(fw) = radio.get_firmware_version().await {
        aprintln!("{}", thd75_repl::output::firmware_version(&fw));
    } else {
        aprintln!("Firmware version: not available");
    }
    if let Ok(level) = radio.get_battery_level().await {
        aprintln!("{}", thd75_repl::output::battery(level));
    } else {
        aprintln!("Battery level: not available");
    }
    if let Ok(time) = radio.get_real_time_clock().await {
        aprintln!("{}", thd75_repl::output::clock(time));
    } else {
        aprintln!("Radio clock: not available");
    }
    aprintln!("Key lock: not available (no verified CAT operation)");
    if let Ok(mode) = radio.get_band_mode().await {
        aprintln!(
            "{}",
            thd75_repl::output::dual_band(matches!(mode, BandMode::Dual))
        );
    } else {
        aprintln!("Dual band: not available");
    }
    if let Ok(enabled) = radio.get_bluetooth().await {
        aprintln!("{}", thd75_repl::output::bluetooth(enabled));
    } else {
        aprintln!("Bluetooth: not available");
    }
    if let Ok(on) = radio.get_vox().await {
        aprintln!("{}", thd75_repl::output::vox(on));
    } else {
        aprintln!("VOX: not available");
    }

    for band in [Band::A, Band::B] {
        let name = band_name(band);
        if let Ok(frequency) = radio.get_frequency(band).await {
            aprintln!("{}", thd75_repl::output::frequency(band, frequency.as_hz()));
        } else {
            aprintln!("Band {name} frequency: not available");
        }
        if let Ok(m) = radio.get_operating_mode(band).await {
            aprintln!("{}", thd75_repl::output::mode_read(band, &m.to_string()));
        } else {
            aprintln!("Band {name} mode: not available");
        }
        if let Ok(level) = radio.get_power_level(band).await {
            aprintln!("{}", thd75_repl::output::power_read(band, level));
        } else {
            aprintln!("Band {name} power: not available");
        }
        if let Ok(sq) = radio.get_squelch(band).await {
            aprintln!("{}", thd75_repl::output::squelch_read(band, u8::from(sq)));
        } else {
            aprintln!("Band {name} squelch level: not available");
        }
        if let Ok(on) = radio.get_attenuator(band).await {
            aprintln!("{}", thd75_repl::output::attenuator(band, on));
        } else {
            aprintln!("Band {name} attenuator: not available");
        }
        if let Ok(reading) = radio.get_smeter(band).await {
            aprintln!("{}", thd75_repl::output::smeter(band, &reading.to_string()));
        } else {
            aprintln!("Band {name} S-meter: not available");
        }
    }

    aprintln!("Status read complete.");
}
