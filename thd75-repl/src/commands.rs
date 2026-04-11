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
//!   adjacent lines. No indented sub-items — repeat the label.
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
use kenwood_thd75::types::{Band, Mode};
use thd75_repl::aprintln;

/// Parse a band argument ("a" or "b"), defaulting to A.
fn parse_band(s: Option<&&str>) -> Band {
    match s.map(|s| s.to_lowercase()).as_deref() {
        Some("b" | "1") => Band::B,
        _ => Band::A,
    }
}

/// Human-readable band name.
fn band_name(band: Band) -> &'static str {
    if band == Band::A { "A" } else { "B" }
}

/// Format a duration for screen reader speech.
pub(crate) fn fmt_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs} seconds")
    } else if secs < 3600 {
        format!("{} minutes", secs / 60)
    } else {
        format!("{} hours", secs / 3600)
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
        Ok(ch) => aprintln!(
            "{}",
            thd75_repl::output::frequency(band, ch.rx_frequency.as_hz())
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Read or set the squelch level. Args: `[a|b] [level]`.
/// With one arg, reads. With two, sets (level 0-5).
pub(crate) async fn squelch<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());

    // Second arg present and numeric = set squelch.
    if args.len() >= 2
        && let Ok(level) = args[1].parse::<u8>()
    {
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

/// Read or set the key lock state. Args: `[on|off]`.
pub(crate) async fn lock<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        match radio.set_lock(val).await {
            Ok(()) => aprintln!("{}", thd75_repl::output::key_lock(val)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    } else {
        match radio.get_lock().await {
            Ok(locked) => aprintln!("{}", thd75_repl::output::key_lock(locked)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    }
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
    match radio.frequency_up(band).await {
        Ok(()) => match radio.get_frequency(band).await {
            Ok(ch) => aprintln!(
                "{}",
                thd75_repl::output::stepped_up(band, ch.rx_frequency.as_hz())
            ),
            Err(e) => aprintln!(
                "{}",
                thd75_repl::output::error(format_args!(
                    "stepped up but could not read back frequency: {e}"
                ))
            ),
        },
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Step frequency down by one increment and read back. Args: `[a|b]`.
pub(crate) async fn step_down<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.frequency_down(band).await {
        Ok(ch) => aprintln!(
            "{}",
            thd75_repl::output::stepped_down(band, ch.rx_frequency.as_hz())
        ),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

// ---------------------------------------------------------------------------
// Tuning
// ---------------------------------------------------------------------------

/// Tune a band to a specific frequency in megahertz. Args: `<a|b> <mhz>`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) async fn tune<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        aprintln!("Usage: tune <a or b> <frequency in megahertz>");
        aprintln!("Example: tune a 146.520");
        return;
    }

    let band = parse_band(args.first());
    let freq_str = args[1];

    let Ok(mhz) = freq_str.parse::<f64>() else {
        aprintln!("Error: invalid frequency: {freq_str}");
        return;
    };

    let hz = (mhz * 1_000_000.0) as u32;
    let freq = kenwood_thd75::types::Frequency::new(hz);
    match radio.tune_frequency(band, freq).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::tuned_to(band, hz)),
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
    let Ok(ch_num) = ch_str.parse::<u16>() else {
        aprintln!(
            "{}",
            thd75_repl::output::error(format_args!("invalid channel number: {ch_str}"))
        );
        return;
    };

    match radio.read_channel(ch_num).await {
        Ok(ch) => aprintln!(
            "{}",
            thd75_repl::output::channel_read(ch_num, ch.rx_frequency.as_hz())
        ),
        Err(e) => aprintln!(
            "{}",
            thd75_repl::output::error(format_args!("reading channel {ch_num}: {e}"))
        ),
    }
}

/// List programmed memory channels in a range. Args: `[start] [end]`.
/// Default range is 0 through 19.
pub(crate) async fn channels<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let start: u16 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let end: u16 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(start + 20);

    aprintln!("{}", thd75_repl::output::channels_reading(start, end - 1));
    match radio.read_channels(start..end).await {
        Ok(list) => {
            if list.is_empty() {
                aprintln!("{}", thd75_repl::output::channels_summary(0));
            } else {
                for (num, ch) in &list {
                    aprintln!(
                        "{}",
                        thd75_repl::output::channel_read(*num, ch.rx_frequency.as_hz())
                    );
                }
                aprintln!("{}", thd75_repl::output::channels_summary(list.len()));
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
                thd75_repl::output::frequency(band, ch.rx_frequency.as_hz())
            );
            aprintln!(
                "{}",
                thd75_repl::output::step_size_read(band, &ch.step_size.to_string())
            );
            if ch.tx_offset.as_hz() != 0 {
                aprintln!(
                    "{}",
                    thd75_repl::output::tx_offset(band, ch.tx_offset.as_hz())
                );
            }
        }
        Err(e) => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("reading VFO: {e}"))
            );
            return;
        }
    }

    // Read mode separately (not embedded in ChannelMemory).
    match radio.get_mode(band).await {
        Ok(m) => aprintln!("{}", thd75_repl::output::mode_read(band, &m.to_string())),
        Err(e) => aprintln!(
            "{}",
            thd75_repl::output::error(format_args!("reading mode: {e}"))
        ),
    }
}

// ---------------------------------------------------------------------------
// Mode set
// ---------------------------------------------------------------------------

/// Read or set the operating mode on a band. Args: `[a|b] [mode_name]`.
/// Valid modes: fm, nfm, am, dv, lsb, usb, cw, dr, wfm.
pub(crate) async fn set_mode<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        // Read mode.
        let band = parse_band(args.first());
        match radio.get_mode(band).await {
            Ok(m) => aprintln!("{}", thd75_repl::output::mode_read(band, &m.to_string())),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    }

    let band = parse_band(args.first());
    let mode = match args[1].to_lowercase().as_str() {
        "fm" => Mode::Fm,
        "nfm" => Mode::Nfm,
        "am" => Mode::Am,
        "dv" => Mode::Dv,
        "lsb" => Mode::Lsb,
        "usb" => Mode::Usb,
        "cw" => Mode::Cw,
        "dr" => Mode::Dr,
        "wfm" => Mode::Wfm,
        other => {
            aprintln!(
                "{}",
                thd75_repl::output::error(format_args!("unknown mode: {other}"))
            );
            aprintln!("Valid modes: fm, nfm, am, dv, lsb, usb, cw, dr, wfm");
            return;
        }
    };

    match radio.set_mode(band, mode).await {
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
    let level = match args[1].to_lowercase().as_str() {
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
#[allow(clippy::cognitive_complexity)]
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
        match radio.set_dual_band(val).await {
            Ok(()) => aprintln!("{}", thd75_repl::output::dual_band(val)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    } else {
        match radio.get_dual_band().await {
            Ok(on) => aprintln!("{}", thd75_repl::output::dual_band(on)),
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// FM broadcast radio
// ---------------------------------------------------------------------------

/// Read or set the FM broadcast radio receiver. Args: `[on|off]`.
pub(crate) async fn fm_radio<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        match radio.set_fm_radio(val).await {
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
            Ok((_b, step)) => aprintln!(
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
    let Ok(ch) = args[1].parse::<u16>() else {
        aprintln!("Error: invalid channel number: {}", args[1]);
        return;
    };

    match radio.tune_channel(band, ch).await {
        Ok(()) => aprintln!("Band {} recalled channel {ch}", band_name(band)),
        Err(e) => aprintln!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// GPS config
// ---------------------------------------------------------------------------

/// Set GPS receiver and PC output configuration. Args: `<on|off> <on|off>`.
/// First argument controls the GPS receiver, second controls PC serial output.
pub(crate) async fn gps<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        aprintln!("Usage: gps <on|off> <on|off>");
        aprintln!("  First argument: GPS receiver on or off");
        aprintln!("  Second argument: PC output on or off");
        return;
    }

    let Some(gps_on) = parse_bool(args[0]) else {
        aprintln!("Error: first argument must be on or off");
        return;
    };
    let Some(pc_on) = parse_bool(args[1]) else {
        aprintln!("Error: second argument must be on or off");
        return;
    };

    match radio.set_gps_config(gps_on, pc_on).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::gps_config(gps_on, pc_on)),
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
            Ok((call, suffix)) => {
                let call = call.trim();
                let suffix = suffix.trim();
                aprintln!("{}", thd75_repl::output::urcall_read(call, suffix));
            }
            Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
        }
        return;
    }

    let callsign = args[0];
    let suffix = args.get(1).unwrap_or(&"");
    match radio.set_urcall(callsign, suffix).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::urcall_set(callsign)),
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

    let name = args[0];
    let module = args[1].chars().next().unwrap_or('A');
    match radio.connect_reflector(name, module).await {
        Ok(()) => aprintln!("{}", thd75_repl::output::reflector_connected(name, module)),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
    }
}

/// Disconnect from the currently linked D-STAR reflector.
pub(crate) async fn unreflector<T: Transport>(radio: &mut Radio<T>) {
    match radio.disconnect_reflector().await {
        Ok(()) => aprintln!("{}", thd75_repl::output::reflector_disconnected()),
        Err(e) => aprintln!("{}", thd75_repl::output::error(e)),
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
/// is independent — a failing call prints a `"not available"` line
/// and the dump continues with the next field instead of aborting.
///
/// S-meter polls are defensive: the D75 firmware occasionally
/// returns spurious values on Band B while squelch is open
/// (`CLAUDE.local.md` §AI Mode), so we accept whatever the radio
/// gives us and only fall through to `"not available"` on an actual
/// transport error rather than gating the read behind AI push
/// events. This keeps the command useful as a one-shot snapshot.
#[allow(clippy::cognitive_complexity)]
pub(crate) async fn status<T: Transport>(radio: &mut Radio<T>) {
    aprintln!("Reading radio status, please wait.");

    if let Ok(info) = radio.identify().await {
        aprintln!("{}", thd75_repl::output::radio_model(info.model));
    } else {
        aprintln!("Radio model: not available");
    }
    if let Ok(fw) = radio.get_firmware_version().await {
        aprintln!("{}", thd75_repl::output::firmware_version(fw));
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
    if let Ok(locked) = radio.get_lock().await {
        aprintln!("{}", thd75_repl::output::key_lock(locked));
    } else {
        aprintln!("Key lock: not available");
    }
    if let Ok(on) = radio.get_dual_band().await {
        aprintln!("{}", thd75_repl::output::dual_band(on));
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
        if let Ok(ch) = radio.get_frequency(band).await {
            aprintln!(
                "{}",
                thd75_repl::output::frequency(band, ch.rx_frequency.as_hz())
            );
        } else {
            aprintln!("Band {name} frequency: not available");
        }
        if let Ok(m) = radio.get_mode(band).await {
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
