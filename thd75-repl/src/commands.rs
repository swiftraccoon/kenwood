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

/// Format frequency in MHz for natural speech output.
#[allow(clippy::cast_precision_loss)]
fn fmt_freq_mhz(hz: u32) -> String {
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

/// Format a battery level for screen reader speech.
const fn fmt_battery(level: kenwood_thd75::types::BatteryLevel) -> &'static str {
    use kenwood_thd75::types::BatteryLevel;
    match level {
        BatteryLevel::Empty => "empty",
        BatteryLevel::OneThird => "one third",
        BatteryLevel::TwoThirds => "two thirds",
        BatteryLevel::Full => "full",
        BatteryLevel::Charging => "charging",
    }
}

/// Format a power level for screen reader speech.
const fn fmt_power(level: kenwood_thd75::types::PowerLevel) -> &'static str {
    use kenwood_thd75::types::PowerLevel;
    match level {
        PowerLevel::High => "high, 5 watts",
        PowerLevel::Medium => "medium, 2 watts",
        PowerLevel::Low => "low, half watt",
        PowerLevel::ExtraLow => "extra-low, 50 milliwatts",
    }
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
// Help
// ---------------------------------------------------------------------------

/// Print the CAT mode help text listing all available commands.
pub(crate) fn help() {
    println!("-- Information --");
    println!("help: Show this help text");
    println!("id: Radio model and identification");
    println!("battery: Battery charge level");
    println!("clock: Radio real-time clock");
    println!("quit: Exit the program");
    println!("-- Reading radio state --");
    println!("freq: Frequency on band A (or: freq b)");
    println!("mode: Operating mode on band A (or: mode b)");
    println!("squelch: Squelch level on band A (or: squelch b)");
    println!("power: Transmit power on band A (or: power b)");
    println!("att (attenuator): Attenuator on or off (or: att b)");
    println!("meter: Signal strength meter, S0 to S9 (or: meter b)");
    println!("lock: Key lock on or off");
    println!("dualband: Dual band display on or off");
    println!("bt (bluetooth): Bluetooth on or off");
    println!("vox: Voice-operated transmit on or off");
    println!("fm: FM broadcast radio on or off");
    println!("vfo: Full variable frequency oscillator state (or: vfo b)");
    println!("-- Changing settings --");
    println!("mode a fm: Set mode. Options: fm, nfm, am, dv, lsb, usb, cw");
    println!("squelch a 3: Set squelch level, 0 through 5");
    println!("power a high: Set power. Options: high, medium, low, extra-low");
    println!("att a on: Set attenuator on or off");
    println!("lock on: Set key lock on or off");
    println!("dualband on: Set dual band on or off");
    println!("bt on: Set Bluetooth on or off");
    println!("vox on: Set voice-operated transmit on or off");
    println!("vox gain 5: Set voice-operated transmit gain, 0 through 9");
    println!("vox delay 3: Set voice-operated transmit delay, 0 through 6");
    println!("fm on: Set FM broadcast radio on or off");
    println!("step a 5: Set frequency step size by index, 0 through 11");
    println!("-- Tuning --");
    println!("up: Step frequency up on band A (or: up b)");
    println!("down: Step frequency down on band A (or: down b)");
    println!("tune a 146.520: Tune band A to a frequency in megahertz");
    println!("recall a 5: Recall memory channel 5 on band A");
    println!("-- Memory channels --");
    println!("ch 5: Read memory channel 5");
    println!("channels: List programmed channels, default 0 through 19");
    println!("channels 0 100: List programmed channels 0 through 99");
    println!("-- D-STAR digital voice --");
    println!("urcall: Read destination callsign (D-STAR your-call field)");
    println!("urcall W1AW: Set destination callsign");
    println!("cq: Set destination to CQ CQ CQ (general call)");
    println!("reflector REF030 C: Connect to reflector, module C");
    println!("unreflector: Disconnect from reflector");
    println!("-- GPS --");
    println!("gps on on: Set GPS receiver on, PC output on");
    println!("gps off off: Set GPS receiver off, PC output off");
    println!("-- APRS packet radio mode --");
    println!("aprs start MYCALL 7: Enter APRS mode with callsign and SSID");
    println!("-- D-STAR gateway mode --");
    println!("dstar start MYCALL: Enter D-STAR gateway mode with callsign");
}

/// Print the APRS mode help text listing APRS-specific commands.
pub(crate) fn aprs_help() {
    println!("You are in APRS packet radio mode.");
    println!("-- APRS commands --");
    println!("listen: Check for the next APRS event");
    println!("msg W1AW Hello there: Send an APRS message to a station");
    println!("beacon: Send a status beacon");
    println!("aprs stop: Leave APRS mode, return to normal radio control");
    println!("quit: Exit the program");
}

/// Print the D-STAR gateway mode help text.
pub(crate) fn dstar_help() {
    println!("You are in D-STAR digital voice gateway mode.");
    println!("-- D-STAR gateway commands --");
    println!("listen: Check for the next D-STAR event");
    println!("heard: List recently heard stations");
    println!("status: Modem buffer and transmit status");
    println!("dstar stop: Leave D-STAR mode, return to normal radio control");
    println!("quit: Exit the program");
}

// ---------------------------------------------------------------------------
// Info
// ---------------------------------------------------------------------------

/// Print the radio model identification (ID command).
pub(crate) async fn identify<T: Transport>(radio: &mut Radio<T>) {
    match radio.identify().await {
        Ok(info) => println!("Radio model: {}", info.model),
        Err(e) => println!("Error: {e}"),
    }
}

/// Print the battery charge level (BL command).
pub(crate) async fn battery<T: Transport>(radio: &mut Radio<T>) {
    match radio.get_battery_level().await {
        Ok(level) => println!("Battery level: {}", fmt_battery(level)),
        Err(e) => println!("Error: {e}"),
    }
}

/// Print the radio's real-time clock (RT command).
pub(crate) async fn clock<T: Transport>(radio: &mut Radio<T>) {
    match radio.get_real_time_clock().await {
        Ok(time) => println!("Radio clock: {time}"),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Frequency / Mode / Squelch / Power
// ---------------------------------------------------------------------------

/// Read the current frequency on a band. Args: `[a|b]`, default A.
pub(crate) async fn frequency<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.get_frequency(band).await {
        Ok(ch) => println!(
            "Band {} frequency: {}",
            band_name(band),
            fmt_freq_mhz(ch.rx_frequency.as_hz())
        ),
        Err(e) => println!("Error: {e}"),
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
                Ok(()) => println!("Band {} squelch set to {level}", band_name(band)),
                Err(e) => println!("Error: {e}"),
            },
            Err(e) => println!("Error: invalid squelch level: {e}"),
        }
        return;
    }

    match radio.get_squelch(band).await {
        Ok(sq) => println!("Band {} squelch level: {}", band_name(band), u8::from(sq)),
        Err(e) => println!("Error: {e}"),
    }
}

/// Read the signal strength meter on a band. Args: `[a|b]`.
pub(crate) async fn smeter<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.get_smeter(band).await {
        Ok(reading) => println!("Band {} S-meter: {reading}", band_name(band)),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Lock / Bluetooth / Attenuator
// ---------------------------------------------------------------------------

/// Read or set the key lock state. Args: `[on|off]`.
pub(crate) async fn lock<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        match radio.set_lock(val).await {
            Ok(()) => println!("Key lock: {}", if val { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
        }
    } else {
        match radio.get_lock().await {
            Ok(locked) => println!("Key lock: {}", if locked { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
        }
    }
}

/// Read or set Bluetooth state. Args: `[on|off]`.
pub(crate) async fn bluetooth<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if let Some(val) = args.first().and_then(|s| parse_bool(s)) {
        match radio.set_bluetooth(val).await {
            Ok(()) => println!("Bluetooth: {}", if val { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
        }
    } else {
        match radio.get_bluetooth().await {
            Ok(enabled) => println!("Bluetooth: {}", if enabled { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
        }
    }
}

/// Read or set the attenuator on a band. Args: `[a|b] [on|off]`.
pub(crate) async fn attenuator<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());

    if let Some(val) = args.get(1).and_then(|s| parse_bool(s)) {
        match radio.set_attenuator(band, val).await {
            Ok(()) => println!(
                "Band {} attenuator: {}",
                band_name(band),
                if val { "on" } else { "off" }
            ),
            Err(e) => println!("Error: {e}"),
        }
        return;
    }

    match radio.get_attenuator(band).await {
        Ok(on) => println!(
            "Band {} attenuator: {}",
            band_name(band),
            if on { "on" } else { "off" }
        ),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Frequency stepping
// ---------------------------------------------------------------------------

/// Step frequency up by one increment, then read back. Args: `[a|b]`.
pub(crate) async fn step_up<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.frequency_up(band).await {
        Ok(()) => {
            // Read back the new frequency.
            match radio.get_frequency(band).await {
                Ok(ch) => println!(
                    "Band {} stepped up to {}",
                    band_name(band),
                    fmt_freq_mhz(ch.rx_frequency.as_hz())
                ),
                Err(e) => println!("Error: stepped up but could not read back frequency: {e}"),
            }
        }
        Err(e) => println!("Error: {e}"),
    }
}

/// Step frequency down by one increment and read back. Args: `[a|b]`.
pub(crate) async fn step_down<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    match radio.frequency_down(band).await {
        Ok(ch) => println!(
            "Band {} stepped down to {}",
            band_name(band),
            fmt_freq_mhz(ch.rx_frequency.as_hz())
        ),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Tuning
// ---------------------------------------------------------------------------

/// Tune a band to a specific frequency in megahertz. Args: `<a|b> <mhz>`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) async fn tune<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        println!("Usage: tune <a or b> <frequency in megahertz>");
        println!("Example: tune a 146.520");
        return;
    }

    let band = parse_band(args.first());
    let freq_str = args[1];

    let Ok(mhz) = freq_str.parse::<f64>() else {
        println!("Error: invalid frequency: {freq_str}");
        return;
    };

    let hz = (mhz * 1_000_000.0) as u32;
    let freq = kenwood_thd75::types::Frequency::new(hz);
    match radio.tune_frequency(band, freq).await {
        Ok(()) => println!("Band {} tuned to {}", band_name(band), fmt_freq_mhz(hz)),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// Read a single memory channel by number. Args: `<number>`.
pub(crate) async fn channel<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let Some(ch_str) = args.first() else {
        println!("Usage: ch <channel number>");
        return;
    };
    let Ok(ch_num) = ch_str.parse::<u16>() else {
        println!("Error: invalid channel number: {ch_str}");
        return;
    };

    match radio.read_channel(ch_num).await {
        Ok(ch) => {
            println!(
                "Channel {ch_num}: {}",
                fmt_freq_mhz(ch.rx_frequency.as_hz()),
            );
        }
        Err(e) => println!("Error reading channel {ch_num}: {e}"),
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

    println!("Reading channels {start} through {}, please wait.", end - 1);
    match radio.read_channels(start..end).await {
        Ok(list) => {
            if list.is_empty() {
                println!("No programmed channels in that range.");
            } else {
                for (num, ch) in &list {
                    println!("Channel {num}: {}", fmt_freq_mhz(ch.rx_frequency.as_hz()));
                }
                println!("{} programmed channels found.", list.len());
            }
        }
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// VFO
// ---------------------------------------------------------------------------

/// Read the full VFO (variable frequency oscillator) state. Args: `[a|b]`.
/// Reports frequency, step size, transmit offset, and operating mode.
pub(crate) async fn vfo<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    let band = parse_band(args.first());
    let bn = band_name(band);

    match radio.get_frequency_full(band).await {
        Ok(ch) => {
            println!(
                "Band {bn} frequency: {}",
                fmt_freq_mhz(ch.rx_frequency.as_hz())
            );
            println!("Band {bn} step size: {}", ch.step_size);
            if ch.tx_offset.as_hz() != 0 {
                println!("Band {bn} transmit offset: {} hertz", ch.tx_offset.as_hz());
            }
        }
        Err(e) => {
            println!("Error reading VFO: {e}");
            return;
        }
    }

    // Read mode separately (not embedded in ChannelMemory).
    match radio.get_mode(band).await {
        Ok(m) => println!("Band {bn} mode: {m}"),
        Err(e) => println!("Error reading mode: {e}"),
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
            Ok(m) => println!("Band {} mode: {m}", band_name(band)),
            Err(e) => println!("Error: {e}"),
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
            println!("Error: unknown mode: {other}");
            println!("Valid modes: fm, nfm, am, dv, lsb, usb, cw, dr, wfm");
            return;
        }
    };

    match radio.set_mode(band, mode).await {
        Ok(()) => println!("Band {} mode set to {mode}", band_name(band)),
        Err(e) => println!("Error: {e}"),
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
            Ok(level) => println!("Band {} power: {}", band_name(band), fmt_power(level)),
            Err(e) => println!("Error: {e}"),
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
            println!("Error: unknown power level: {other}");
            println!("Valid levels: high, medium, low, extra-low");
            return;
        }
    };

    match radio.set_power_level(band, level).await {
        Ok(()) => println!("Band {} power set to {}", band_name(band), fmt_power(level)),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// VOX
// ---------------------------------------------------------------------------

/// Read or set voice-operated transmit (VOX) settings.
/// Args: `[on|off]`, `gain [0-9]`, or `delay [0-6]`.
pub(crate) async fn vox<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    match args.first().map(|s| s.to_lowercase()).as_deref() {
        Some("gain") => {
            if let Some(Ok(g)) = args.get(1).map(|s| s.parse::<u8>()) {
                match kenwood_thd75::types::VoxGain::try_from(g) {
                    Ok(gain) => match radio.set_vox_gain(gain).await {
                        Ok(()) => println!("VOX gain set to {g}"),
                        Err(e) => println!("Error: {e}"),
                    },
                    Err(e) => println!("Error: invalid VOX gain: {e}"),
                }
            } else {
                match radio.get_vox_gain().await {
                    Ok(gain) => println!("VOX gain: {}", u8::from(gain)),
                    Err(e) => println!("Error: {e}"),
                }
            }
        }
        Some("delay") => {
            if let Some(Ok(d)) = args.get(1).map(|s| s.parse::<u8>()) {
                match kenwood_thd75::types::VoxDelay::try_from(d) {
                    Ok(delay) => match radio.set_vox_delay(delay).await {
                        Ok(()) => println!("VOX delay set to {d}"),
                        Err(e) => println!("Error: {e}"),
                    },
                    Err(e) => println!("Error: invalid VOX delay: {e}"),
                }
            } else {
                match radio.get_vox_delay().await {
                    Ok(delay) => println!("VOX delay: {}", u8::from(delay)),
                    Err(e) => println!("Error: {e}"),
                }
            }
        }
        Some(s) => {
            if let Some(val) = parse_bool(s) {
                match radio.set_vox(val).await {
                    Ok(()) => println!("VOX: {}", if val { "on" } else { "off" }),
                    Err(e) => println!("Error: {e}"),
                }
            } else {
                println!("Usage: vox on|off, vox gain 0-9, vox delay 0-6");
            }
        }
        None => match radio.get_vox().await {
            Ok(on) => println!("VOX: {}", if on { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
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
            Ok(()) => println!("Dual band: {}", if val { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
        }
    } else {
        match radio.get_dual_band().await {
            Ok(on) => println!("Dual band: {}", if on { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
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
            Ok(()) => println!("FM radio: {}", if val { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
        }
    } else {
        match radio.get_fm_radio().await {
            Ok(on) => println!("FM radio: {}", if on { "on" } else { "off" }),
            Err(e) => println!("Error: {e}"),
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
                Ok(()) => println!("Band {} step size set to {step}", band_name(band)),
                Err(e) => println!("Error: {e}"),
            },
            Err(e) => println!("Error: invalid step index: {e}"),
        }
    } else {
        match radio.get_step_size(band).await {
            Ok((_b, step)) => println!("Band {} step size: {step}", band_name(band)),
            Err(e) => println!("Error: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Recall channel
// ---------------------------------------------------------------------------

/// Recall a memory channel on a band. Args: `<a|b> <channel_number>`.
pub(crate) async fn recall<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        println!("Usage: recall <a or b> <channel number>");
        return;
    }

    let band = parse_band(args.first());
    let Ok(ch) = args[1].parse::<u16>() else {
        println!("Error: invalid channel number: {}", args[1]);
        return;
    };

    match radio.tune_channel(band, ch).await {
        Ok(()) => println!("Band {} recalled channel {ch}", band_name(band)),
        Err(e) => println!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// GPS config
// ---------------------------------------------------------------------------

/// Set GPS receiver and PC output configuration. Args: `<on|off> <on|off>`.
/// First argument controls the GPS receiver, second controls PC serial output.
pub(crate) async fn gps<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        println!("Usage: gps <on|off> <on|off>");
        println!("  First argument: GPS receiver on or off");
        println!("  Second argument: PC output on or off");
        return;
    }

    let Some(gps_on) = parse_bool(args[0]) else {
        println!("Error: first argument must be on or off");
        return;
    };
    let Some(pc_on) = parse_bool(args[1]) else {
        println!("Error: second argument must be on or off");
        return;
    };

    match radio.set_gps_config(gps_on, pc_on).await {
        Ok(()) => println!(
            "GPS: {}, PC output: {}",
            if gps_on { "on" } else { "off" },
            if pc_on { "on" } else { "off" }
        ),
        Err(e) => println!("Error: {e}"),
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
                if suffix.is_empty() {
                    println!("Destination callsign: {call}");
                } else {
                    println!("Destination callsign: {call} suffix {suffix}");
                }
            }
            Err(e) => println!("Error: {e}"),
        }
        return;
    }

    let callsign = args[0];
    let suffix = args.get(1).unwrap_or(&"");
    match radio.set_urcall(callsign, suffix).await {
        Ok(()) => println!("Destination callsign set to {callsign}"),
        Err(e) => println!("Error: {e}"),
    }
}

/// Set the D-STAR destination to CQCQCQ (general call to all stations).
pub(crate) async fn cq<T: Transport>(radio: &mut Radio<T>) {
    match radio.set_cq().await {
        Ok(()) => println!("Destination set to CQCQCQ"),
        Err(e) => println!("Error: {e}"),
    }
}

/// Connect to a D-STAR reflector. Args: `<name> <module>`.
/// Example: `reflector REF030 C` connects to REF030 module C.
pub(crate) async fn reflector<T: Transport>(radio: &mut Radio<T>, args: &[&str]) {
    if args.len() < 2 {
        println!("Usage: reflector <name> <module>");
        println!("Example: reflector REF030 C");
        return;
    }

    let name = args[0];
    let module = args[1].chars().next().unwrap_or('A');
    match radio.connect_reflector(name, module).await {
        Ok(()) => println!("Connected to {name} module {module}"),
        Err(e) => println!("Error: {e}"),
    }
}

/// Disconnect from the currently linked D-STAR reflector.
pub(crate) async fn unreflector<T: Transport>(radio: &mut Radio<T>) {
    match radio.disconnect_reflector().await {
        Ok(()) => println!("Disconnected from reflector"),
        Err(e) => println!("Error: {e}"),
    }
}
