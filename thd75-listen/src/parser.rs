//! Command grammar for the listener prompt.

use if_dsp::DemodMode;

/// A parsed prompt command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    /// Tune the radio (value in megahertz, already validated on the
    /// 5 kHz raster).
    Tune(f64),
    /// Switch the computer-side demodulator.
    Mode(DemodMode),
    /// Set the audio passband width in hertz.
    Filter(f32),
    /// Set output volume, 0-100.
    Volume(u8),
    /// Report the signal level.
    Signal,
    /// Report tuning, mode, filter, volume, stream health.
    Status,
    /// Print the command list.
    Help,
    /// Restore the radio and exit.
    Quit,
}

/// Lowest tunable frequency in megahertz (Band B receive floor).
const MIN_MHZ: f64 = 0.1;
/// Highest tunable frequency in megahertz (Band B receive ceiling).
const MAX_MHZ: f64 = 524.0;

/// Parse one prompt line. Errors are complete, accessible sentences;
/// the caller adds the `Error: ` prefix.
///
/// # Errors
///
/// Returns an explanatory sentence for empty input, unknown commands,
/// missing or malformed arguments, and out-of-range values.
pub fn parse(line: &str) -> Result<Command, String> {
    let mut words = line.split_whitespace();
    let Some(keyword) = words.next() else {
        return Err("Type a command, or help for the list.".to_owned());
    };
    let arg = words.next();
    match keyword.to_lowercase().as_str() {
        "tune" => parse_tune(arg),
        "mode" => parse_mode(arg),
        "filter" => parse_filter(arg),
        "volume" => parse_volume(arg),
        "signal" => Ok(Command::Signal),
        "status" => Ok(Command::Status),
        "help" | "?" => Ok(Command::Help),
        "quit" | "exit" | "q" => Ok(Command::Quit),
        other => Err(format!(
            "Unknown command {other:?}. Type help for the command list."
        )),
    }
}

fn parse_tune(arg: Option<&str>) -> Result<Command, String> {
    let Some(arg) = arg else {
        return Err("Usage: tune followed by megahertz, like tune 435.640.".to_owned());
    };
    let mhz: f64 = arg
        .parse()
        .map_err(|_ignored| format!("Not a frequency: {arg:?}. Example: tune 435.640."))?;
    if !mhz.is_finite() || !(MIN_MHZ..=MAX_MHZ).contains(&mhz) {
        return Err("Frequency out of range. Use 0.1 through 524 megahertz.".to_owned());
    }
    // The listener pins a 5 kHz step; the radio silently snaps other
    // frequencies to that raster, so reject them with guidance.
    let hz = mhz * 1_000_000.0;
    let raster = hz / 5_000.0;
    if (raster - raster.round()).abs() > 1e-6 {
        return Err("Use a multiple of 5 kilohertz, like 435.640 or 14.070.".to_owned());
    }
    Ok(Command::Tune(mhz))
}

fn parse_mode(arg: Option<&str>) -> Result<Command, String> {
    let Some(arg) = arg else {
        return Err("Usage: mode usb, lsb, cw, or am.".to_owned());
    };
    let mode = match arg.to_lowercase().as_str() {
        "usb" => DemodMode::Usb,
        "lsb" => DemodMode::Lsb,
        "cw" => DemodMode::Cw,
        "am" => DemodMode::Am,
        other => {
            return Err(format!(
                "Unknown mode {other:?}. Options: usb, lsb, cw, am."
            ));
        }
    };
    Ok(Command::Mode(mode))
}

fn parse_filter(arg: Option<&str>) -> Result<Command, String> {
    let Some(arg) = arg else {
        return Err("Usage: filter followed by kilohertz, like filter 2.4.".to_owned());
    };
    let khz: f32 = arg
        .parse()
        .map_err(|_ignored| format!("Not a width: {arg:?}. Example: filter 2.4."))?;
    if !khz.is_finite() || !(0.2..=6.0).contains(&khz) {
        return Err("Filter width out of range. Use 0.2 through 6 kilohertz.".to_owned());
    }
    Ok(Command::Filter(khz * 1_000.0))
}

fn parse_volume(arg: Option<&str>) -> Result<Command, String> {
    let Some(arg) = arg else {
        return Err("Usage: volume followed by 0 through 100.".to_owned());
    };
    let pct: u8 = arg
        .parse()
        .map_err(|_ignored| format!("Not a volume: {arg:?}. Use 0 through 100."))?;
    if pct > 100 {
        return Err("Volume out of range. Use 0 through 100.".to_owned());
    }
    Ok(Command::Volume(pct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command() {
        assert_eq!(parse("tune 435.640"), Ok(Command::Tune(435.640)));
        assert_eq!(parse("MODE usb"), Ok(Command::Mode(DemodMode::Usb)));
        assert_eq!(parse("mode LSB"), Ok(Command::Mode(DemodMode::Lsb)));
        assert_eq!(parse("filter 2.4"), Ok(Command::Filter(2_400.0)));
        assert_eq!(parse("volume 55"), Ok(Command::Volume(55)));
        assert_eq!(parse("signal"), Ok(Command::Signal));
        assert_eq!(parse("status"), Ok(Command::Status));
        assert_eq!(parse("?"), Ok(Command::Help));
        assert_eq!(parse("QUIT"), Ok(Command::Quit));
    }

    #[test]
    fn rejects_off_raster_tuning() {
        let e = parse("tune 435.641");
        assert!(
            matches!(&e, Err(msg) if msg.contains("5 kilohertz")),
            "expected raster guidance, got {e:?}"
        );
        assert_eq!(parse("tune 14.070"), Ok(Command::Tune(14.070)));
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(parse("tune 999").is_err(), "frequency ceiling");
        assert!(parse("tune -7").is_err(), "negative frequency");
        assert!(parse("filter 9").is_err(), "filter ceiling");
        assert!(parse("filter 0.1").is_err(), "filter floor");
        assert!(parse("volume 101").is_err(), "volume ceiling");
    }

    #[test]
    fn rejects_garbage_with_guidance() {
        let e = parse("blorp");
        assert!(
            matches!(&e, Err(msg) if msg.contains("help")),
            "unknown command must point at help, got {e:?}"
        );
        assert!(parse("tune fish").is_err(), "non-numeric frequency");
        assert!(parse("mode fm").is_err(), "unsupported mode");
        assert!(parse("").is_err(), "empty line");
    }
}
