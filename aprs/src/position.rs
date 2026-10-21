//! APRS position reports (uncompressed and compressed).
//!
//! See the [packet construction and parsing example](crate#packet-construction-and-parsing).

use crate::error::{AprsError, AprsWireTextField};
use crate::mic_e::{MiceMessage, MiceTelemetry};
use crate::packet::{AprsDataExtension, AprsReportTimestamp, CourseSpeed, PositionAmbiguity};
use crate::text::decode_wire_ascii;
use crate::units::{AprsSymbol, Course, Latitude, Longitude, Speed, SymbolTable};
use crate::weather::AprsWeather;

/// A parsed APRS position report.
///
/// Includes optional speed/course fields populated by Mic-E decoding and
/// optional embedded weather data populated when the station reports with
/// the weather-station symbol code `_`. Data extensions (course/speed,
/// PHG, altitude, DAO) found in the comment field are parsed
/// automatically and exposed via [`Self::extensions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsPosition {
    /// Optional seven-byte timestamp carried by `/` and `@` reports.
    /// Untimestamped `!`/`=` reports and Mic-E positions use `None`.
    timestamp: Option<AprsReportTimestamp>,
    /// Latitude in decimal degrees (positive = North).
    latitude: Latitude,
    /// Longitude in decimal degrees (positive = East).
    longitude: Longitude,
    /// APRS symbol table identifier character.
    symbol: AprsSymbol,
    /// Motion reported in the position body, taking priority over comment extensions.
    motion: PositionMotion,
    /// Optional comment/extension text after the position.
    comment: String,
    /// Optional weather data embedded in the position comment.
    ///
    /// Populated when the symbol code is `_` (weather station) and the
    /// comment starts with the `DDD/SSS` wind direction/speed extension,
    /// followed by the remaining weather fields. See APRS 1.0.1 §12.1.
    weather: Option<AprsWeather>,
    /// Parsed data extensions (course/speed, PHG, altitude, DAO) found in
    /// the comment field.
    ///
    /// Populated automatically by [`Self::from_bytes`] through
    /// [`AprsDataExtension::from_comment`]. Fields that aren't present in the
    /// comment are `None`.
    extensions: AprsDataExtension,
    /// Mic-E standard message code (only populated by
    /// [`Self::from_mice_bytes`]).
    mice_message: Option<MiceMessage>,
    /// Mic-E altitude in metres, decoded from the comment per APRS 1.0.1
    /// §10.1.1 (three base-91 chars followed by `}`, offset from -10000).
    mice_altitude_m: Option<i32>,
    /// Optional Mic-E telemetry decoded from the field that is mutually
    /// exclusive with Mic-E status text.
    mice_telemetry: Option<MiceTelemetry>,
    /// Position ambiguity level (APRS 1.0.1 §8.1.6).
    ///
    /// Stations can deliberately reduce their precision by replacing
    /// trailing lat/lon digits with spaces; this field records how many
    /// digits were masked. Mic-E masks the corresponding longitude digits;
    /// compressed positions use [`PositionAmbiguity::None`].
    ambiguity: PositionAmbiguity,
    compressed: Option<CompressedPositionData>,
}

/// Independently reported speed and course, with checked units and domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PositionMotion {
    speed: Option<Speed>,
    course: Option<Course>,
}

impl PositionMotion {
    /// Combine available motion components; either may be unknown independently.
    #[must_use]
    pub const fn new(speed: Option<Speed>, course: Option<Course>) -> Self {
        Self { speed, course }
    }
    /// Return reported speed without format-dependent integer rounding.
    #[must_use]
    pub const fn speed(self) -> Option<Speed> {
        self.speed
    }
    /// Return reported course, or None when the encoding marks it unknown.
    #[must_use]
    pub const fn course(self) -> Option<Course> {
        self.course
    }
}

/// One compressed radio-range digit, measured as `2 × 1.08^s` statute miles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompressedRange(u8);
impl CompressedRange {
    /// Validate the base-91 numeric digit `0..=90`.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] for a digit above 90.
    pub const fn from_code(code: u8) -> Result<Self, AprsError> {
        if code <= 90 {
            Ok(Self(code))
        } else {
            Err(AprsError::InvalidFormat)
        }
    }
    /// Return the exact received base-91 numeric digit.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.0
    }
    /// Return pre-calculated radio range in statute miles.
    #[must_use]
    pub fn miles(self) -> f64 {
        2.0 * 1.08_f64.powi(i32::from(self.0))
    }
}
impl TryFrom<u8> for CompressedRange {
    type Error = AprsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_code(value)
    }
}
impl From<CompressedRange> for u8 {
    fn from(value: CompressedRange) -> Self {
        value.0
    }
}

/// The checked two-digit exponent of compressed altitude in feet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompressedAltitude(u16);
impl CompressedAltitude {
    /// Validate a combined two-digit exponent, accepting `0..=8280`.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] above the largest two-digit value.
    pub const fn from_exponent(exponent: u16) -> Result<Self, AprsError> {
        if exponent <= 8280 {
            Ok(Self(exponent))
        } else {
            Err(AprsError::InvalidFormat)
        }
    }
    /// Validate and combine two base-91 numeric digits, each `0..=90`.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] when either digit exceeds 90.
    pub fn from_codes(high: u8, low: u8) -> Result<Self, AprsError> {
        if high > 90 || low > 90 {
            return Err(AprsError::InvalidFormat);
        }
        Self::from_exponent(u16::from(high) * 91 + u16::from(low))
    }
    /// Return the exact exponent, from zero through 8280.
    #[must_use]
    pub const fn exponent(self) -> u16 {
        self.0
    }
    /// Return altitude in feet, preserving fractional precision.
    #[must_use]
    pub fn feet(self) -> f64 {
        1.002_f64.powi(i32::from(self.0))
    }
}
impl TryFrom<[u8; 2]> for CompressedAltitude {
    type Error = AprsError;
    fn try_from([high, low]: [u8; 2]) -> Result<Self, Self::Error> {
        Self::from_codes(high, low)
    }
}
impl TryFrom<u16> for CompressedAltitude {
    type Error = AprsError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::from_exponent(value)
    }
}
impl From<CompressedAltitude> for u16 {
    fn from(value: CompressedAltitude) -> Self {
        value.0
    }
}

/// Mutually exclusive interpretation of the compressed `csT` tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressedPositionData {
    /// The leading `c` is space; the other two bytes are uninterpreted fillers.
    None,
    /// Course/speed for other, GLL or RMC source bits.
    CourseSpeed(CourseSpeed),
    /// `c` is `{`, with a pre-calculated radio range.
    RadioRange(CompressedRange),
    /// GGA source bits select altitude.
    Altitude(CompressedAltitude),
}

impl AprsPosition {
    /// Construct an exact position from validated coordinates and symbol.
    #[must_use]
    pub fn new(latitude: Latitude, longitude: Longitude, symbol: AprsSymbol) -> Self {
        Self {
            timestamp: None,
            latitude,
            longitude,
            symbol,
            motion: PositionMotion::default(),
            comment: String::new(),
            weather: None,
            extensions: AprsDataExtension::default(),
            mice_message: None,
            mice_altitude_m: None,
            mice_telemetry: None,
            ambiguity: PositionAmbiguity::None,
            compressed: None,
        }
    }
    /// Parse a complete uncompressed or compressed position information field.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError`] for an invalid type, timestamp, coordinate, symbol,
    /// compression digit or ASCII comment. Mic-E uses [`Self::from_mice_bytes`].
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_position(info)
    }
    /// Parse the 19-byte uncompressed position body and its optional comment.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError`] for missing bytes, malformed coordinates/symbol or non-ASCII text.
    pub fn from_uncompressed_body(body: &[u8]) -> Result<Self, AprsError> {
        parse_uncompressed_body(body)
    }
    /// Parse the 13-byte compressed position body and its optional comment.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError`] for missing bytes, out-of-range coordinates, symbol,
    /// compression digits or non-ASCII text.
    pub fn from_compressed_body(body: &[u8]) -> Result<Self, AprsError> {
        parse_compressed_body(body)
    }
    /// Return the validated latitude; ambiguous reports retain their lower magnitude bound.
    #[must_use]
    pub const fn latitude(&self) -> Latitude {
        self.latitude
    }
    /// Return the validated longitude; ambiguous reports retain their lower magnitude bound.
    #[must_use]
    pub const fn longitude(&self) -> Longitude {
        self.longitude
    }
    /// Return the complete validated symbol.
    #[must_use]
    pub const fn symbol(&self) -> AprsSymbol {
        self.symbol
    }
    /// Return the report's optional observation timestamp.
    #[must_use]
    pub const fn timestamp(&self) -> Option<AprsReportTimestamp> {
        self.timestamp
    }
    /// Return motion encoded in the position body, before comment-extension fallback.
    #[must_use]
    pub const fn motion(&self) -> PositionMotion {
        self.motion
    }
    /// Return native motion speed, falling back to the initial comment extension.
    #[must_use]
    pub fn speed(&self) -> Option<Speed> {
        self.motion
            .speed
            .or_else(|| self.extensions.course_speed().map(CourseSpeed::speed))
    }
    /// Return native motion course, falling back to the initial comment extension.
    #[must_use]
    pub fn course(&self) -> Option<Course> {
        self.motion
            .course
            .or_else(|| self.extensions.course_speed().map(CourseSpeed::course))
    }
    /// Borrow the exact decoded ASCII comment.
    #[must_use]
    pub fn comment(&self) -> &str {
        &self.comment
    }
    /// Borrow weather derived from this symbol and comment.
    #[must_use]
    pub const fn weather(&self) -> Option<&AprsWeather> {
        self.weather.as_ref()
    }
    /// Borrow immutable comment extensions, including a rounded compressed altitude when present.
    #[must_use]
    pub const fn extensions(&self) -> &AprsDataExtension {
        &self.extensions
    }
    /// Return the number of masked coordinate digits.
    #[must_use]
    pub const fn ambiguity(&self) -> PositionAmbiguity {
        self.ambiguity
    }
    /// Return the compressed tail interpretation, retaining range and altitude precision.
    #[must_use]
    pub const fn compressed_data(&self) -> Option<CompressedPositionData> {
        self.compressed
    }
    /// Return the Mic-E standard message code when present.
    #[must_use]
    pub const fn mice_message(&self) -> Option<MiceMessage> {
        self.mice_message
    }
    /// Return Mic-E altitude in metres when present.
    #[must_use]
    pub const fn mice_altitude_m(&self) -> Option<i32> {
        self.mice_altitude_m
    }
    /// Return the mutually exclusive Mic-E telemetry block when present.
    #[must_use]
    pub const fn mice_telemetry(&self) -> Option<MiceTelemetry> {
        self.mice_telemetry
    }
    /// Replace native motion and discard a compressed course/speed interpretation.
    #[must_use]
    pub(crate) const fn with_motion(mut self, motion: PositionMotion) -> Self {
        self.motion = motion;
        if matches!(
            self.compressed,
            Some(CompressedPositionData::CourseSpeed(_))
        ) {
            self.compressed = None;
        }
        self
    }
    /// Replace ASCII comment text and recompute all comment-derived views.
    ///
    /// Native position-body motion and compressed altitude retain priority.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError`] for non-ASCII text, malformed embedded weather, or
    /// nonempty text on a Mic-E report carrying mutually exclusive telemetry.
    /// Validation completes before the replacement's correlated fields change.
    pub(crate) fn with_comment(mut self, comment: &str) -> Result<Self, AprsError> {
        if self.mice_telemetry.is_some() && !comment.is_empty() {
            return Err(AprsError::InvalidFormat);
        }
        let comment = decode_wire_ascii(
            AprsWireTextField::UncompressedPositionComment,
            comment.as_bytes(),
        )?;
        let weather = AprsWeather::from_position_comment(char::from(self.symbol.code()), comment)?;
        let mut extensions = AprsDataExtension::from_comment(comment);
        if let Some(CompressedPositionData::Altitude(altitude)) = self.compressed {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the checked exponent is at most 8280; rounded 1.002^8280 feet fits i32"
            )]
            {
                extensions = extensions.with_altitude(altitude.feet().round() as i32);
            }
        }
        comment.clone_into(&mut self.comment);
        self.weather = weather;
        self.extensions = extensions;
        Ok(self)
    }
    pub(crate) const fn with_mice(
        mut self,
        message: Option<MiceMessage>,
        altitude: Option<i32>,
        telemetry: Option<MiceTelemetry>,
        ambiguity: PositionAmbiguity,
    ) -> Self {
        self.mice_message = message;
        self.mice_altitude_m = altitude;
        self.mice_telemetry = telemetry;
        self.ambiguity = ambiguity;
        self
    }
}

/// Parse APRS latitude from the standard `DDMM.HH[N/S]` format.
///
/// Returns `(degrees, ambiguity)` where `degrees` is the decimal-degree
/// value (positive North) and `ambiguity` counts how many trailing
/// digits were replaced with spaces per APRS 1.0.1 §8.1.6. Rejects
/// out-of-range fields: minutes must be below 60 and the resulting
/// magnitude at most 90°.
fn parse_aprs_latitude(s: &[u8]) -> Result<(f64, PositionAmbiguity), AprsError> {
    let bytes_slice = s.get(..8).ok_or(AprsError::InvalidCoordinates)?;
    let bytes: [u8; 8] = bytes_slice
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    // Field layout: DD MM . HH H/S   (indices 0..7, hemisphere at 7)
    let field = bytes.get(..7).ok_or(AprsError::InvalidCoordinates)?;
    let (digits, ambiguity) = unmask_coord_digits(field, 4)?;
    let text = std::str::from_utf8(&digits).map_err(|_| AprsError::InvalidCoordinates)?;
    let deg_str = text.get(..2).ok_or(AprsError::InvalidCoordinates)?;
    let min_str = text.get(2..7).ok_or(AprsError::InvalidCoordinates)?;
    let degrees: f64 = deg_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = min_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    // Range validation: minutes run 0..60 and latitude degrees at most
    // 90. Without this, garbage like `9999.99N` parses to an
    // impossible 100.67° that then flows into distance calculations
    // and map consumers.
    if minutes >= 60.0 || degrees > 90.0 {
        return Err(AprsError::InvalidCoordinates);
    }
    let hemisphere = *bytes.get(7).ok_or(AprsError::InvalidCoordinates)?;

    let mut lat = degrees + minutes / 60.0;
    if hemisphere == b'S' {
        lat = -lat;
    } else if hemisphere != b'N' {
        return Err(AprsError::InvalidCoordinates);
    }
    // 90° with nonzero minutes slips past the per-field checks; the
    // combined magnitude must not exceed the pole.
    if lat.abs() > 90.0 {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok((lat, ambiguity))
}

/// Parse APRS longitude from the standard `DDDMM.HH[E/W]` format.
///
/// Rejects out-of-range fields: minutes must be below 60 and the
/// resulting magnitude at most 180°.
fn parse_aprs_longitude(s: &[u8]) -> Result<(f64, PositionAmbiguity), AprsError> {
    let bytes_slice = s.get(..9).ok_or(AprsError::InvalidCoordinates)?;
    let bytes: [u8; 9] = bytes_slice
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    // Field layout: DDD MM . HH E/W  (indices 0..8, hemisphere at 8)
    let field = bytes.get(..8).ok_or(AprsError::InvalidCoordinates)?;
    let (digits, ambiguity) = unmask_coord_digits(field, 5)?;
    let text = std::str::from_utf8(&digits).map_err(|_| AprsError::InvalidCoordinates)?;
    let deg_str = text.get(..3).ok_or(AprsError::InvalidCoordinates)?;
    let min_str = text.get(3..8).ok_or(AprsError::InvalidCoordinates)?;
    let degrees: f64 = deg_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = min_str.parse().map_err(|_| AprsError::InvalidCoordinates)?;
    // Range validation: minutes run 0..60 and longitude degrees at
    // most 180. Rejects impossible values like `36101.75W` (361°).
    if minutes >= 60.0 || degrees > 180.0 {
        return Err(AprsError::InvalidCoordinates);
    }
    let hemisphere = *bytes.get(8).ok_or(AprsError::InvalidCoordinates)?;

    let mut lon = degrees + minutes / 60.0;
    if hemisphere == b'W' {
        lon = -lon;
    } else if hemisphere != b'E' {
        return Err(AprsError::InvalidCoordinates);
    }
    // 180° with nonzero minutes slips past the per-field checks; the
    // combined magnitude must not exceed the antimeridian.
    if lon.abs() > 180.0 {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok((lon, ambiguity))
}

/// Replace space-masked digits with `'0'` and return the masked-count
/// alongside the rebuilt byte sequence. `dot_idx` is the index of the
/// literal `.` in the field (4 for latitude, 5 for longitude).
fn unmask_coord_digits(
    field: &[u8],
    dot_idx: usize,
) -> Result<([u8; 8], PositionAmbiguity), AprsError> {
    if field.len() > 8 {
        return Err(AprsError::InvalidCoordinates);
    }
    let dot_byte = *field.get(dot_idx).ok_or(AprsError::InvalidCoordinates)?;
    if dot_byte != b'.' {
        return Err(AprsError::InvalidCoordinates);
    }
    // Ambiguity is counted by walking the mask-eligible positions from
    // rightmost back to the start until we stop seeing spaces.
    // Mask order (rightmost first): HH tens, HH ones, MM ones, MM tens
    let mask_order: [usize; 4] = if dot_idx == 4 {
        [6, 5, 3, 2] // lat: HH(6,5), MM(3,2)
    } else {
        [7, 6, 4, 3] // lon: HH(7,6), MM(4,3)
    };
    let mut count: u8 = 0;
    for &pos in &mask_order {
        if field.get(pos) == Some(&b' ') {
            count += 1;
        } else {
            break;
        }
    }
    // Build output buffer.
    let mut out = [b'0'; 8];
    if let Some(dst) = out.get_mut(..field.len()) {
        dst.copy_from_slice(field);
    }
    let masked = mask_order.get(..count as usize).unwrap_or(&[]);
    for pos in masked {
        if let Some(slot) = out.get_mut(*pos) {
            *slot = b'0';
        }
    }
    // Also fail if we see a space at a non-maskable position (outside
    // the trailing run).
    for (i, &b) in field.iter().enumerate() {
        if b == b' ' && !masked.contains(&i) {
            return Err(AprsError::InvalidCoordinates);
        }
        if i != dot_idx && b != b' ' && !b.is_ascii_digit() {
            return Err(AprsError::InvalidCoordinates);
        }
    }
    let ambiguity = match count {
        0 => PositionAmbiguity::None,
        1 => PositionAmbiguity::OneDigit,
        2 => PositionAmbiguity::TwoDigits,
        3 => PositionAmbiguity::ThreeDigits,
        _ => PositionAmbiguity::FourDigits,
    };
    Ok((out, ambiguity))
}

/// Parse an APRS position report from an AX.25 information field.
///
/// Supports three APRS position formats (per APRS101.PDF chapters 8-9):
/// - **Uncompressed**: `!`/`=`/`/`/`@` with ASCII lat/lon (`DDMM.HH`)
/// - **Compressed**: `!`/`=`/`/`/`@` with base-91 encoded lat/lon (13 bytes)
///
/// For **Mic-E** positions (`` ` ``/`'`), use
/// [`AprsPosition::from_mice_bytes`] which also requires the AX.25
/// destination address.
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or coordinates are invalid.
fn parse_aprs_position(info: &[u8]) -> Result<AprsPosition, AprsError> {
    let data_type = *info.first().ok_or(AprsError::InvalidFormat)?;
    let (body, timestamp) = match data_type {
        // Position without timestamp: ! or =
        b'!' | b'=' => (info.get(1..).ok_or(AprsError::InvalidFormat)?, None),
        // Position with timestamp: / or @
        // Timestamp is exactly 7 characters after the type byte.
        b'/' | b'@' => {
            let timestamp_bytes = info.get(1..8).ok_or(AprsError::InvalidFormat)?;
            let timestamp_wire =
                decode_wire_ascii(AprsWireTextField::PositionTimestamp, timestamp_bytes)?;
            let timestamp = AprsReportTimestamp::from_wire(timestamp_wire)?;
            (
                info.get(8..).ok_or(AprsError::InvalidFormat)?,
                Some(timestamp),
            )
        }
        _ => return Err(AprsError::InvalidFormat),
    };

    let first = *body.first().ok_or(AprsError::InvalidFormat)?;
    // Detect compressed vs uncompressed: if the first byte is a digit (0-9),
    // it's uncompressed latitude. Otherwise it's a compressed symbol table char.
    let mut position = if first.is_ascii_digit() {
        parse_uncompressed_body(body)
    } else {
        parse_compressed_body(body)
    }?;
    position.timestamp = timestamp;
    Ok(position)
}

/// Parse uncompressed APRS position body.
///
/// Format: `lat(8) sym_table(1) lon(9) sym_code(1) [comment]` = 19+ bytes.
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the body is shorter than 19
/// bytes or [`AprsError::InvalidCoordinates`] if the latitude or
/// longitude fields are malformed.
fn parse_uncompressed_body(body: &[u8]) -> Result<AprsPosition, AprsError> {
    let lat_slice = body.get(..8).ok_or(AprsError::InvalidFormat)?;
    let (latitude, lat_ambig) = parse_aprs_latitude(lat_slice)?;
    let symbol_table = *body.get(8).ok_or(AprsError::InvalidFormat)? as char;
    let lon_slice = body.get(9..18).ok_or(AprsError::InvalidFormat)?;
    let (longitude, lon_ambig) = parse_aprs_longitude(lon_slice)?;
    let symbol_code = *body.get(18).ok_or(AprsError::InvalidFormat)? as char;
    let symbol = AprsSymbol::from_chars(symbol_table, symbol_code)?;
    if lon_ambig != PositionAmbiguity::None && lon_ambig != lat_ambig {
        return Err(AprsError::InvalidCoordinates);
    }
    // Latitude declares both coordinates' ambiguity (APRS101 chapter6,p24).
    let mut longitude_wire: [u8; 9] = lon_slice
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    for index in [7, 6, 4, 3]
        .into_iter()
        .take(usize::from(lat_ambig.masked_digits()))
    {
        *longitude_wire
            .get_mut(index)
            .ok_or(AprsError::InvalidCoordinates)? = b'0';
    }
    let (longitude, _) = if lat_ambig == PositionAmbiguity::None {
        (longitude, lon_ambig)
    } else {
        parse_aprs_longitude(&longitude_wire)?
    };

    let comment_bytes = body.get(19..).ok_or(AprsError::InvalidFormat)?;
    let comment = decode_wire_ascii(
        AprsWireTextField::UncompressedPositionComment,
        comment_bytes,
    )?;
    let mut position =
        AprsPosition::new(Latitude::new(latitude)?, Longitude::new(longitude)?, symbol)
            .with_comment(comment)?;
    position.ambiguity = lat_ambig;
    Ok(position)
}

/// Parse compressed APRS position body (APRS101.PDF Chapter 9).
///
/// Format: `sym_table(1) YYYY(4) XXXX(4) sym_code(1) cs(1) s(1) t(1)` = 13 bytes.
/// YYYY and XXXX are base-91 encoded (each byte = ASCII `33..=123`,
/// `'!'` to `'{'`, value = byte - 33).
///
/// Latitude:  `90 - (YYYY / 380926.0)` degrees
/// Longitude: `-180 + (XXXX / 190463.0)` degrees
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] if the body is shorter than 13
/// bytes or [`AprsError::InvalidCoordinates`] if the base-91 lat/lon
/// fields contain bytes outside the `33..=123` range.
fn parse_compressed_body(body: &[u8]) -> Result<AprsPosition, AprsError> {
    // Require the full 13-byte compressed body upfront.
    let header = body.get(..13).ok_or(AprsError::InvalidFormat)?;
    let table =
        SymbolTable::from_compressed_byte(*header.first().ok_or(AprsError::InvalidFormat)?)?;
    let lat_bytes = header.get(1..5).ok_or(AprsError::InvalidFormat)?;
    let lon_bytes = header.get(5..9).ok_or(AprsError::InvalidFormat)?;
    let lat_val = decode_base91_4(lat_bytes)?;
    let lon_val = decode_base91_4(lon_bytes)?;
    let symbol = AprsSymbol::new(table, *header.get(9).ok_or(AprsError::InvalidFormat)?)?;

    let latitude = Latitude::new(90.0 - f64::from(lat_val) / 380_926.0)?;
    let longitude = Longitude::new(-180.0 + f64::from(lon_val) / 190_463.0)?;

    // Decode the 3-byte cs/s/t tail per APRS 1.0.1 §9.
    let cs_byte = *header.get(10).ok_or(AprsError::InvalidFormat)?;
    let s_byte = *header.get(11).ok_or(AprsError::InvalidFormat)?;
    let t_byte = *header.get(12).ok_or(AprsError::InvalidFormat)?;
    let compressed = decode_compressed_tail(cs_byte, s_byte, t_byte)?;

    let comment_bytes = body.get(13..).ok_or(AprsError::InvalidFormat)?;
    let comment = decode_wire_ascii(AprsWireTextField::CompressedPositionComment, comment_bytes)?;
    let mut position = AprsPosition::new(latitude, longitude, symbol);
    position.compressed = Some(compressed);
    if let CompressedPositionData::CourseSpeed(motion) = compressed {
        position.motion = PositionMotion::new(Some(motion.speed()), Some(motion.course()));
    }
    position.with_comment(comment)
}

/// Decode the 3-byte `cs`/`s`/`t` compression tail of a compressed APRS
/// position report per APRS 1.0.1 §9 Table 10.
///
/// Course/speed, radio range and altitude are mutually exclusive.
fn decode_compressed_tail(cs: u8, s: u8, t: u8) -> Result<CompressedPositionData, AprsError> {
    // Space in the `cs` column means "no data."
    if cs == b' ' {
        return Ok(CompressedPositionData::None);
    }
    if !(b'!'..=b'{').contains(&cs) || !(b'!'..=b'{').contains(&s) || !(b'!'..=b'`').contains(&t) {
        return Err(AprsError::InvalidFormat);
    }
    // The `t` byte minus 33 gives a 6-bit compression type value. Bits
    // 3-4 (0x18) select the semantic meaning of `cs`/`s`.
    let t_val = t - 33;
    let type_bits = (t_val >> 3) & 0x03;
    match type_bits {
        // Other, GLL and RMC sources carry course/speed or the c='{' range form.
        // degrees. Speed is 1.08^(s - 33) - 1 knots.
        0 | 1 | 3 => {
            if cs == b'{' {
                return Ok(CompressedPositionData::RadioRange(
                    CompressedRange::from_code(s - b'!')?,
                ));
            }
            let c = cs - 33;
            let s_val = s - 33;
            let speed = Speed::from_knots(1.08_f64.powi(i32::from(s_val)) - 1.0)?;
            let course_deg = u16::from(c) * 4;
            Ok(CompressedPositionData::CourseSpeed(CourseSpeed::new(
                Course::new(course_deg)?,
                speed,
            )))
        }
        // 0b10: altitude. cs,s = base-91 two-char altitude, value =
        // 1.002^((cs-33)*91 + (s-33)) feet.
        2 => Ok(CompressedPositionData::Altitude(
            CompressedAltitude::from_codes(cs - b'!', s - b'!')?,
        )),
        _ => unreachable!("two-bit source has four alternatives"),
    }
}

/// Decode a 4-byte base-91 value.
///
/// Each byte is a base-91 digit `0..=90`, encoded as ASCII `33..=123`
/// (`'!'` to `'{'`). The value is:
/// `b[0]*91^3 + b[1]*91^2 + b[2]*91 + b[3]`
///
/// # Errors
///
/// Returns [`AprsError::InvalidCoordinates`] if `bytes` is shorter than
/// 4 bytes or any byte is outside the `33..=123` base-91 range. ASCII
/// `'|'` (124) and above are not valid base-91 digits (they would encode
/// digit 91, one past the legal maximum) and are rejected.
fn decode_base91_4(bytes: &[u8]) -> Result<u32, AprsError> {
    let window = bytes.get(..4).ok_or(AprsError::InvalidCoordinates)?;
    let mut val: u32 = 0;
    for &b in window {
        if !(33..=123).contains(&b) {
            return Err(AprsError::InvalidCoordinates);
        }
        val = val * 91 + u32::from(b - 33);
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Fahrenheit;
    use crate::weather::{BarometricPressure, Humidity, ThreeDigitWeatherValue, WindDirection};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- APRS position tests ----

    #[test]
    fn parse_aprs_position_no_timestamp() -> TestResult {
        let info = b"!4903.50N/07201.75W-Test comment";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.timestamp(), None);
        // 49 degrees 3.50 minutes N = 49.058333...
        assert!(
            (pos.latitude().as_degrees() - 49.058_333).abs() < 0.001,
            "lat={}",
            pos.latitude().as_degrees()
        );
        // 72 degrees 1.75 minutes W = -72.029166...
        assert!(
            (pos.longitude().as_degrees() - (-72.029_166)).abs() < 0.001,
            "lon={}",
            pos.longitude().as_degrees()
        );
        assert_eq!(pos.symbol().table_char(), '/');
        assert_eq!(pos.symbol().code_char(), '-');
        assert_eq!(pos.comment(), "Test comment");
        Ok(())
    }

    #[test]
    fn parse_uncompressed_position_rejects_non_ascii_comment_without_replacement() -> TestResult {
        let mut info = b"!4903.50N/07201.75W-comment".to_vec();
        *info
            .last_mut()
            .ok_or("static uncompressed-position fixture must contain a comment")? = 0xFF;

        assert_eq!(
            parse_aprs_position(&info),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::UncompressedPositionComment,
                index: 6,
                byte: 0xFF,
            }),
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_position_with_timestamp() -> TestResult {
        // '@' type with DHM timestamp "092345z"
        let info = b"@092345z4903.50N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(
            pos.timestamp(),
            Some(AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?),
        );
        assert!(
            (pos.latitude().as_degrees() - 49.058_333).abs() < 0.001,
            "lat check"
        );
        assert!(
            (pos.longitude().as_degrees() - (-72.029_166)).abs() < 0.001,
            "lon check"
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_position_without_messaging_retains_local_timestamp() -> TestResult {
        let pos = parse_aprs_position(b"/110000/4903.50N/07201.75W-")?;

        assert_eq!(
            pos.timestamp(),
            Some(AprsReportTimestamp::day_hour_minute_local(11, 0, 0)?),
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_position_south_east() -> TestResult {
        let info = b"!3356.65S/15113.72E>";
        let pos = parse_aprs_position(info)?;
        assert!(
            pos.latitude().as_degrees() < 0.0,
            "expected South, got lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            pos.longitude().as_degrees() > 0.0,
            "expected East, got lon={}",
            pos.longitude().as_degrees()
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_position_messaging_enabled() -> TestResult {
        let info = b"=4903.50N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.timestamp(), None);
        assert!(
            (pos.latitude().as_degrees() - 49.058_333).abs() < 0.001,
            "lat check"
        );
        Ok(())
    }

    #[test]
    fn parse_timestamped_positions_reject_malformed_timestamps() {
        let uncompressed = b"/092345X4903.50N/07201.75W-";
        let compressed = b"@09A345z/%Ztl'&XW> sT";

        assert!(
            matches!(
                parse_aprs_position(uncompressed),
                Err(AprsError::InvalidTimestamp(_))
            ),
            "uncompressed position accepted an unknown timestamp suffix",
        );
        assert!(
            matches!(
                parse_aprs_position(compressed),
                Err(AprsError::InvalidTimestamp(_))
            ),
            "compressed position accepted a non-decimal timestamp component",
        );
    }

    #[test]
    fn parse_timestamped_positions_reject_out_of_range_timestamps() {
        let uncompressed = b"/002345z4903.50N/07201.75W-";
        let compressed = b"@092445z/%Ztl'&XW> sT";

        assert_eq!(
            parse_aprs_position(uncompressed),
            Err(AprsError::InvalidTimestamp(
                crate::packet::TimestampError::Day
            )),
        );
        assert_eq!(
            parse_aprs_position(compressed),
            Err(AprsError::InvalidTimestamp(
                crate::packet::TimestampError::Hour
            )),
        );
    }

    #[test]
    fn parse_timestamped_positions_reject_non_ascii_timestamps() -> TestResult {
        let mut uncompressed = b"/092345z4903.50N/07201.75W-".to_vec();
        *uncompressed
            .get_mut(3)
            .ok_or("static uncompressed fixture must contain timestamp byte 3")? = 0xFF;
        let mut compressed = b"@092345z/%Ztl'&XW> sT".to_vec();
        *compressed
            .get_mut(6)
            .ok_or("static compressed fixture must contain timestamp byte 6")? = 0x80;

        assert_eq!(
            parse_aprs_position(&uncompressed),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::PositionTimestamp,
                index: 2,
                byte: 0xFF,
            }),
        );
        assert_eq!(
            parse_aprs_position(&compressed),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::PositionTimestamp,
                index: 5,
                byte: 0x80,
            }),
        );
        Ok(())
    }

    #[test]
    fn parse_position_rejects_out_of_range_latitude() {
        // Degrees 91 with minutes 60 is an impossible latitude.
        let r = parse_aprs_position(b"!9160.00N/07201.75W-");
        assert!(
            matches!(r, Err(AprsError::InvalidCoordinates)),
            "9160.00N must be rejected: {r:?}",
        );
        // Minutes 99.99: minutes must be < 60.
        let r = parse_aprs_position(b"!4999.99N/07201.75W-");
        assert!(
            matches!(r, Err(AprsError::InvalidCoordinates)),
            "latitude minutes 99.99 must be rejected: {r:?}",
        );
        // 90° plus nonzero minutes exceeds the pole (90.5°).
        let r = parse_aprs_position(b"!9030.00N/07201.75W-");
        assert!(
            matches!(r, Err(AprsError::InvalidCoordinates)),
            "9030.00N must be rejected: {r:?}",
        );
    }

    #[test]
    fn parse_position_rejects_out_of_range_longitude() {
        // Degrees 361 is an impossible longitude.
        let r = parse_aprs_position(b"!4903.50N/36101.75W-");
        assert!(
            matches!(r, Err(AprsError::InvalidCoordinates)),
            "36101.75W must be rejected: {r:?}",
        );
        // Minutes 90: minutes must be < 60.
        let r = parse_aprs_position(b"!4903.50N/07290.00W-");
        assert!(
            matches!(r, Err(AprsError::InvalidCoordinates)),
            "longitude minutes 90 must be rejected: {r:?}",
        );
        // 180° plus nonzero minutes exceeds the antimeridian (180.5°).
        let r = parse_aprs_position(b"!4903.50N/18030.00W-");
        assert!(
            matches!(r, Err(AprsError::InvalidCoordinates)),
            "18030.00W must be rejected: {r:?}",
        );
    }

    #[test]
    fn parse_position_accepts_polar_and_antimeridian_boundary() -> TestResult {
        // Exactly 90°00.00' / 180°00.00': the boundary itself is valid.
        let pos = parse_aprs_position(b"!9000.00N/18000.00E-")?;
        assert!(
            (pos.latitude().as_degrees() - 90.0).abs() < f64::EPSILON,
            "lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - 180.0).abs() < f64::EPSILON,
            "lon={}",
            pos.longitude().as_degrees()
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_position_invalid_type() {
        let info = b"X4903.50N/07201.75W-";
        assert!(
            parse_aprs_position(info).is_err(),
            "expected error for invalid type",
        );
    }

    #[test]
    fn parse_aprs_position_too_short() {
        assert!(
            parse_aprs_position(b"!short").is_err(),
            "expected error for short input",
        );
    }

    #[test]
    fn parse_aprs_position_empty() {
        assert!(
            parse_aprs_position(b"").is_err(),
            "expected error for empty"
        );
    }

    // ---- APRS compressed position tests ----

    #[test]
    fn parse_aprs_compressed_position() -> TestResult {
        // Use computed example with known values.
        // lat_val = 3493929 → lat = 90 - 3493929/380926 = 80.828
        //   bytes: ('%', 'Z', 't', 'l')
        // lon_val = 4567890 → lon = -180 + 4567890/190463 = -156.018
        //   bytes: (''', '&', 'X', 'W')
        let body: &[u8] = b"/%Ztl'&XW> sT";
        let mut info = vec![b'!'];
        info.extend_from_slice(body);

        let pos = parse_aprs_position(&info)?;
        assert_eq!(pos.timestamp(), None);
        assert!(
            (pos.latitude().as_degrees() - 80.828).abs() < 0.01,
            "lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - (-156.018)).abs() < 0.01,
            "lon={}",
            pos.longitude().as_degrees()
        );
        assert_eq!(pos.symbol().table_char(), '/');
        assert_eq!(pos.symbol().code_char(), '>');
        Ok(())
    }

    #[test]
    fn parse_compressed_position_rejects_non_ascii_comment_without_replacement() -> TestResult {
        let mut info = b"!/%Ztl'&XW> sTcomment".to_vec();
        *info
            .last_mut()
            .ok_or("static compressed-position fixture must contain a comment")? = 0xFF;

        assert_eq!(
            parse_aprs_position(&info),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::CompressedPositionComment,
                index: 6,
                byte: 0xFF,
            }),
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_compressed_with_timestamp() -> TestResult {
        let mut info = Vec::new();
        info.push(b'@');
        info.extend_from_slice(b"092345z"); // 7-char timestamp
        info.extend_from_slice(b"/%Ztl'&XW> sT"); // compressed body
        let pos = parse_aprs_position(&info)?;
        assert_eq!(
            pos.timestamp(),
            Some(AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?),
        );
        assert!(
            (pos.latitude().as_degrees() - 80.828).abs() < 0.01,
            "lat check"
        );
        Ok(())
    }

    #[test]
    fn parse_aprs_compressed_too_short() {
        let info = b"!/short";
        assert!(parse_aprs_position(info).is_err(), "too-short compressed");
    }

    #[test]
    fn base91_decode_zero() -> TestResult {
        assert_eq!(decode_base91_4(b"!!!!")?, 0);
        Ok(())
    }

    #[test]
    fn compressed_reference_vectors_preserve_rmc_altitude_range_and_no_data() -> TestResult {
        let rmc = parse_aprs_position(b"=/5L!!<*e7>7P[")?;
        assert_eq!(rmc.course().map(Course::as_degrees), Some(88));
        assert!(
            (rmc.speed().ok_or("RMC speed")?.as_knots() - (1.08_f64.powi(47) - 1.0)).abs() < 1e-12
        );
        let range = parse_aprs_position(b"=/5L!!<*e7>{?!")?;
        assert!(range.course().is_none());
        assert!(range.speed().is_none());
        let Some(CompressedPositionData::RadioRange(range)) = range.compressed_data() else {
            return Err("radio range missing".into());
        };
        assert_eq!(range.code(), 30);
        assert!(2.0_f64.mul_add(-1.08_f64.powi(30), range.miles()).abs() < 1e-12);
        let altitude = parse_aprs_position(b"=/5L!!<*e7OS]S")?;
        assert_eq!(altitude.extensions().altitude_feet(), Some(10_005));
        assert!(altitude.course().is_none());
        let no_data = parse_aprs_position(b"=/5L!!<*e7> sT")?;
        assert!(no_data.course().is_none());
        assert!(no_data.speed().is_none());
        assert_eq!(
            no_data.compressed_data(),
            Some(CompressedPositionData::None)
        );
        Ok(())
    }

    #[test]
    fn compressed_geography_and_tail_domains_are_checked() -> TestResult {
        let boundary = AprsPosition::from_bytes(b"=/{{!!{{!!>   ")?;
        assert!((boundary.latitude().as_degrees() + 90.0).abs() < f64::EPSILON);
        assert!((boundary.longitude().as_degrees() - 180.0).abs() < f64::EPSILON);
        assert!(AprsPosition::from_bytes(b"=/{{!\"{{!!>   ").is_err());
        assert!(AprsPosition::from_bytes(b"=/{{!!{{!\">   ").is_err());
        assert!(parse_aprs_position(b"=/{{{{<*e7>7P[").is_err());
        assert!(parse_aprs_position(b"=/5L!!{{{{>7P[").is_err());
        for tail in [b"~P[", b"7 [", b"7P~", b"7Pa"] {
            let mut packet = b"=/5L!!<*e7>".to_vec();
            packet.extend_from_slice(tail);
            assert!(
                parse_aprs_position(&packet).is_err(),
                "accepted invalid tail {tail:?}"
            );
        }
        let north_stationary = parse_aprs_position(b"=/5L!!<*e7>!!!")?;
        assert_eq!(north_stationary.course().map(Course::as_degrees), Some(0));
        assert_eq!(north_stationary.speed().map(Speed::as_knots), Some(0.0));
        Ok(())
    }

    #[test]
    fn all_overlay_selectors_are_validated_and_compressed_digits_are_normalized() -> TestResult {
        for byte in u8::MIN..=u8::MAX {
            let accepted = byte.is_ascii_digit() || byte.is_ascii_uppercase();
            assert_eq!(
                crate::SymbolOverlay::new(byte).is_ok(),
                accepted,
                "overlay byte {byte}"
            );
            if accepted {
                let table = SymbolTable::from_byte(byte)?;
                assert_eq!(
                    SymbolTable::from_compressed_byte(table.compressed_byte())?,
                    table
                );
                let mut packet = b"=/5L!!<*e7>   ".to_vec();
                *packet.get_mut(1).ok_or("table selector")? = table.compressed_byte();
                assert_eq!(
                    parse_aprs_position(&packet)?.symbol().table_char(),
                    char::from(byte)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn base91_decode_max() -> TestResult {
        // base-91 digits run 0..=90 (ASCII 33..=123, '!'..='{'). The true
        // maximum 4-digit value is all-90s, i.e. b"{{{{" (byte 123).
        let val = decode_base91_4(b"{{{{")?;
        let expected = 90_u32 * (753_571 + 8_281 + 91 + 1);
        assert_eq!(val, expected, "max base-91 value is 68_574_960");
        assert_eq!(val, 68_574_960);
        Ok(())
    }

    #[test]
    fn base91_decode_rejects_pipe_byte() {
        // ASCII '|' (124) is one past the legal base-91 range and would
        // encode digit 91 (one past the max 90), so it must be rejected, not
        // decoded to 68_574_961.
        assert!(
            decode_base91_4(b"||||").is_err(),
            "'|' (124) is above the valid base-91 range"
        );
        assert!(
            decode_base91_4(b"|!!!").is_err(),
            "leading '|' (124) is above the valid base-91 range"
        );
    }

    #[test]
    fn base91_decode_invalid_char() {
        assert!(
            decode_base91_4(b" !!!").is_err(),
            "space is below valid range"
        );
    }

    #[test]
    fn parse_position_with_one_digit_ambiguity() -> TestResult {
        let info = b"!4903.5 N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity(), PositionAmbiguity::OneDigit);
        assert!(
            (pos.latitude().as_degrees() - 49.0583).abs() < 0.001,
            "lat check"
        );
        assert!(
            (pos.longitude().as_degrees() + 72.0 + 1.70 / 60.0).abs() < 1e-12,
            "latitude ambiguity also masks longitude"
        );
        Ok(())
    }

    #[test]
    fn parse_position_with_two_digit_ambiguity() -> TestResult {
        let info = b"!4903.  N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity(), PositionAmbiguity::TwoDigits);
        assert!((pos.longitude().as_degrees() + 72.0 + 1.0 / 60.0).abs() < 1e-12);
        Ok(())
    }

    #[test]
    fn parse_position_with_four_digit_ambiguity() -> TestResult {
        let info = b"!49  .  N/072  .  W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity(), PositionAmbiguity::FourDigits);
        assert!((pos.longitude().as_degrees() + 72.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn parse_position_full_precision_has_no_ambiguity() -> TestResult {
        let info = b"!4903.50N/07201.75W-";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.ambiguity(), PositionAmbiguity::None);
        Ok(())
    }

    #[test]
    fn parse_position_populates_extensions_from_comment() -> TestResult {
        let info = b"!3515.00N/09745.00W>088/036/A=001234hello";
        let pos = parse_aprs_position(info)?;
        let motion = pos
            .extensions()
            .course_speed()
            .ok_or("course/speed extension")?;
        assert_eq!(motion.course().as_degrees(), 88);
        assert!((motion.speed().as_knots() - 36.0).abs() < f64::EPSILON);
        assert_eq!(pos.extensions().altitude_feet(), Some(1234));
        assert_eq!(pos.speed().map(Speed::as_knots), Some(36.0));
        assert_eq!(pos.course().map(Course::as_degrees), Some(88));
        Ok(())
    }

    #[test]
    fn parse_position_embedded_weather() -> TestResult {
        let info = b"!3515.00N/09745.00W_090/010g015t072r001P020h55b10135";
        let pos = parse_aprs_position(info)?;
        assert_eq!(pos.symbol().code_char(), '_');
        let wx = pos.weather().ok_or("embedded weather missing")?;
        assert_eq!(wx.wind_direction().map(WindDirection::degrees), Some(90));
        assert_eq!(wx.wind_speed().map(ThreeDigitWeatherValue::value), Some(10),);
        assert_eq!(wx.wind_gust().map(ThreeDigitWeatherValue::value), Some(15),);
        assert_eq!(wx.temperature().map(Fahrenheit::get), Some(72));
        assert_eq!(wx.rain_1h().map(ThreeDigitWeatherValue::value), Some(1),);
        assert_eq!(
            wx.rain_since_midnight().map(ThreeDigitWeatherValue::value),
            Some(20),
        );
        assert_eq!(wx.humidity().map(Humidity::percent), Some(55));
        assert_eq!(
            wx.pressure().map(BarometricPressure::tenths_hpa),
            Some(10_135),
        );
        Ok(())
    }

    #[test]
    fn parse_position_without_weather_symbol_has_no_weather() -> TestResult {
        let info = b"!3515.00N/09745.00W>mobile comment";
        let pos = parse_aprs_position(info)?;
        assert!(pos.weather().is_none(), "no weather expected");
        Ok(())
    }

    #[test]
    fn parse_position_weather_symbol_bad_format_has_no_weather() -> TestResult {
        let info = b"!3515.00N/09745.00W_hello";
        let pos = parse_aprs_position(info)?;
        assert!(pos.weather().is_none(), "bad format → no weather");
        Ok(())
    }

    #[test]
    fn immutable_position_updates_keep_derived_views_consistent() -> TestResult {
        let position = AprsPosition::from_bytes(b"=/5L!!<*e7>7P[")?;
        let changed =
            position.with_motion(PositionMotion::new(Some(Speed::from_knots(1.5)?), None));
        assert_eq!(changed.compressed_data(), None);
        assert_eq!(changed.speed().map(Speed::as_knots), Some(1.5));
        let weather = AprsPosition::new(
            Latitude::new(0.0)?,
            Longitude::new(0.0)?,
            AprsSymbol::from_chars('/', '_')?,
        )
        .with_comment("090/010g015t072")?;
        assert!(weather.weather().is_some());
        let weather = weather.with_comment("plain text")?;
        assert!(weather.weather().is_none());
        assert!(weather.extensions().course_speed().is_none());
        Ok(())
    }

    #[test]
    fn coordinate_syntax_and_ambiguity_are_consistent() -> TestResult {
        for body in [
            b"+903.50N/07201.75W-".as_slice(),
            b"4903.50N/+7201.75W-",
            b"490 .50N/07201.75W-",
            b"4903.5 N/07201.  W-",
        ] {
            assert!(
                AprsPosition::from_uncompressed_body(body).is_err(),
                "accepted {body:?}"
            );
        }
        let position = AprsPosition::from_bytes(b"!490 .  N/07201.75W-")?;
        assert_eq!(position.ambiguity(), PositionAmbiguity::ThreeDigits);
        assert!((position.longitude().as_degrees() + 72.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn compressed_domains_include_their_full_wire_ranges() -> TestResult {
        assert_eq!(CompressedRange::from_code(90)?.code(), 90);
        assert!(CompressedRange::from_code(91).is_err());
        let altitude = CompressedAltitude::from_codes(90, 90)?;
        assert_eq!(altitude.exponent(), 8280);
        assert!(CompressedAltitude::from_exponent(8281).is_err());
        assert!(altitude.feet().is_finite());
        assert!(CompressedAltitude::from_codes(91, 0).is_err());
        assert!(CompressedAltitude::from_codes(0, 91).is_err());
        assert_eq!(
            decode_compressed_tail(b' ', 0, 255)?,
            CompressedPositionData::None
        );
        Ok(())
    }
}
