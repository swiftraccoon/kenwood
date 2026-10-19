//! One stored repeater or hotspot record and its field types.

use std::fmt;

use super::{REPEATER_RECORD_SIZE, REPEATER_TEXT_SIZE, RepeaterListError};
use crate::memory::StoredText;
use crate::types::StoredCallsign;

const NAME: usize = 0;
const SUB_NAME: usize = 16;
const CALLSIGN: usize = 32;
const GATEWAY: usize = 40;
const FREQUENCY: usize = 48;
const OFFSET: usize = 52;
const LATITUDE: usize = 56;
const LONGITUDE: usize = 60;
const HEMISPHERES: usize = 64;
const ACCURACY: usize = 65;
const ZONE: usize = 66;
const SHIFT: usize = 67;
const REGION: usize = 70;
const COUNTRY: usize = 71;
const GROUP: usize = 72;
const STATE: usize = 75;
const PREVIOUS: usize = 76;
const NEXT: usize = 78;

const CALLSIGN_SIZE: usize = 8;
const SOUTH: u8 = 0x04;
const WEST: u8 = 0x08;
const EMPTY_SLOT: u8 = 0x10;
const NO_INDEX: u8 = 0xFF;
const NO_GROUP: u16 = 0xFFFF;
const NO_LINK: u16 = 0xFFFF;
const MAX_QUARTER_HOURS: u8 = 112;

/// A frequency or frequency offset of a record in hertz.
///
/// Catalog entries span bands outside the receiver's range (29 MHz to
/// 1.3 GHz were stored), so this is not a [`crate::types::Frequency`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepeaterFrequency(u32);

impl RepeaterFrequency {
    /// Accept any value below `u32::MAX`, the value of an empty slot.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::FieldOutOfRange`] for `u32::MAX`.
    pub const fn new(hertz: u32) -> Result<Self, RepeaterListError> {
        if hertz == u32::MAX {
            Err(RepeaterListError::FieldOutOfRange {
                field: "frequency",
                value: hertz,
                max: u32::MAX - 1,
            })
        } else {
            Ok(Self(hertz))
        }
    }

    /// Parse an unsigned decimal megahertz value with at most six fractional
    /// digits, such as `448.375` or `1291`.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::InvalidMegahertzText`] for any other shape
    /// or a value beyond `u32::MAX` hertz, and the error of [`Self::new`].
    pub fn from_mhz_str(text: &str) -> Result<Self, RepeaterListError> {
        let hertz = crate::types::frequency::megahertz_text_hz(text).ok_or_else(|| {
            RepeaterListError::InvalidMegahertzText {
                text: text.to_owned(),
            }
        })?;
        Self::new(hertz)
    }

    /// The value in hertz.
    #[must_use]
    pub const fn hertz(self) -> u32 {
        self.0
    }
}

impl fmt::Display for RepeaterFrequency {
    /// Megahertz with six fractional digits, for example `434.320000 MHz`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{:06} MHz",
            self.0 / 1_000_000,
            self.0 % 1_000_000
        )
    }
}

/// Record byte 67: the direction of the repeater's input from its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepeaterShift {
    /// `1`: the input is below the output.
    Minus,
    /// `2`: the input is above the output.
    Plus,
    /// Any other stored value, retained exactly.
    Unnamed(u8),
}

impl From<u8> for RepeaterShift {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Minus,
            2 => Self::Plus,
            other => Self::Unnamed(other),
        }
    }
}

impl From<RepeaterShift> for u8 {
    fn from(value: RepeaterShift) -> Self {
        match value {
            RepeaterShift::Minus => 1,
            RepeaterShift::Plus => 2,
            RepeaterShift::Unnamed(raw) => raw,
        }
    }
}

impl fmt::Display for RepeaterShift {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Minus => formatter.write_str("-"),
            Self::Plus => formatter.write_str("+"),
            Self::Unnamed(value) => write!(formatter, "unnamed value {value}"),
        }
    }
}

/// Record byte 65: how exact the stored position is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionAccuracy {
    /// `1`: the catalog's `Approx.` position.
    Approximate,
    /// `2`: the catalog's `Exact` position.
    Exact,
    /// Any other stored value, retained exactly.
    Unnamed(u8),
}

impl From<u8> for PositionAccuracy {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Approximate,
            2 => Self::Exact,
            other => Self::Unnamed(other),
        }
    }
}

impl From<PositionAccuracy> for u8 {
    fn from(value: PositionAccuracy) -> Self {
        match value {
            PositionAccuracy::Approximate => 1,
            PositionAccuracy::Exact => 2,
            PositionAccuracy::Unnamed(raw) => raw,
        }
    }
}

impl fmt::Display for PositionAccuracy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approximate => formatter.write_str("approximate"),
            Self::Exact => formatter.write_str("exact"),
            Self::Unnamed(value) => write!(formatter, "unnamed value {value}"),
        }
    }
}

/// An angle as whole degrees, whole minutes and ten-thousandths of a minute,
/// stored as a degree byte, a minute byte and a little-endian `u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coordinate {
    degrees: u8,
    minutes: u8,
    ten_thousandths: u16,
}

impl Coordinate {
    /// Validate the minute fields.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::FieldOutOfRange`] for minutes above 59 or
    /// ten-thousandths above 9999.
    pub const fn new(
        degrees: u8,
        minutes: u8,
        ten_thousandths: u16,
    ) -> Result<Self, RepeaterListError> {
        if minutes > 59 {
            return Err(RepeaterListError::FieldOutOfRange {
                field: "position minutes",
                value: minutes as u32,
                max: 59,
            });
        }
        if ten_thousandths > 9999 {
            return Err(RepeaterListError::FieldOutOfRange {
                field: "position minute fraction",
                value: ten_thousandths as u32,
                max: 9999,
            });
        }
        Ok(Self {
            degrees,
            minutes,
            ten_thousandths,
        })
    }

    /// Whole degrees.
    #[must_use]
    pub const fn degrees(self) -> u8 {
        self.degrees
    }

    /// Whole minutes, 0 to 59.
    #[must_use]
    pub const fn minutes(self) -> u8 {
        self.minutes
    }

    /// Ten-thousandths of a minute, 0 to 9999.
    #[must_use]
    pub const fn ten_thousandths(self) -> u16 {
        self.ten_thousandths
    }

    /// The angle in decimal degrees.
    fn magnitude(self) -> f64 {
        let minutes = f64::from(self.minutes) + f64::from(self.ten_thousandths) / 10_000.0;
        f64::from(self.degrees) + minutes / 60.0
    }

    /// Whether the angle exceeds `limit` whole degrees.
    const fn exceeds(self, limit: u8) -> bool {
        self.degrees > limit
            || (self.degrees == limit && (self.minutes > 0 || self.ten_thousandths > 0))
    }

    const fn from_stored(stored: [u8; 4]) -> Self {
        let [degrees, minutes, low, high] = stored;
        Self {
            degrees,
            minutes,
            ten_thousandths: u16::from_le_bytes([low, high]),
        }
    }

    const fn to_stored(self) -> [u8; 4] {
        let [low, high] = self.ten_thousandths.to_le_bytes();
        [self.degrees, self.minutes, low, high]
    }
}

/// North or south of the equator: bit `0x04` of record byte 64 is south.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatitudeHemisphere {
    /// North of the equator.
    North,
    /// South of the equator.
    South,
}

/// East or west of Greenwich: bit `0x08` of record byte 64 is west.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LongitudeHemisphere {
    /// East of Greenwich.
    East,
    /// West of Greenwich.
    West,
}

/// A latitude of at most 90 degrees, record bytes 56 to 59.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Latitude {
    hemisphere: LatitudeHemisphere,
    coordinate: Coordinate,
}

impl Latitude {
    /// Validate the angle.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::FieldOutOfRange`] beyond 90 degrees.
    pub const fn new(
        hemisphere: LatitudeHemisphere,
        coordinate: Coordinate,
    ) -> Result<Self, RepeaterListError> {
        if coordinate.exceeds(90) {
            return Err(RepeaterListError::FieldOutOfRange {
                field: "latitude degrees",
                value: coordinate.degrees as u32,
                max: 90,
            });
        }
        Ok(Self {
            hemisphere,
            coordinate,
        })
    }

    /// North or south.
    #[must_use]
    pub const fn hemisphere(self) -> LatitudeHemisphere {
        self.hemisphere
    }

    /// The angle from the equator.
    #[must_use]
    pub const fn coordinate(self) -> Coordinate {
        self.coordinate
    }

    /// Signed decimal degrees, negative to the south.
    #[must_use]
    pub fn degrees(self) -> f64 {
        match self.hemisphere {
            LatitudeHemisphere::North => self.coordinate.magnitude(),
            LatitudeHemisphere::South => -self.coordinate.magnitude(),
        }
    }
}

/// A longitude of at most 180 degrees, record bytes 60 to 63.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Longitude {
    hemisphere: LongitudeHemisphere,
    coordinate: Coordinate,
}

impl Longitude {
    /// Validate the angle.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::FieldOutOfRange`] beyond 180 degrees.
    pub const fn new(
        hemisphere: LongitudeHemisphere,
        coordinate: Coordinate,
    ) -> Result<Self, RepeaterListError> {
        if coordinate.exceeds(180) {
            return Err(RepeaterListError::FieldOutOfRange {
                field: "longitude degrees",
                value: coordinate.degrees as u32,
                max: 180,
            });
        }
        Ok(Self {
            hemisphere,
            coordinate,
        })
    }

    /// East or west.
    #[must_use]
    pub const fn hemisphere(self) -> LongitudeHemisphere {
        self.hemisphere
    }

    /// The angle from Greenwich.
    #[must_use]
    pub const fn coordinate(self) -> Coordinate {
        self.coordinate
    }

    /// Signed decimal degrees, negative to the west.
    #[must_use]
    pub fn degrees(self) -> f64 {
        match self.hemisphere {
            LongitudeHemisphere::East => self.coordinate.magnitude(),
            LongitudeHemisphere::West => -self.coordinate.magnitude(),
        }
    }
}

/// A record's position: record bytes 56 to 65.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepeaterPosition {
    /// How exact the position is.
    pub accuracy: PositionAccuracy,
    /// The latitude.
    pub latitude: Latitude,
    /// The longitude.
    pub longitude: Longitude,
}

/// Record byte 66: the repeater's UTC offset, stored as quarter hours from
/// UTC−14:00 (`92` is UTC+09:00).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcOffset(u8);

impl UtcOffset {
    /// Validate an offset in minutes east of UTC.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::UtcOffsetOutOfRange`] unless `minutes` is
    /// a multiple of 15 from −840 to 840.
    pub fn from_minutes(minutes: i16) -> Result<Self, RepeaterListError> {
        let refused = || RepeaterListError::UtcOffsetOutOfRange { minutes };
        if minutes % 15 != 0 {
            return Err(refused());
        }
        let quarter_hours =
            u8::try_from((i32::from(minutes) + 840) / 15).map_err(|_negative| refused())?;
        Self::from_stored(quarter_hours).map_err(|_beyond| refused())
    }

    fn from_stored(quarter_hours: u8) -> Result<Self, RepeaterListError> {
        if quarter_hours > MAX_QUARTER_HOURS {
            return Err(RepeaterListError::FieldOutOfRange {
                field: "UTC offset",
                value: u32::from(quarter_hours),
                max: u32::from(MAX_QUARTER_HOURS),
            });
        }
        Ok(Self(quarter_hours))
    }

    /// Minutes east of UTC, −840 to 840.
    #[must_use]
    pub fn minutes(self) -> i16 {
        i16::from(self.0) * 15 - 840
    }
}

impl fmt::Display for UtcOffset {
    /// `+09:00` or `-03:30`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let minutes = self.minutes();
        let sign = if minutes < 0 { '-' } else { '+' };
        let magnitude = minutes.unsigned_abs();
        write!(
            formatter,
            "{sign}{:02}:{:02}",
            magnitude / 60,
            magnitude % 60
        )
    }
}

macro_rules! table_index {
    ($name:ident, $raw:ty, $count:literal, $field:literal, $table:literal) => {
        #[doc = concat!("An index into the ", $table, " name table, below ", $count, ".")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($raw);

        impl $name {
            /// Entries in the table.
            pub const COUNT: $raw = $count;

            /// Validate an index.
            ///
            /// # Errors
            ///
            /// Returns [`RepeaterListError::FieldOutOfRange`] at or past
            /// [`Self::COUNT`].
            pub const fn new(index: $raw) -> Result<Self, RepeaterListError> {
                if index < Self::COUNT {
                    Ok(Self(index))
                } else {
                    Err(RepeaterListError::FieldOutOfRange {
                        field: $field,
                        value: index as u32,
                        max: Self::COUNT as u32 - 1,
                    })
                }
            }

            /// The index.
            #[must_use]
            pub const fn get(self) -> $raw {
                self.0
            }
        }
    };
}

table_index!(RegionIndex, u8, 6, "region index", "world-region");
table_index!(CountryIndex, u8, 150, "country index", "country");
table_index!(GroupIndex, u16, 300, "group index", "group");

/// One populated record slot, held as its 80 stored bytes.
///
/// [`Self::from_bytes`] validates every field it decodes, so the accessors
/// cannot fail; each setter rewrites only its own field's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepeaterRecord {
    bytes: [u8; REPEATER_RECORD_SIZE],
}

impl RepeaterRecord {
    /// Decode one stored slot; `None` when bit `0x10` of byte 75 marks it
    /// empty.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::Text`] or
    /// [`RepeaterListError::MalformedCallsign`] for a text field that is not
    /// a stored form, and [`RepeaterListError::FieldOutOfRange`] for a
    /// frequency or offset of `FFFFFFFF`, a coordinate, UTC offset or table
    /// index outside its domain.
    pub fn from_bytes(
        bytes: [u8; REPEATER_RECORD_SIZE],
    ) -> Result<Option<Self>, RepeaterListError> {
        let record = Self { bytes };
        if record.byte(STATE) & EMPTY_SLOT != 0 {
            return Ok(None);
        }
        for offset in [NAME, SUB_NAME] {
            let _text = StoredText::from_bytes(record.text_field(offset))?;
        }
        for offset in [CALLSIGN, GATEWAY] {
            validate_callsign(record.callsign_field(offset))?;
        }
        for offset in [FREQUENCY, OFFSET] {
            let _frequency = RepeaterFrequency::new(record.word(offset))?;
        }
        let position = record.position();
        for stored in [position.latitude.coordinate, position.longitude.coordinate] {
            let _coordinate =
                Coordinate::new(stored.degrees, stored.minutes, stored.ten_thousandths)?;
        }
        let _latitude = Latitude::new(position.latitude.hemisphere, position.latitude.coordinate)?;
        let _longitude =
            Longitude::new(position.longitude.hemisphere, position.longitude.coordinate)?;
        let _offset = UtcOffset::from_stored(record.byte(ZONE))?;
        let _region = index(record.byte(REGION), NO_INDEX, RegionIndex::new)?;
        let _country = index(record.byte(COUNTRY), NO_INDEX, CountryIndex::new)?;
        let _group = index(record.half_word(GROUP), NO_GROUP, GroupIndex::new)?;
        Ok(Some(record))
    }

    /// The stored bytes, including every byte without a field accessor.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; REPEATER_RECORD_SIZE] {
        self.bytes
    }

    /// The name, bytes 0 to 15.
    #[must_use]
    pub fn name(&self) -> StoredText<REPEATER_TEXT_SIZE> {
        StoredText::validated(self.text_field(NAME))
    }

    /// The sub-name, bytes 16 to 31.
    #[must_use]
    pub fn sub_name(&self) -> StoredText<REPEATER_TEXT_SIZE> {
        StoredText::validated(self.text_field(SUB_NAME))
    }

    /// The repeater callsign with its module letter, bytes 32 to 39.
    #[must_use]
    pub fn callsign(&self) -> StoredCallsign {
        StoredCallsign::from_bytes(self.callsign_field(CALLSIGN))
    }

    /// The gateway callsign, bytes 40 to 47; empty when all NUL.
    #[must_use]
    pub fn gateway(&self) -> StoredCallsign {
        StoredCallsign::from_bytes(self.callsign_field(GATEWAY))
    }

    /// The output frequency, bytes 48 to 51 little-endian.
    #[must_use]
    pub fn frequency(&self) -> RepeaterFrequency {
        RepeaterFrequency(self.word(FREQUENCY))
    }

    /// The input offset, bytes 52 to 55 little-endian.
    #[must_use]
    pub fn offset(&self) -> RepeaterFrequency {
        RepeaterFrequency(self.word(OFFSET))
    }

    /// The offset direction.
    #[must_use]
    pub fn shift(&self) -> RepeaterShift {
        RepeaterShift::from(self.byte(SHIFT))
    }

    /// The stored position.
    #[must_use]
    pub fn position(&self) -> RepeaterPosition {
        let hemispheres = self.byte(HEMISPHERES);
        RepeaterPosition {
            accuracy: PositionAccuracy::from(self.byte(ACCURACY)),
            latitude: Latitude {
                hemisphere: if hemispheres & SOUTH == 0 {
                    LatitudeHemisphere::North
                } else {
                    LatitudeHemisphere::South
                },
                coordinate: Coordinate::from_stored(self.field(LATITUDE)),
            },
            longitude: Longitude {
                hemisphere: if hemispheres & WEST == 0 {
                    LongitudeHemisphere::East
                } else {
                    LongitudeHemisphere::West
                },
                coordinate: Coordinate::from_stored(self.field(LONGITUDE)),
            },
        }
    }

    /// The UTC offset.
    #[must_use]
    pub fn utc_offset(&self) -> UtcOffset {
        UtcOffset(self.byte(ZONE))
    }

    /// The world region, byte 70; `None` when it stores `FF`.
    #[must_use]
    pub fn region(&self) -> Option<RegionIndex> {
        Some(self.byte(REGION))
            .filter(|&raw| raw != NO_INDEX)
            .map(RegionIndex)
    }

    /// The country, byte 71; `None` when it stores `FF`.
    #[must_use]
    pub fn country(&self) -> Option<CountryIndex> {
        Some(self.byte(COUNTRY))
            .filter(|&raw| raw != NO_INDEX)
            .map(CountryIndex)
    }

    /// The group, bytes 72 and 73 little-endian; `None` when they store
    /// `FFFF`.
    #[must_use]
    pub fn group(&self) -> Option<GroupIndex> {
        Some(self.half_word(GROUP))
            .filter(|&raw| raw != NO_GROUP)
            .map(GroupIndex)
    }

    /// The slot number of the previous record in the list, bytes 76 and 77;
    /// `None` for the first record.
    #[must_use]
    pub fn previous(&self) -> Option<u16> {
        Some(self.half_word(PREVIOUS)).filter(|&raw| raw != NO_LINK)
    }

    /// The slot number of the next record in the list, bytes 78 and 79;
    /// `None` for the last record.
    #[must_use]
    pub fn next(&self) -> Option<u16> {
        Some(self.half_word(NEXT)).filter(|&raw| raw != NO_LINK)
    }

    /// Replace the name.
    pub fn set_name(&mut self, name: StoredText<REPEATER_TEXT_SIZE>) {
        self.put(NAME, name.as_bytes());
    }

    /// Replace the sub-name.
    pub fn set_sub_name(&mut self, sub_name: StoredText<REPEATER_TEXT_SIZE>) {
        self.put(SUB_NAME, sub_name.as_bytes());
    }

    /// Replace the repeater callsign.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::MalformedCallsign`], leaving the record
    /// unchanged, unless the bytes are graphic ASCII or space followed by NUL
    /// padding.
    pub fn set_callsign(&mut self, callsign: StoredCallsign) -> Result<(), RepeaterListError> {
        validate_callsign(*callsign.as_bytes())?;
        self.put(CALLSIGN, callsign.as_bytes());
        Ok(())
    }

    /// Replace the gateway callsign.
    ///
    /// # Errors
    ///
    /// As [`Self::set_callsign`].
    pub fn set_gateway(&mut self, gateway: StoredCallsign) -> Result<(), RepeaterListError> {
        validate_callsign(*gateway.as_bytes())?;
        self.put(GATEWAY, gateway.as_bytes());
        Ok(())
    }

    /// Replace the output frequency.
    pub fn set_frequency(&mut self, frequency: RepeaterFrequency) {
        self.put(FREQUENCY, &frequency.0.to_le_bytes());
    }

    /// Replace the input offset.
    pub fn set_offset(&mut self, offset: RepeaterFrequency) {
        self.put(OFFSET, &offset.0.to_le_bytes());
    }

    /// Replace the offset direction.
    pub fn set_shift(&mut self, shift: RepeaterShift) {
        self.put(SHIFT, &[u8::from(shift)]);
    }

    /// Replace the position; the other bits of byte 64 are kept.
    pub fn set_position(&mut self, position: RepeaterPosition) {
        let mut hemispheres = self.byte(HEMISPHERES) & !(SOUTH | WEST);
        if position.latitude.hemisphere == LatitudeHemisphere::South {
            hemispheres |= SOUTH;
        }
        if position.longitude.hemisphere == LongitudeHemisphere::West {
            hemispheres |= WEST;
        }
        self.put(LATITUDE, &position.latitude.coordinate.to_stored());
        self.put(LONGITUDE, &position.longitude.coordinate.to_stored());
        self.put(HEMISPHERES, &[hemispheres, u8::from(position.accuracy)]);
    }

    /// Replace the UTC offset.
    pub fn set_utc_offset(&mut self, offset: UtcOffset) {
        self.put(ZONE, &[offset.0]);
    }

    /// Replace the world region; `None` stores `FF`.
    pub fn set_region(&mut self, region: Option<RegionIndex>) {
        self.put(REGION, &[region.map_or(NO_INDEX, RegionIndex::get)]);
    }

    /// Replace the country; `None` stores `FF`.
    pub fn set_country(&mut self, country: Option<CountryIndex>) {
        self.put(COUNTRY, &[country.map_or(NO_INDEX, CountryIndex::get)]);
    }

    /// Replace the group; `None` stores `FFFF`.
    pub fn set_group(&mut self, group: Option<GroupIndex>) {
        self.put(
            GROUP,
            &group.map_or(NO_GROUP, GroupIndex::get).to_le_bytes(),
        );
    }

    /// Replace the links; `None` stores `FFFF`.
    pub(super) fn set_links(&mut self, previous: Option<u16>, next: Option<u16>) {
        self.put(PREVIOUS, &previous.unwrap_or(NO_LINK).to_le_bytes());
        self.put(NEXT, &next.unwrap_or(NO_LINK).to_le_bytes());
    }

    fn byte(&self, offset: usize) -> u8 {
        self.bytes.get(offset).copied().unwrap_or_default()
    }

    fn half_word(&self, offset: usize) -> u16 {
        u16::from_le_bytes(self.field(offset))
    }

    fn word(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.field(offset))
    }

    fn text_field(&self, offset: usize) -> [u8; REPEATER_TEXT_SIZE] {
        self.field(offset)
    }

    fn callsign_field(&self, offset: usize) -> [u8; CALLSIGN_SIZE] {
        self.field(offset)
    }

    fn field<const N: usize>(&self, offset: usize) -> [u8; N] {
        let mut field = [0; N];
        for (destination, source) in field.iter_mut().zip(self.bytes.iter().skip(offset)) {
            *destination = *source;
        }
        field
    }

    fn put(&mut self, offset: usize, value: &[u8]) {
        for (destination, source) in self.bytes.iter_mut().skip(offset).zip(value) {
            *destination = *source;
        }
    }
}

/// Graphic ASCII or space, then NUL padding.
fn validate_callsign(field: [u8; CALLSIGN_SIZE]) -> Result<(), RepeaterListError> {
    if StoredCallsign::from_bytes(field).is_padded_text() {
        Ok(())
    } else {
        Err(RepeaterListError::MalformedCallsign { bytes: field })
    }
}

/// Validate a stored table index unless it is the all-ones "no entry" value.
fn index<R: PartialEq + Copy, T>(
    raw: R,
    none: R,
    new: impl FnOnce(R) -> Result<T, RepeaterListError>,
) -> Result<Option<T>, RepeaterListError> {
    if raw == none {
        Ok(None)
    } else {
        new(raw).map(Some)
    }
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
