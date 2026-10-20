//! The ten weather channels, `WX 1` to `WX 10`.
//!
//! On type `K,2,1` their flags, records and names occupy memory-channel table
//! indexes 1102 to 1111, directly after the APRS channel; table indexes 1112
//! to 1131 are empty. Firmware 1.02 holds the weather-broadcast frequencies
//! from 161.650 to 163.275 MHz there, group 32 in each flag and the names
//! `WX  1` to `WX 10`.

use std::fmt;

use crate::error::{SettingName, ValidationError};
use crate::types::stored::{data_address_at, flag_address_at, name_address_at};
use crate::types::{
    CHANNEL_NAME_SIZE, CHANNEL_RECORD_SIZE, ChannelName, StoredChannel, StoredChannelFlag,
};

#[cfg(test)]
#[path = "weather_tests.rs"]
mod tests;

/// One of the ten weather channels, numbered 1 to 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WeatherChannel(u8);

checked_integer_traits!(WeatherChannel, u8, number);

impl WeatherChannel {
    /// Number of weather channels.
    pub const COUNT: u8 = 10;
    /// Table index of `WX 1`.
    pub const FIRST_INDEX: u16 = 1102;

    /// Validate a channel number.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] outside 1 to 10.
    pub const fn new(number: u8) -> Result<Self, ValidationError> {
        if number == 0 || number > Self::COUNT {
            Err(ValidationError::SettingOutOfRange {
                setting: SettingName::WeatherChannel,
                value: number as u64,
                max: Self::COUNT as u64,
            })
        } else {
            Ok(Self(number))
        }
    }

    /// Every channel, `WX 1` first.
    pub fn all() -> impl Iterator<Item = Self> {
        (1..=Self::COUNT).map(Self)
    }

    /// The channel number, 1 to 10.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Table index of the flag, record and name, 1102 to 1111.
    #[must_use]
    pub const fn index(self) -> u16 {
        Self::FIRST_INDEX - 1 + self.0 as u16
    }

    /// Image address of the flag record.
    #[must_use]
    pub const fn flag_address(self) -> usize {
        flag_address_at(self.index())
    }

    /// Image address of the channel record.
    #[must_use]
    pub const fn data_address(self) -> usize {
        data_address_at(self.index())
    }

    /// Image address of the name.
    #[must_use]
    pub const fn name_address(self) -> usize {
        name_address_at(self.index())
    }
}

impl fmt::Display for WeatherChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WX {}", self.0)
    }
}

/// A weather channel's stored flag, record and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeatherChannelEntry {
    channel: WeatherChannel,
    flag: StoredChannelFlag,
    record: Option<StoredChannel>,
    raw_record: [u8; CHANNEL_RECORD_SIZE],
    name: ChannelName,
}

impl WeatherChannelEntry {
    /// Decode the flag, record and name of `channel`.
    ///
    /// The record is decoded only when the flag marks the slot stored.
    ///
    /// # Errors
    ///
    /// Returns the [`StoredChannelFlag::from_bytes`] errors,
    /// [`ValidationError::StoredChannelLength`] for a record or name slice of
    /// the wrong length and, for a stored slot, the
    /// [`StoredChannel::from_bytes`] errors.
    pub fn from_records(
        channel: WeatherChannel,
        flag: &[u8],
        record: &[u8],
        name: &[u8],
    ) -> Result<Self, ValidationError> {
        let flag = StoredChannelFlag::from_bytes(flag)?;
        let raw_record: [u8; CHANNEL_RECORD_SIZE] =
            record
                .try_into()
                .map_err(|_| ValidationError::StoredChannelLength {
                    actual: record.len(),
                })?;
        let name: [u8; CHANNEL_NAME_SIZE] = name
            .try_into()
            .map_err(|_| ValidationError::StoredChannelLength { actual: name.len() })?;
        let record = if flag.is_programmed() {
            Some(StoredChannel::from_bytes(&raw_record)?)
        } else {
            None
        };
        Ok(Self {
            channel,
            flag,
            record,
            raw_record,
            name: ChannelName::from_bytes(name),
        })
    }

    /// The channel.
    #[must_use]
    pub const fn channel(&self) -> WeatherChannel {
        self.channel
    }

    /// The flag record.
    #[must_use]
    pub const fn flag(&self) -> StoredChannelFlag {
        self.flag
    }

    /// The decoded record of a stored slot.
    #[must_use]
    pub const fn record(&self) -> Option<&StoredChannel> {
        self.record.as_ref()
    }

    /// The 40 record bytes, decoded or not.
    #[must_use]
    pub const fn raw_record(&self) -> &[u8; CHANNEL_RECORD_SIZE] {
        &self.raw_record
    }

    /// The stored name.
    #[must_use]
    pub const fn name(&self) -> ChannelName {
        self.name
    }
}

impl fmt::Display for WeatherChannelEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.record {
            Some(record) => {
                write!(formatter, "{}: {}", self.channel, record.receive_frequency)?;
                if self.flag.scan_lockout() {
                    formatter.write_str(" lockout")?;
                }
                Ok(())
            }
            None => write!(formatter, "{}: empty", self.channel),
        }
    }
}
