//! Validated command grammar and listener-specific scalar domains.

use std::fmt;
use std::str::FromStr;

use if_dsp::DemodMode;
use kenwood_thd75::types::{Frequency, StepSize};

/// A command or scalar rejected before radio or audio processing.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// The command line contained no word.
    Empty,
    /// The first word did not name a command.
    UnknownCommand {
        /// Rejected command word.
        command: String,
    },
    /// The tune command lacked its MHz argument.
    MissingFrequency,
    /// The mode command lacked its mode argument.
    MissingMode,
    /// The filter command lacked its kilohertz argument.
    MissingFilter,
    /// The volume command lacked its percentage argument.
    MissingVolume,
    /// A command had an additional argument.
    UnexpectedArgument {
        /// First additional word.
        argument: String,
    },
    /// The MHz input was not accepted by the radio frequency parser.
    InvalidFrequency {
        /// Rejected MHz text.
        input: String,
    },
    /// A frequency lay outside 100 kHz through 524 MHz.
    FrequencyOutOfRange {
        /// Rejected frequency in hertz.
        hz: u32,
    },
    /// A frequency was not a multiple of 5 kHz.
    OffRaster {
        /// Rejected frequency in hertz.
        hz: u32,
    },
    /// A mode was not USB, LSB, CW or AM.
    UnknownMode {
        /// Rejected mode text.
        input: String,
    },
    /// A filter argument was not a floating-point number.
    InvalidFilter {
        /// Rejected kilohertz text.
        input: String,
    },
    /// A filter was nonfinite or outside 200 through 6000 Hz.
    FilterOutOfRange {
        /// Rejected width in hertz.
        hz: f32,
    },
    /// A volume argument was not an unsigned byte.
    InvalidVolume {
        /// Rejected percentage text.
        input: String,
    },
    /// A volume percentage exceeded 100.
    VolumeOutOfRange {
        /// Rejected percentage.
        percent: u8,
    },
    /// A signal reading was NaN or infinite.
    NonFiniteSignal {
        /// Rejected dBFS reading.
        db: f32,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Type a command, or help for the list."),
            Self::UnknownCommand { command } => write!(
                f,
                "Unknown command {command:?}. Type help for the command list."
            ),
            Self::MissingFrequency => {
                f.write_str("Usage: tune followed by megahertz, like tune 435.640.")
            }
            Self::MissingMode => f.write_str("Usage: mode usb, lsb, cw, or am."),
            Self::MissingFilter => {
                f.write_str("Usage: filter followed by kilohertz, like filter 2.4.")
            }
            Self::MissingVolume => f.write_str("Usage: volume followed by 0 through 100."),
            Self::UnexpectedArgument { argument } => write!(
                f,
                "Unexpected argument {argument:?}. Type help for command syntax."
            ),
            Self::InvalidFrequency { input } => {
                write!(f, "Not a frequency: {input:?}. Example: tune 435.640.")
            }
            Self::FrequencyOutOfRange { .. } => {
                f.write_str("Frequency out of range. Use 0.1 through 524 megahertz.")
            }
            Self::OffRaster { .. } => {
                f.write_str("Use a multiple of 5 kilohertz, like 435.640 or 14.070.")
            }
            Self::UnknownMode { input } => {
                write!(f, "Unknown mode {input:?}. Options: usb, lsb, cw, am.")
            }
            Self::InvalidFilter { input } => {
                write!(f, "Not a width: {input:?}. Example: filter 2.4.")
            }
            Self::FilterOutOfRange { .. } => {
                f.write_str("Filter width out of range. Use 0.2 through 6 kilohertz.")
            }
            Self::InvalidVolume { input } => {
                write!(f, "Not a volume: {input:?}. Use 0 through 100.")
            }
            Self::VolumeOutOfRange { .. } => f.write_str("Volume out of range. Use 0 through 100."),
            Self::NonFiniteSignal { .. } => {
                f.write_str("Signal level is unavailable because the reading is not finite.")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// A listener frequency from 100 kHz through 524 MHz, on the 5 kHz raster.
///
/// See the [command workflow](crate#library-workflow). The radio's current
/// frequency also determines whether its bounded step walk can reach a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListenerFrequency(Frequency);

impl ListenerFrequency {
    /// Validate the listener's frequency range and raster.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::FrequencyOutOfRange`] outside 100 kHz through
    /// 524 MHz, or [`ParseError::OffRaster`] for a nonmultiple of 5 kHz.
    pub fn new(frequency: Frequency) -> Result<Self, ParseError> {
        if !(100_000..=524_000_000).contains(&frequency.as_hz()) {
            return Err(ParseError::FrequencyOutOfRange {
                hz: frequency.as_hz(),
            });
        }
        if !frequency.is_aligned_to(StepSize::Hz5000) {
            return Err(ParseError::OffRaster {
                hz: frequency.as_hz(),
            });
        }
        Ok(Self(frequency))
    }

    /// Return the radio frequency without changing its value.
    #[must_use]
    pub const fn frequency(self) -> Frequency {
        self.0
    }

    /// Return the frequency in integer hertz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        self.0.as_hz()
    }
}

impl TryFrom<Frequency> for ListenerFrequency {
    type Error = ParseError;
    fn try_from(value: Frequency) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ListenerFrequency> for Frequency {
    fn from(value: ListenerFrequency) -> Self {
        value.frequency()
    }
}

impl FromStr for ListenerFrequency {
    type Err = ParseError;
    /// Parse exact MHz text, then validate through [`Self::new`].
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::InvalidFrequency`] for malformed MHz text, or
    /// the range/raster error documented by [`Self::new`].
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let frequency =
            Frequency::from_mhz_str(input).map_err(|_| ParseError::InvalidFrequency {
                input: input.to_owned(),
            })?;
        Self::new(frequency)
    }
}

/// A finite listener filter width from 200 through 6000 Hz.
///
/// The active mode can impose a narrower upper bound. The binary checks that
/// combined configuration before submitting it to the DSP worker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterWidth(f32);

impl Eq for FilterWidth {}

impl FilterWidth {
    /// Validate a filter width in hertz.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::FilterOutOfRange`] for nonfinite widths or values
    /// outside 200 through 6000 Hz.
    pub fn new(hz: f32) -> Result<Self, ParseError> {
        if hz.is_finite() && (200.0..=6000.0).contains(&hz) {
            Ok(Self(hz))
        } else {
            Err(ParseError::FilterOutOfRange { hz })
        }
    }

    /// Return the width in hertz.
    #[must_use]
    pub const fn hz(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for FilterWidth {
    type Error = ParseError;
    fn try_from(hz: f32) -> Result<Self, Self::Error> {
        Self::new(hz)
    }
}

impl From<FilterWidth> for f32 {
    fn from(value: FilterWidth) -> Self {
        value.hz()
    }
}

/// An output volume percentage from zero (mute) through 100 (unity gain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Volume(u8);

impl Volume {
    /// Validate a volume percentage.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::VolumeOutOfRange`] above 100.
    pub const fn new(percent: u8) -> Result<Self, ParseError> {
        if percent <= 100 {
            Ok(Self(percent))
        } else {
            Err(ParseError::VolumeOutOfRange { percent })
        }
    }

    /// Return the percentage.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Volume {
    type Error = ParseError;
    fn try_from(percent: u8) -> Result<Self, Self::Error> {
        Self::new(percent)
    }
}

impl From<Volume> for u8 {
    fn from(value: Volume) -> Self {
        value.percent()
    }
}

/// A finite dBFS readout clamped to the display range of -120 through 0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalLevel(f32);

impl Eq for SignalLevel {}

impl SignalLevel {
    /// Reject nonfinite readings and clamp finite readings to -120 through 0 dBFS.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::NonFiniteSignal`] for NaN or either infinity.
    pub const fn new(db: f32) -> Result<Self, ParseError> {
        if db.is_finite() {
            Ok(Self(db.clamp(-120.0, 0.0)))
        } else {
            Err(ParseError::NonFiniteSignal { db })
        }
    }

    /// Return the clamped reading in dBFS.
    #[must_use]
    pub const fn dbfs(self) -> f32 {
        self.0
    }
}

impl TryFrom<f32> for SignalLevel {
    type Error = ParseError;
    fn try_from(db: f32) -> Result<Self, Self::Error> {
        Self::new(db)
    }
}

impl From<SignalLevel> for f32 {
    fn from(value: SignalLevel) -> Self {
        value.dbfs()
    }
}

/// A prompt command whose scalar payloads are already validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Tune to a listener frequency; the radio checks step-walk reachability.
    Tune(ListenerFrequency),
    /// Switch the computer-side demodulator.
    Mode(DemodMode),
    /// Set a filter width, subject to the selected mode's DSP limit.
    Filter(FilterWidth),
    /// Set output volume.
    Volume(Volume),
    /// Report signal level.
    Signal,
    /// Report tuning, mode, filter, volume and stream health.
    Status,
    /// Print command syntax.
    Help,
    /// Stop audio, restore radio settings and exit.
    Quit,
}

impl FromStr for Command {
    type Err = ParseError;

    /// Parse one command, accepting ASCII-insensitive keywords and mode names.
    ///
    /// # Errors
    ///
    /// Returns the corresponding [`ParseError`] for empty input, unknown
    /// keywords/modes, missing/additional arguments, malformed values or scalar
    /// domain violations. No radio or audio I/O occurs.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut words = line.split_whitespace();
        let keyword = words.next().ok_or(ParseError::Empty)?.to_ascii_lowercase();
        let argument = words.next();
        if let Some(argument) = words.next() {
            return Err(ParseError::UnexpectedArgument {
                argument: argument.to_owned(),
            });
        }
        let command = match keyword.as_str() {
            "tune" => Self::Tune(argument.ok_or(ParseError::MissingFrequency)?.parse()?),
            "mode" => Self::Mode(
                match argument
                    .ok_or(ParseError::MissingMode)?
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "usb" => DemodMode::Usb,
                    "lsb" => DemodMode::Lsb,
                    "cw" => DemodMode::Cw,
                    "am" => DemodMode::Am,
                    input => {
                        return Err(ParseError::UnknownMode {
                            input: input.to_owned(),
                        });
                    }
                },
            ),
            "filter" => {
                let input = argument.ok_or(ParseError::MissingFilter)?;
                let khz = input
                    .parse::<f32>()
                    .map_err(|_| ParseError::InvalidFilter {
                        input: input.to_owned(),
                    })?;
                Self::Filter(FilterWidth::new(khz * 1000.0)?)
            }
            "volume" => {
                let input = argument.ok_or(ParseError::MissingVolume)?;
                let percent = input.parse::<u8>().map_err(|_| ParseError::InvalidVolume {
                    input: input.to_owned(),
                })?;
                Self::Volume(Volume::new(percent)?)
            }
            "signal" => Self::Signal,
            "status" => Self::Status,
            "help" | "?" => Self::Help,
            "quit" | "exit" | "q" => Self::Quit,
            command => {
                return Err(ParseError::UnknownCommand {
                    command: command.to_owned(),
                });
            }
        };
        if matches!(
            command,
            Self::Signal | Self::Status | Self::Help | Self::Quit
        ) && let Some(argument) = argument
        {
            return Err(ParseError::UnexpectedArgument {
                argument: argument.to_owned(),
            });
        }
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parses_commands_with_validated_payloads() -> TestResult {
        assert_eq!(
            "tune 435.640".parse::<Command>()?,
            Command::Tune("435.640".parse()?)
        );
        assert_eq!(
            "MODE usb".parse::<Command>()?,
            Command::Mode(DemodMode::Usb)
        );
        assert_eq!(
            "mode LSB".parse::<Command>()?,
            Command::Mode(DemodMode::Lsb)
        );
        assert_eq!(
            "filter 2.4".parse::<Command>()?,
            Command::Filter(FilterWidth::new(2400.0)?)
        );
        assert_eq!(
            "volume 55".parse::<Command>()?,
            Command::Volume(Volume::new(55)?)
        );
        for (text, expected) in [
            ("signal", Command::Signal),
            ("status", Command::Status),
            ("?", Command::Help),
            ("QUIT", Command::Quit),
        ] {
            assert_eq!(text.parse::<Command>()?, expected);
        }
        Ok(())
    }

    #[test]
    fn construction_enforces_ranges_raster_and_finiteness() -> TestResult {
        for hz in [100_000, 14_070_000, 524_000_000] {
            assert_eq!(ListenerFrequency::new(Frequency::new(hz))?.as_hz(), hz);
        }
        assert!(matches!(
            ListenerFrequency::new(Frequency::new(99_999)),
            Err(ParseError::FrequencyOutOfRange { .. })
        ));
        assert!(matches!(
            ListenerFrequency::new(Frequency::new(524_005_000)),
            Err(ParseError::FrequencyOutOfRange { .. })
        ));
        assert!(matches!(
            ListenerFrequency::new(Frequency::new(435_641_000)),
            Err(ParseError::OffRaster { .. })
        ));
        for width in [200.0, 6000.0] {
            assert_eq!(FilterWidth::new(width)?.hz().to_bits(), width.to_bits());
        }
        for width in [199.0, 6001.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(FilterWidth::new(width).is_err(), "width {width}");
        }
        assert_eq!(Volume::new(0)?.percent(), 0);
        assert_eq!(Volume::new(100)?.percent(), 100);
        assert!(matches!(
            Volume::new(101),
            Err(ParseError::VolumeOutOfRange { percent: 101 })
        ));
        for db in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(SignalLevel::new(db).is_err(), "reading {db}");
        }
        assert_eq!(SignalLevel::new(5.0)?.dbfs().to_bits(), 0.0_f32.to_bits());
        assert_eq!(
            SignalLevel::new(-200.0)?.dbfs().to_bits(),
            (-120.0_f32).to_bits()
        );
        Ok(())
    }

    #[test]
    fn grammar_rejects_bad_and_additional_arguments() {
        for text in [
            "",
            "blorp",
            "tune",
            "tune fish",
            "tune 999",
            "tune -7",
            "tune 435.641",
            "mode fm",
            "filter 9",
            "filter 0.1",
            "filter NaN",
            "volume 101",
            "status extra",
            "tune 435.640 extra",
        ] {
            assert!(text.parse::<Command>().is_err(), "command {text:?}");
        }
    }
}
