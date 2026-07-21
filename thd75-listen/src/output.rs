//! Pure format functions for every user-facing listener string.
//!
//! Same accessibility contract as the `thd75-repl` output module: one
//! self-contained spoken line per datum, label-colon-value shapes,
//! units spelled out, `Error: ` prefix on errors. Tests run every
//! string through `thd75_repl::lint`.

use if_dsp::DemodMode;

/// `TH-D75 IF listener, version {v}.`
#[must_use]
pub fn banner(version: &str) -> String {
    format!("TH-D75 IF listener, version {version}.")
}

/// `{x} megahertz` with trailing zeros trimmed (`435.64 megahertz`).
#[must_use]
pub fn mhz(freq_hz: u32) -> String {
    let mhz = f64::from(freq_hz) / 1_000_000.0;
    let s = format!("{mhz:.6}");
    let s = s.trim_end_matches('0');
    let s = if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_string()
    };
    format!("{s} megahertz")
}

/// `Listening on {mhz}, {mode} mode. Type help for commands.`
#[must_use]
pub fn ready(freq_hz: u32, mode: DemodMode) -> String {
    format!(
        "Listening on {}, {mode} mode. Type help for commands.",
        mhz(freq_hz)
    )
}

/// `Tuned to {mhz}.`
#[must_use]
pub fn tuned(freq_hz: u32) -> String {
    format!("Tuned to {}.", mhz(freq_hz))
}

/// `Mode set to {mode}.`
#[must_use]
pub fn mode_set(mode: DemodMode) -> String {
    format!("Mode set to {mode}.")
}

/// `Filter width set to {x} kilohertz.`
#[must_use]
pub fn filter_set(hz: f32) -> String {
    format!("Filter width set to {} kilohertz.", hz / 1_000.0)
}

/// `Volume set to {p} percent.`
#[must_use]
pub fn volume_set(percent: u8) -> String {
    format!("Volume set to {percent} percent.")
}

/// `Signal: {x} decibels below full scale.`
///
/// `db` is a dBFS value (zero or negative in practice); the spoken
/// form uses the magnitude. Values are clamped to the sane readout
/// range before formatting.
#[must_use]
pub fn signal(db: f32) -> String {
    let below = (-db).clamp(0.0, 120.0);
    format!("Signal: {below:.1} decibels below full scale.")
}

/// Multi-line status block; every line is self-contained.
#[must_use]
pub fn status(
    freq_hz: u32,
    mode: DemodMode,
    filter_hz: f32,
    volume: u8,
    stream: &StreamHealth,
) -> String {
    format!(
        "Frequency: {}\nMode: {mode}\nFilter width: {} kilohertz\nVolume: {volume} percent\nAudio input blocks: {}\nAudio output blocks: {}\nAudio input overruns: {}\nAudio output underruns: {}",
        mhz(freq_hz),
        filter_hz / 1_000.0,
        stream.input_blocks,
        stream.output_blocks,
        stream.overruns,
        stream.underruns,
    )
}

/// Audio stream health counters for the status block.
#[derive(Debug, Default, Clone, Copy)]
pub struct StreamHealth {
    /// Input callbacks delivered so far.
    pub input_blocks: usize,
    /// Output callbacks served so far.
    pub output_blocks: usize,
    /// Input chunks dropped because processing fell behind.
    pub overruns: usize,
    /// Output callbacks that ran short of samples.
    pub underruns: usize,
}

/// `Radio restored. Goodbye.`
#[must_use]
pub const fn goodbye() -> &'static str {
    "Radio restored. Goodbye."
}

/// `Warning: could not restore {item}. Check the radio.`
#[must_use]
pub fn restore_warning(item: &str) -> String {
    format!("Warning: could not restore {item}. Check the radio.")
}

/// `Error: {message}` - the canonical error prefix.
#[must_use]
pub fn error(e: impl std::fmt::Display) -> String {
    format!("Error: {e}")
}

/// One-shot notice when the audio output falls behind.
#[must_use]
pub const fn underrun_notice() -> &'static str {
    "Audio output fell behind once. Continuing."
}

/// The command list, one command per line.
#[must_use]
pub const fn help() -> &'static str {
    "tune 435.640: Tune the radio, megahertz, 5 kilohertz steps
mode usb: Set demodulation. Options: usb, lsb, cw, am
filter 2.4: Set filter width in kilohertz
volume 50: Set audio volume, 0 through 100
signal: Report the received signal level
status: Report tuning, mode, filter, volume, stream health
help: Show this list
quit: Restore the radio and exit"
}

/// Guidance when the radio's audio interface cannot be found.
#[must_use]
pub const fn no_device_guidance() -> &'static str {
    "The radio's audio device was not found.
The device is named ADC stream IN, not TH-D75.
Check that the USB cable is a data cable, not a charging cable.
Check that the radio is on and Menu 980 is COM plus AF IF Output.
A radio that only charges never appears as an audio device."
}

/// Guidance when no TH-D75 USB serial port exists.
#[must_use]
pub const fn no_serial_guidance() -> &'static str {
    "No TH-D75 USB serial port was found.
The IF stream exists only on the USB connector, not Bluetooth.
Check that the radio is on and the cable is a data cable.
A radio that only charges never shows a serial port."
}

#[cfg(test)]
mod tests {
    use super::*;
    use thd75_repl::lint;

    fn assert_lint(s: &str) {
        if let Err(v) = lint::check_output(s) {
            unreachable!("line {s:?} failed accessibility lint: {v:?}");
        }
    }

    #[test]
    fn mhz_trims_trailing_zeros() {
        assert_eq!(mhz(435_640_000), "435.64 megahertz");
        assert_eq!(mhz(146_000_000), "146.0 megahertz");
        assert_eq!(mhz(14_074_000), "14.074 megahertz");
    }

    #[test]
    fn fixed_strings_lint() {
        assert_lint(&banner("0.1.0"));
        assert_lint(goodbye());
        assert_lint(underrun_notice());
        assert_lint(help());
        assert_lint(no_device_guidance());
        assert_lint(no_serial_guidance());
    }

    #[test]
    fn tuning_and_mode_lines() {
        assert_eq!(tuned(435_640_000), "Tuned to 435.64 megahertz.");
        assert_eq!(mode_set(DemodMode::Usb), "Mode set to USB.");
        assert_eq!(mode_set(DemodMode::Lsb), "Mode set to LSB.");
        assert_lint(&tuned(435_640_000));
        assert_lint(&mode_set(DemodMode::Cw));
        assert_lint(&ready(435_640_000, DemodMode::Usb));
    }

    #[test]
    fn filter_volume_signal_lines() {
        assert_eq!(filter_set(2_400.0), "Filter width set to 2.4 kilohertz.");
        assert_eq!(volume_set(50), "Volume set to 50 percent.");
        assert_eq!(signal(-12.34), "Signal: 12.3 decibels below full scale.");
        assert_eq!(signal(5.0), "Signal: 0.0 decibels below full scale.");
        assert_lint(&filter_set(2_400.0));
        assert_lint(&volume_set(0));
        assert_lint(&signal(-60.0));
    }

    #[test]
    fn status_block_lints_per_line() {
        let health = StreamHealth {
            input_blocks: 500,
            output_blocks: 498,
            overruns: 0,
            underruns: 2,
        };
        let s = status(435_640_000, DemodMode::Usb, 2_600.0, 40, &health);
        assert!(s.contains("Frequency: 435.64 megahertz"), "{s}");
        assert!(s.contains("Audio input blocks: 500"), "{s}");
        assert!(s.contains("Audio output underruns: 2"), "{s}");
        assert_lint(&s);
    }

    #[test]
    fn errors_and_warnings_lint() {
        assert_lint(&error("something broke"));
        assert_lint(&restore_warning("USB audio output"));
    }
}
