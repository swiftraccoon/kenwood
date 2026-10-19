//! The GPS position memory: up to one hundred stored positions, each a name,
//! a UTC time, a latitude and longitude, an altitude and an APRS symbol, in
//! the order an index page lists them.
//!
//! The records are 32 bytes each from `0x4C000`; the index page from
//! `0x4CF00` names the record slot of each position in order, `0xFF` where
//! none is listed. Standard backups omit both regions
//! ([`crate::protocol::mcp::regions::POSITION_MEMORY`]);
//! [`crate::radio::Radio::backup_mcp_with_position_memory_until_exit`] reads
//! them after the standard configuration. The layout follows the official
//! program's position-memory transfer, which writes the positions to slots 0
//! onward in order and lists them in the same order.
//!
//! On firmware 1.02, type `K,2,1`, a radio with no positions held every
//! record as nine NUL bytes then `0xFF`, the 128 bytes after the records and
//! the whole index page `0xFF`. A position written over the operation-panel
//! USB read back in a fresh session, changing only its record and index
//! byte, and removing it restored every byte of both regions. How the radio
//! displays a written position and the unit of its altitude are not
//! established.

use std::fmt;

use crate::error::{Error, ValidationError};
use crate::memory::{MemoryImage, StoredText, StoredTextError};
use crate::protocol::mcp::regions;
use crate::radio::programming::PageReplacement;
use crate::types::{
    Address, Coordinate, Latitude, LatitudeHemisphere, Longitude, LongitudeHemisphere, Page,
};

/// Positions the memory holds.
pub const POSITION_MEMORY_ENTRIES: usize = 100;
/// Bytes of one stored position.
pub const POSITION_RECORD_SIZE: usize = 32;
/// Bytes of a position name.
pub const POSITION_NAME_SIZE: usize = 9;

/// Image address of slot 0's record.
const RECORDS: usize = 0x4_C000;
/// Image address of the index page.
const INDEX: usize = 0x4_CF00;
/// An index byte that lists no slot, and the year byte of an empty record.
const NONE: u8 = 0xFF;
/// Record bytes 15 bits 4 to 7 as the official program leaves them when it
/// writes a position into an empty slot.
const PROGRAM_FLAG_BITS: u8 = 0xF;
/// The record of an empty slot.
const EMPTY_RECORD: [u8; POSITION_RECORD_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// Image address of `slot`'s record.
const fn record_address(slot: usize) -> usize {
    RECORDS + POSITION_RECORD_SIZE * slot
}

/// A UTC time from 2000 through 2099, to the second: record bytes 9 to 14,
/// the year as years since 2000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PositionTime {
    year: u8,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl PositionTime {
    /// Validate a calendar time.
    ///
    /// # Errors
    ///
    /// Returns [`PositionMemoryError::InvalidTime`] for a year outside 2000
    /// to 2099, a day the month does not have, or an hour, minute or second
    /// outside the day.
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, PositionMemoryError> {
        let invalid = || PositionMemoryError::InvalidTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        };
        let since_2000 = year
            .checked_sub(2000)
            .and_then(|years| u8::try_from(years).ok())
            .filter(|years| *years <= 99)
            .ok_or_else(invalid)?;
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if year.is_multiple_of(4) => 29,
            2 => 28,
            _ => return Err(invalid()),
        };
        if day == 0 || day > days || hour > 23 || minute > 59 || second > 59 {
            return Err(invalid());
        }
        Ok(Self {
            year: since_2000,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// The year, 2000 to 2099.
    #[must_use]
    pub fn year(self) -> u16 {
        2000 + u16::from(self.year)
    }

    /// The month, 1 to 12.
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    /// The day of the month.
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    /// The hour, 0 to 23.
    #[must_use]
    pub const fn hour(self) -> u8 {
        self.hour
    }

    /// The minute, 0 to 59.
    #[must_use]
    pub const fn minute(self) -> u8 {
        self.minute
    }

    /// The second, 0 to 59.
    #[must_use]
    pub const fn second(self) -> u8 {
        self.second
    }

    fn from_stored(stored: [u8; 6]) -> Result<Self, PositionMemoryError> {
        let [year, month, day, hour, minute, second] = stored;
        Self::new(2000 + u16::from(year), month, day, hour, minute, second)
    }

    const fn to_stored(self) -> [u8; 6] {
        [
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
        ]
    }
}

impl fmt::Display for PositionTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            self.year(),
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        )
    }
}

/// An APRS symbol: the table identifier and the symbol code, each a graphic
/// ASCII byte, record bytes 28 and 29.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PositionSymbol {
    table: u8,
    code: u8,
}

impl PositionSymbol {
    /// The official program's symbol for a new position: table `\`, code `K`.
    pub const PROGRAM_DEFAULT: Self = Self {
        table: b'\\',
        code: b'K',
    };

    /// Validate the two bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PositionMemoryError::InvalidSymbol`] unless both are
    /// graphic ASCII (`0x21` to `0x7E`).
    pub const fn new(table: u8, code: u8) -> Result<Self, PositionMemoryError> {
        if table.is_ascii_graphic() && code.is_ascii_graphic() {
            Ok(Self { table, code })
        } else {
            Err(PositionMemoryError::InvalidSymbol { table, code })
        }
    }

    /// The table identifier.
    #[must_use]
    pub const fn table(self) -> u8 {
        self.table
    }

    /// The symbol code.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.code
    }
}

impl fmt::Display for PositionSymbol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}{}",
            char::from(self.table),
            char::from(self.code)
        )
    }
}

/// One stored position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPosition {
    /// The name, record bytes 0 to 8.
    pub name: StoredText<POSITION_NAME_SIZE>,
    /// When the position was stored.
    pub time: PositionTime,
    /// Record byte 15 bits 0 and 1, raw: the official program's GPS-valid
    /// field, which it sets to 0 for a position entered by hand.
    pub gps_fix: u8,
    /// Record byte 15 bits 4 to 7, raw.
    pub flag_bits: u8,
    /// The latitude: bytes 16 to 19, south in byte 15 bit 2.
    pub latitude: Latitude,
    /// The longitude: bytes 20 to 23, west in byte 15 bit 3.
    pub longitude: Longitude,
    /// Record bytes 24 to 27, a little-endian `i32`; the official program
    /// bounds it to -500 through 15000.
    pub altitude: i32,
    /// The APRS symbol.
    pub symbol: PositionSymbol,
    /// Record bytes 30 and 31, raw.
    pub reserved: [u8; 2],
}

impl StoredPosition {
    /// A position as the official program stores a new one: GPS-valid field
    /// 0, byte 15 bits 4 to 7 set and bytes 30 and 31 `0xFF`.
    #[must_use]
    pub const fn new(
        name: StoredText<POSITION_NAME_SIZE>,
        time: PositionTime,
        latitude: Latitude,
        longitude: Longitude,
        altitude: i32,
        symbol: PositionSymbol,
    ) -> Self {
        Self {
            name,
            time,
            gps_fix: 0,
            flag_bits: PROGRAM_FLAG_BITS,
            latitude,
            longitude,
            altitude,
            symbol,
            reserved: [NONE; 2],
        }
    }

    /// Decode a record.
    ///
    /// # Errors
    ///
    /// Returns the [`PositionMemoryError`] of the first field outside its
    /// domain.
    pub fn from_bytes(bytes: [u8; POSITION_RECORD_SIZE]) -> Result<Self, PositionMemoryError> {
        let [flags] = field(&bytes, 15);
        let [table, code] = field(&bytes, 28);
        let latitude = Latitude::from_stored(flags & 0x04 != 0, field(&bytes, 16));
        let longitude = Longitude::from_stored(flags & 0x08 != 0, field(&bytes, 20));
        Ok(Self {
            name: StoredText::from_bytes(field(&bytes, 0))?,
            time: PositionTime::from_stored(field(&bytes, 9))?,
            gps_fix: flags & 0x03,
            flag_bits: flags >> 4,
            latitude: Latitude::new(latitude.hemisphere(), validated(latitude.coordinate())?)?,
            longitude: Longitude::new(longitude.hemisphere(), validated(longitude.coordinate())?)?,
            altitude: i32::from_le_bytes(field(&bytes, 24)),
            symbol: PositionSymbol::new(table, code)?,
            reserved: field(&bytes, 30),
        })
    }

    /// Encode the record; `gps_fix` keeps its two low bits and `flag_bits`
    /// its four.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; POSITION_RECORD_SIZE] {
        let mut bytes = [0; POSITION_RECORD_SIZE];
        let mut flags = (self.flag_bits << 4) | (self.gps_fix & 0x03);
        if self.latitude.hemisphere() == LatitudeHemisphere::South {
            flags |= 0x04;
        }
        if self.longitude.hemisphere() == LongitudeHemisphere::West {
            flags |= 0x08;
        }
        let runs: [(usize, &[u8]); 8] = [
            (0, self.name.as_bytes()),
            (9, &self.time.to_stored()),
            (15, &[flags]),
            (16, &self.latitude.coordinate().to_stored()),
            (20, &self.longitude.coordinate().to_stored()),
            (24, &self.altitude.to_le_bytes()),
            (28, &[self.symbol.table, self.symbol.code]),
            (30, &self.reserved),
        ];
        for (start, run) in runs {
            if let Some(target) = bytes.get_mut(start..start + run.len()) {
                target.copy_from_slice(run);
            }
        }
        bytes
    }
}

/// `N` bytes of `record` from `start`.
fn field<const N: usize>(record: &[u8; POSITION_RECORD_SIZE], start: usize) -> [u8; N] {
    let mut field = [0; N];
    for (destination, source) in field.iter_mut().zip(record.iter().skip(start)) {
        *destination = *source;
    }
    field
}

/// A stored angle, validated.
const fn validated(coordinate: Coordinate) -> Result<Coordinate, ValidationError> {
    Coordinate::new(
        coordinate.degrees(),
        coordinate.minutes(),
        coordinate.ten_thousandths(),
    )
}

/// The position memory: its positions in index order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PositionMemory {
    entries: Vec<StoredPosition>,
}

impl PositionMemory {
    /// Hold `entries` in order.
    ///
    /// # Errors
    ///
    /// Returns [`PositionMemoryError::Full`] for more than
    /// [`POSITION_MEMORY_ENTRIES`].
    pub fn from_entries(entries: Vec<StoredPosition>) -> Result<Self, PositionMemoryError> {
        if entries.len() > POSITION_MEMORY_ENTRIES {
            return Err(PositionMemoryError::Full);
        }
        Ok(Self { entries })
    }

    /// Decode the positions the index page lists, skipping `0xFF` bytes.
    ///
    /// The caller checks that `image` holds both
    /// [`crate::protocol::mcp::regions::POSITION_MEMORY`] regions.
    ///
    /// # Errors
    ///
    /// Returns [`PositionMemoryError::SlotOutOfRange`] or
    /// [`PositionMemoryError::DuplicateSlot`] for an index byte naming no
    /// slot or one slot twice, [`PositionMemoryError::EmptySlotListed`] for
    /// a listed slot holding no position, and the [`StoredPosition::from_bytes`]
    /// errors.
    pub fn read(image: &MemoryImage) -> Result<Self, PositionMemoryError> {
        let bytes = image.as_bytes();
        let index = bytes
            .get(INDEX..INDEX + POSITION_MEMORY_ENTRIES)
            .unwrap_or_default();
        let mut listed = [false; POSITION_MEMORY_ENTRIES];
        let mut entries = Vec::new();
        for slot in index.iter().copied().filter(|slot| *slot != NONE) {
            let seen = listed
                .get_mut(usize::from(slot))
                .ok_or(PositionMemoryError::SlotOutOfRange { slot })?;
            if *seen {
                return Err(PositionMemoryError::DuplicateSlot { slot });
            }
            *seen = true;
            let start = record_address(usize::from(slot));
            let record: [u8; POSITION_RECORD_SIZE] = bytes
                .get(start..start + POSITION_RECORD_SIZE)
                .and_then(|run| run.try_into().ok())
                .ok_or(PositionMemoryError::SlotOutOfRange { slot })?;
            if record.get(9) == Some(&NONE) {
                return Err(PositionMemoryError::EmptySlotListed { slot });
            }
            entries.push(StoredPosition::from_bytes(record)?);
        }
        Ok(Self { entries })
    }

    /// The positions, in index order.
    #[must_use]
    pub fn entries(&self) -> &[StoredPosition] {
        &self.entries
    }

    /// Append a position.
    ///
    /// # Errors
    ///
    /// Returns [`PositionMemoryError::Full`] when the memory holds
    /// [`POSITION_MEMORY_ENTRIES`] positions.
    pub fn push(&mut self, entry: StoredPosition) -> Result<(), PositionMemoryError> {
        if self.entries.len() >= POSITION_MEMORY_ENTRIES {
            return Err(PositionMemoryError::Full);
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Remove and return the position at `index`.
    ///
    /// # Errors
    ///
    /// Returns [`PositionMemoryError::NoEntry`] past the last position.
    pub fn remove(&mut self, index: usize) -> Result<StoredPosition, PositionMemoryError> {
        if index >= self.entries.len() {
            return Err(PositionMemoryError::NoEntry { index });
        }
        Ok(self.entries.remove(index))
    }

    /// The writes of the record and index pages whose bytes change: position
    /// `n` in slot `n` and listed `n`-th, every other slot holding the empty
    /// record and every other index byte `0xFF`. The 128 bytes after the
    /// records and the index page's last 156 bytes keep `image`'s.
    ///
    /// Apply them with [`crate::radio::McpSession::compare_exchange_pages`],
    /// which compares each page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(&self, image: &MemoryImage) -> Result<Vec<PageReplacement>, Error> {
        let mut fields: Vec<(usize, Vec<u8>)> = (0..POSITION_MEMORY_ENTRIES)
            .map(|slot| {
                let record = self
                    .entries
                    .get(slot)
                    .map_or(EMPTY_RECORD, StoredPosition::to_bytes);
                (record_address(slot), record.to_vec())
            })
            .collect();
        let mut index = vec![NONE; POSITION_MEMORY_ENTRIES];
        for (listed, slot) in index.iter_mut().zip(0_u8..).take(self.entries.len()) {
            *listed = slot;
        }
        fields.push((INDEX, index));
        let mut replacements = Vec::new();
        for page in pages() {
            let start = page.address().as_usize();
            let expected = image
                .as_bytes()
                .get(start..start + page.len())
                .unwrap_or_default();
            let mut replacement = expected.to_vec();
            for (address, bytes) in &fields {
                if let Some(offset) = address.checked_sub(start)
                    && let Some(target) = replacement.get_mut(offset..offset + bytes.len())
                {
                    target.copy_from_slice(bytes);
                }
            }
            if replacement != expected {
                replacements.push(PageReplacement::new(page, expected, &replacement)?);
            }
        }
        Ok(replacements)
    }
}

/// Every writable page of the record area and the index page, in address
/// order.
fn pages() -> Vec<Page> {
    let mut pages = Vec::new();
    let mut address = RECORDS;
    while address < record_address(POSITION_MEMORY_ENTRIES) {
        if let Some(page) = writable_page(address) {
            address = page.address().as_usize() + page.len();
            pages.push(page);
        } else {
            break;
        }
    }
    pages.extend(writable_page(INDEX));
    pages
}

/// The writable page holding image address `address`.
fn writable_page(address: usize) -> Option<Page> {
    let address = Address::new(u32::try_from(address).ok()?).ok()?;
    regions::writable_page_for(address)
}

/// How a position memory fails to decode or to take an edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PositionMemoryError {
    /// The memory already holds [`POSITION_MEMORY_ENTRIES`] positions.
    #[error("the position memory holds at most {POSITION_MEMORY_ENTRIES} positions")]
    Full,
    /// An edit names a position past the last one.
    #[error("there is no position {index}")]
    NoEntry {
        /// The position requested.
        index: usize,
    },
    /// An index byte names a slot past the last one.
    #[error("the position index lists slot {slot}, outside the {POSITION_MEMORY_ENTRIES} slots")]
    SlotOutOfRange {
        /// The index byte.
        slot: u8,
    },
    /// The index lists one slot twice.
    #[error("position slot {slot} is listed twice in the index")]
    DuplicateSlot {
        /// The slot.
        slot: u8,
    },
    /// The index lists a slot whose year byte marks it empty.
    #[error("position slot {slot} is empty but listed in the index")]
    EmptySlotListed {
        /// The slot.
        slot: u8,
    },
    /// A stored or requested time is not a calendar time from 2000 to 2099.
    #[error(
        "{year}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} is not a valid position time"
    )]
    InvalidTime {
        /// The year.
        year: u16,
        /// The month.
        month: u8,
        /// The day.
        day: u8,
        /// The hour.
        hour: u8,
        /// The minute.
        minute: u8,
        /// The second.
        second: u8,
    },
    /// A symbol byte is not graphic ASCII.
    #[error("symbol bytes {table:#04X} {code:#04X} are not graphic ASCII")]
    InvalidSymbol {
        /// The table byte.
        table: u8,
        /// The code byte.
        code: u8,
    },
    /// A name is not in the stored text form.
    #[error(transparent)]
    Name(#[from] StoredTextError),
    /// A coordinate is outside its domain.
    #[error(transparent)]
    Coordinate(#[from] ValidationError),
}

#[cfg(test)]
#[path = "position_tests.rs"]
mod tests;
