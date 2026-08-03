//! System configuration types.
//!
//! Covers radio-wide settings including user preferences, frequency range
//! limits, I/O port control, and SD card operations.
//!
//! System setting types are defined in the [`settings`](super::settings) module:
//! [`SystemSettings`](super::settings::SystemSettings),
//! [`AudioSettings`](super::settings::AudioSettings), and
//! [`DisplaySettings`](super::settings::DisplaySettings).
//!
//! # Transceiver reset (per User Manual Chapter 12)
//!
//! Menu No. 999 or `[F]` + Power ON provides three reset types:
//!
//! - **VFO Reset**: initializes VFO and accompanying settings only.
//! - **Partial Reset**: initializes all settings except memory channels
//!   and DTMF memory channels.
//! - **Full Reset**: initializes all customized settings. Date and time
//!   are not reset. To enable voice guidance after full reset, press
//!   `[PF2]` + Power ON.
//!
//! # Firmware version (per User Manual Chapter 12)
//!
//! Menu No. 991 displays the current firmware version. Firmware updates
//! are applied by connecting to a PC via USB.
//!
//! # USB function (per User Manual Chapter 17)
//!
//! Menu No. 980: `COM+AF/IF Output` (virtual COM port + audio output)
//! or `Mass Storage` (microSD card access from PC). The radio is a
//! USB 2.0 device supporting CDC, ADC 1.0, and MSC device classes.
//! USB hub connections are not supported.
//!
use std::{fmt, str::FromStr};

use crate::error::ValidationError;

/// Exact RT payload used when the radio clock is unavailable.
pub const RADIO_CLOCK_UNAVAILABLE_WIRE: &str = "------------";

/// Calendar-validated TH-D75 real-time clock value.
///
/// The RT CAT response encodes a date and time as twelve decimal digits in
/// `YYMMDDHHmmss` order. The two-digit year is interpreted as 2000-2099.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RadioDateTime {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl RadioDateTime {
    /// Construct a calendar-valid radio date and time.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidRadioDateTime`] unless `year` is in
    /// 2000-2099 and every calendar/time component is valid.
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, ValidationError> {
        let value = format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}");
        if !(2000..=2099).contains(&year) {
            return Err(Self::invalid(value, "year must be in 2000-2099"));
        }
        if !(1..=12).contains(&month) {
            return Err(Self::invalid(value, "month must be 01-12"));
        }
        let maximum_day = days_in_month(year, month);
        if day == 0 || day > maximum_day {
            return Err(Self::invalid(
                value,
                "day is outside the valid range for the month",
            ));
        }
        if hour > 23 {
            return Err(Self::invalid(value, "hour must be 00-23"));
        }
        if minute > 59 {
            return Err(Self::invalid(value, "minute must be 00-59"));
        }
        if second > 59 {
            return Err(Self::invalid(value, "second must be 00-59"));
        }

        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// Four-digit year (2000-2099).
    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    /// Calendar month (1-12).
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    /// Day of the month (1-31, constrained by month and leap year).
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    /// Hour (0-23).
    #[must_use]
    pub const fn hour(self) -> u8 {
        self.hour
    }

    /// Minute (0-59).
    #[must_use]
    pub const fn minute(self) -> u8 {
        self.minute
    }

    /// Second (0-59).
    #[must_use]
    pub const fn second(self) -> u8 {
        self.second
    }

    /// Encode the value exactly as an RT `YYMMDDHHmmss` payload.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        let wire_year = self.year - 2000;
        format!(
            "{wire_year:02}{:02}{:02}{:02}{:02}{:02}",
            self.month, self.day, self.hour, self.minute, self.second
        )
    }

    const fn invalid(value: String, detail: &'static str) -> ValidationError {
        ValidationError::InvalidRadioDateTime { value, detail }
    }
}

impl TryFrom<&str> for RadioDateTime {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.len() != 12
            || !value.is_ascii()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(Self::invalid(
                value.to_owned(),
                "expected exactly 12 ASCII digits in YYMMDDHHmmss format",
            ));
        }

        let mut pairs = value.as_bytes().chunks_exact(2).map(two_decimal_digits);
        let Some(Some(year)) = pairs.next() else {
            return Err(Self::invalid(value.to_owned(), "missing year"));
        };
        let Some(Some(month)) = pairs.next() else {
            return Err(Self::invalid(value.to_owned(), "missing month"));
        };
        let Some(Some(day)) = pairs.next() else {
            return Err(Self::invalid(value.to_owned(), "missing day"));
        };
        let Some(Some(hour)) = pairs.next() else {
            return Err(Self::invalid(value.to_owned(), "missing hour"));
        };
        let Some(Some(minute)) = pairs.next() else {
            return Err(Self::invalid(value.to_owned(), "missing minute"));
        };
        let Some(Some(second)) = pairs.next() else {
            return Err(Self::invalid(value.to_owned(), "missing second"));
        };

        Self::new(2000 + u16::from(year), month, day, hour, minute, second).map_err(|error| {
            let detail = match error {
                ValidationError::InvalidRadioDateTime { detail, .. } => detail,
                _ => "invalid radio date/time",
            };
            Self::invalid(value.to_owned(), detail)
        })
    }
}

impl TryFrom<String> for RadioDateTime {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl FromStr for RadioDateTime {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl fmt::Display for RadioDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Typed RT response, including the radio's explicit unavailable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RadioClock {
    /// Calendar-valid RT value.
    DateTime(RadioDateTime),
    /// The radio returned the exact `------------` unavailable sentinel.
    Unavailable,
}

impl RadioClock {
    /// Return the date/time when the radio supplied one.
    #[must_use]
    pub const fn date_time(self) -> Option<RadioDateTime> {
        match self {
            Self::DateTime(value) => Some(value),
            Self::Unavailable => None,
        }
    }

    /// Encode the value exactly as an RT response payload.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        match self {
            Self::DateTime(value) => value.to_wire_string(),
            Self::Unavailable => RADIO_CLOCK_UNAVAILABLE_WIRE.to_owned(),
        }
    }
}

impl From<RadioDateTime> for RadioClock {
    fn from(value: RadioDateTime) -> Self {
        Self::DateTime(value)
    }
}

impl TryFrom<&str> for RadioClock {
    type Error = ValidationError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value == RADIO_CLOCK_UNAVAILABLE_WIRE {
            Ok(Self::Unavailable)
        } else {
            RadioDateTime::try_from(value).map(Self::DateTime)
        }
    }
}

impl TryFrom<String> for RadioClock {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl FromStr for RadioClock {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl fmt::Display for RadioClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DateTime(value) => value.fmt(f),
            Self::Unavailable => f.write_str("unavailable"),
        }
    }
}

fn two_decimal_digits(bytes: &[u8]) -> Option<u8> {
    let [tens, ones] = bytes else {
        return None;
    };
    Some((tens - b'0') * 10 + (ones - b'0'))
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[cfg(test)]
mod tests {
    use super::{RADIO_CLOCK_UNAVAILABLE_WIRE, RadioClock, RadioDateTime};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn valid_wire_value_round_trips() -> TestResult {
        let value = RadioDateTime::try_from("240229235959")?;
        assert_eq!(value.year(), 2024);
        assert_eq!(value.month(), 2);
        assert_eq!(value.day(), 29);
        assert_eq!(value.hour(), 23);
        assert_eq!(value.minute(), 59);
        assert_eq!(value.second(), 59);
        assert_eq!(value.to_wire_string(), "240229235959");
        assert_eq!(value.to_string(), "2024-02-29 23:59:59");
        Ok(())
    }

    #[test]
    fn invalid_calendar_values_are_rejected() {
        for value in [
            "230229235959",
            "241332000000",
            "240431000000",
            "240101240000",
            "240101006000",
            "240101000060",
        ] {
            assert!(RadioDateTime::try_from(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn wire_shape_is_exact() {
        for value in [
            "",
            "24010100000",
            "2401010000000",
            "24010100000x",
            "------------ ",
        ] {
            assert!(
                RadioDateTime::try_from(value).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn unavailable_sentinel_is_exact() -> TestResult {
        let unavailable = RadioClock::try_from(RADIO_CLOCK_UNAVAILABLE_WIRE)?;
        assert_eq!(unavailable, RadioClock::Unavailable);
        assert_eq!(unavailable.to_wire_string(), RADIO_CLOCK_UNAVAILABLE_WIRE);
        assert_eq!(unavailable.to_string(), "unavailable");
        assert!(RadioClock::try_from("-----------").is_err());
        assert!(RadioClock::try_from("-------------").is_err());
        Ok(())
    }
}
