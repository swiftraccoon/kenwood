//! Pure format functions for every user-facing listener string.
//!
//! Same accessibility contract as the `thd75-repl` output module: one
//! self-contained spoken line per datum, label-colon-value shapes,
//! units spelled out, `Error: ` prefix on errors. Tests run every
//! string through `thd75_repl::lint`.

use if_dsp::DemodMode;

use crate::parser::{FilterWidth, ListenerFrequency, SignalLevel, Volume};

/// `TH-D75 IF listener, version {v}.`
#[must_use]
pub fn banner(version: &str) -> String {
    format!("TH-D75 IF listener, version {version}.")
}

/// `{x} megahertz` with trailing zeros trimmed (`435.64 megahertz`).
#[must_use]
pub fn mhz(frequency: ListenerFrequency) -> String {
    let mhz = f64::from(frequency.as_hz()) / 1_000_000.0;
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
pub fn ready(frequency: ListenerFrequency, mode: DemodMode) -> String {
    format!(
        "Listening on {}, {mode} mode. Type help for commands.",
        mhz(frequency)
    )
}

/// `Tuned to {mhz}.`
#[must_use]
pub fn tuned(frequency: ListenerFrequency) -> String {
    format!("Tuned to {}.", mhz(frequency))
}

/// `Mode set to {mode}.`
#[must_use]
pub fn mode_set(mode: DemodMode) -> String {
    format!("Mode set to {mode}.")
}

/// `Filter width set to {x} kilohertz.`
#[must_use]
pub fn filter_set(width: FilterWidth) -> String {
    format!("Filter width set to {} kilohertz.", width.hz() / 1_000.0)
}

/// `Volume set to {p} percent.`
#[must_use]
pub fn volume_set(volume: Volume) -> String {
    format!("Volume set to {} percent.", volume.percent())
}

/// `Signal: {x} decibels below full scale.`
///
/// The validated input carries a finite reading clamped to -120 through
/// zero dBFS; the spoken form uses its magnitude.
#[must_use]
pub fn signal(level: SignalLevel) -> String {
    let below = level.dbfs().abs();
    format!("Signal: {below:.1} decibels below full scale.")
}

/// Multi-line status block; every line is self-contained.
#[must_use]
pub fn status(
    frequency: ListenerFrequency,
    mode: DemodMode,
    filter: FilterWidth,
    volume: Volume,
    stream: &StreamHealth,
) -> String {
    format!(
        "Frequency: {}\nMode: {mode}\nFilter width: {} kilohertz\nVolume: {} percent\nAudio input blocks: {}\nAudio output blocks: {}\nAudio input overruns: {}\nAudio output underruns: {}",
        mhz(frequency),
        filter.hz() / 1_000.0,
        volume.percent(),
        stream.input_blocks,
        stream.output_blocks,
        stream.overruns,
        stream.underruns,
    )
}

/// Audio stream health counters for the status block.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn assert_lint(s: &str) {
        if let Err(v) = lint::check_output(s) {
            unreachable!("line {s:?} failed accessibility lint: {v:?}");
        }
    }

    #[test]
    fn mhz_trims_trailing_zeros() -> TestResult {
        assert_eq!(mhz("435.640".parse()?), "435.64 megahertz");
        assert_eq!(mhz("146.000".parse()?), "146.0 megahertz");
        assert_eq!(mhz("14.075".parse()?), "14.075 megahertz");
        Ok(())
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
    fn tuning_and_mode_lines() -> TestResult {
        let frequency = "435.640".parse()?;
        assert_eq!(tuned(frequency), "Tuned to 435.64 megahertz.");
        assert_eq!(mode_set(DemodMode::Usb), "Mode set to USB.");
        assert_eq!(mode_set(DemodMode::Lsb), "Mode set to LSB.");
        assert_lint(&tuned(frequency));
        assert_lint(&mode_set(DemodMode::Cw));
        assert_lint(&ready(frequency, DemodMode::Usb));
        Ok(())
    }

    #[test]
    fn filter_volume_signal_lines() -> TestResult {
        let width = FilterWidth::new(2400.0)?;
        assert_eq!(filter_set(width), "Filter width set to 2.4 kilohertz.");
        assert_eq!(volume_set(Volume::new(50)?), "Volume set to 50 percent.");
        assert_eq!(
            signal(SignalLevel::new(-12.34)?),
            "Signal: 12.3 decibels below full scale."
        );
        assert_eq!(
            signal(SignalLevel::new(5.0)?),
            "Signal: 0.0 decibels below full scale."
        );
        assert_lint(&filter_set(width));
        assert_lint(&volume_set(Volume::new(0)?));
        assert_lint(&signal(SignalLevel::new(-60.0)?));
        Ok(())
    }

    #[test]
    fn status_block_lints_per_line() -> TestResult {
        let health = StreamHealth {
            input_blocks: 500,
            output_blocks: 498,
            overruns: 0,
            underruns: 2,
        };
        let s = status(
            "435.640".parse()?,
            DemodMode::Usb,
            FilterWidth::new(2600.0)?,
            Volume::new(40)?,
            &health,
        );
        assert!(s.contains("Frequency: 435.64 megahertz"), "{s}");
        assert!(s.contains("Audio input blocks: 500"), "{s}");
        assert!(s.contains("Audio output underruns: 2"), "{s}");
        assert_lint(&s);
        Ok(())
    }

    #[test]
    fn errors_and_warnings_lint() {
        assert_lint(&error("something broke"));
        assert_lint(&restore_warning("USB audio output"));
    }
}
