//! Mic-E compressed position encoding (APRS 1.0.1 ch. 10).
//!
//! Mic-E splits the position across two fields: latitude is encoded in the
//! 6-character AX.25 destination address, and longitude + speed/course
//! are in the info field body.
//!
//! See [packet construction and parsing](crate#packet-construction-and-parsing).

use crate::error::{AprsError, AprsWireTextField};
use crate::packet::{AprsData, PositionAmbiguity};
use crate::position::{AprsPosition, PositionMotion};
use crate::text::decode_wire_ascii;
use crate::units::{AprsSymbol, Course, Latitude, Longitude, MiceSpeed, Speed};

/// Width or byte failure in an optional Mic-E telemetry block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiceTelemetryError {
    /// The two-channel hexadecimal payload is not four bytes.
    HexadecimalTwoLength {
        /// Supplied payload bytes.
        actual: usize,
    },
    /// The five-channel hexadecimal payload is not ten bytes.
    HexadecimalFiveLength {
        /// Supplied payload bytes.
        actual: usize,
    },
    /// The five-channel binary payload is not five bytes.
    BinaryFiveLength {
        /// Supplied payload bytes.
        actual: usize,
    },
    /// A two-digit hexadecimal channel value is truncated.
    TruncatedHex,
    /// A channel value contains a nonhexadecimal octet.
    NonHexadecimal {
        /// Rejected octet.
        byte: u8,
    },
}

impl std::fmt::Display for MiceTelemetryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HexadecimalTwoLength { actual } => write!(
                formatter,
                "two-channel hexadecimal telemetry requires 4 digits; received {actual}"
            ),
            Self::HexadecimalFiveLength { actual } => write!(
                formatter,
                "five-channel hexadecimal telemetry requires 10 digits; received {actual}"
            ),
            Self::BinaryFiveLength { actual } => write!(
                formatter,
                "five-channel binary telemetry requires 5 bytes; received {actual}"
            ),
            Self::TruncatedHex => formatter.write_str("truncated hexadecimal value"),
            Self::NonHexadecimal { byte } => write!(
                formatter,
                "telemetry channel contains nonhexadecimal byte {byte:#04X}"
            ),
        }
    }
}
impl std::error::Error for MiceTelemetryError {}

/// Mic-E standard message code.
///
/// APRS 1.0.1 pp.44-45 defines seven standard messages and Emergency.
/// Destination characters from the custom alphabet are retained as an
/// unknown message (`None`) by [`AprsPosition::mice_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MiceMessage {
    /// Standard message bits 111: "Off Duty".
    OffDuty,
    /// Standard message bits 110: "En Route".
    EnRoute,
    /// Standard message bits 101: "In Service".
    InService,
    /// Standard message bits 100: "Returning".
    Returning,
    /// Standard message bits 011: "Committed".
    Committed,
    /// Standard message bits 010: "Special".
    Special,
    /// Standard message bits 001: "Priority".
    Priority,
    /// Standard message bits 000: "Emergency".
    Emergency,
}

/// Optional telemetry carried after the nine mandatory Mic-E bytes.
///
/// APRS 1.0.1 chapter 10 defines three distinct wire encodings. Keeping the
/// encoding in the variant preserves whether five values arrived as printable
/// hexadecimal or as the legacy binary form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MiceTelemetry {
    /// A grave-accent flag followed by hexadecimal channels 1 and 3.
    HexadecimalTwo {
        /// Telemetry channel 1.
        channel_1: u8,
        /// Telemetry channel 3.
        channel_3: u8,
    },
    /// An apostrophe flag followed by five hexadecimal channel values.
    HexadecimalFive([u8; 5]),
    /// A `0x1D` flag followed by five opaque 8-bit channel values.
    BinaryFive([u8; 5]),
}

/// Parse a Mic-E encoded APRS position (APRS101.PDF Chapter 10).
///
/// Mic-E splits the position across two fields:
/// - **Latitude** is encoded in the 6-character AX.25 destination address
/// - **Longitude** and speed/course are in the info field body
///
/// Data type identifiers: `` ` `` (0x60, current Mic-E) or `'` (0x27, old Mic-E).
/// The legacy `0x1C` and `0x1D` identifiers are also accepted.
///
/// # Parameters
///
/// - `destination`: The AX.25 destination callsign (e.g., "T4SP0R")
/// - `info`: The full AX.25 information field (including the type byte)
///
/// # Errors
///
/// Returns [`AprsError`] if the Mic-E encoding is invalid.
fn parse_mice_position(destination: &str, info: &[u8]) -> Result<AprsPosition, AprsError> {
    let header = info.get(..9).ok_or(AprsError::InvalidFormat)?;
    let dest = destination.as_bytes();
    let dest_head: &[u8; 6] = dest.try_into().map_err(|_| AprsError::InvalidFormat)?;

    let data_type = *header.first().ok_or(AprsError::InvalidFormat)?;
    if data_type != b'`' && data_type != b'\'' && data_type != 0x1C && data_type != 0x1D {
        return Err(AprsError::InvalidFormat);
    }

    // Longitude degree/minute/hundredth byte ranges (APRS101,pp47-48).
    let lon_bytes = header.get(1..4).ok_or(AprsError::InvalidFormat)?;
    if !matches!(lon_bytes, [38..=127, 38..=97, 28..=127]) {
        return Err(AprsError::InvalidCoordinates);
    }

    // --- Latitude from destination address ---
    // Each of the 6 destination chars encodes a latitude digit plus
    // N/S and longitude offset flags. Chars 0-9 and A-L map to digits.
    let mut lat_digits = [0u8; 6];
    let mut north = true;
    let mut lon_offset = 0i16;
    let mut ambiguity_start = None;

    // Per APRS 1.0.1 §10 destination table (document p.44), chars 3-5
    // carry hemisphere/offset flags. `mice_dest_digit` returns the
    // North/+100/West indicator (true only for the std-set chars P-Z).
    for (i, &ch) in dest_head.iter().enumerate() {
        if i >= 3 && (b'A'..=b'K').contains(&ch) {
            return Err(AprsError::InvalidCoordinates);
        }
        let masked = matches!(ch, b'K' | b'L' | b'Z');
        if masked {
            if i < 2 {
                return Err(AprsError::InvalidCoordinates);
            }
            if ambiguity_start.is_none() {
                ambiguity_start = Some(i);
            }
        } else if ambiguity_start.is_some() {
            return Err(AprsError::InvalidCoordinates);
        }
        let (digit, north_offset_west) = mice_dest_digit(ch)?;
        if let Some(slot) = lat_digits.get_mut(i) {
            *slot = digit;
        }

        // Char 3: N/S flag. P-Z = North, otherwise (0-9, L) = South.
        if i == 3 {
            north = north_offset_west;
        }
        // Char 4: longitude offset. P-Z = +100 degrees, otherwise +0.
        if i == 4 && north_offset_west {
            lon_offset = 100;
        }
        // Char 5: W/E flag. P-Z = West (negate longitude), otherwise East.
    }

    let d0 = f64::from(*lat_digits.first().ok_or(AprsError::InvalidCoordinates)?);
    let d1 = f64::from(*lat_digits.get(1).ok_or(AprsError::InvalidCoordinates)?);
    let d2 = f64::from(*lat_digits.get(2).ok_or(AprsError::InvalidCoordinates)?);
    let d3 = f64::from(*lat_digits.get(3).ok_or(AprsError::InvalidCoordinates)?);
    let d4 = f64::from(*lat_digits.get(4).ok_or(AprsError::InvalidCoordinates)?);
    let d5 = f64::from(*lat_digits.get(5).ok_or(AprsError::InvalidCoordinates)?);
    let lat_deg = d0.mul_add(10.0, d1);
    let lat_min = d2.mul_add(10.0, d3) + d4 / 10.0 + d5 / 100.0;
    if lat_min >= 60.0 {
        return Err(AprsError::InvalidCoordinates);
    }
    let mut latitude = lat_deg + lat_min / 60.0;
    if !north {
        latitude = -latitude;
    }
    let latitude = Latitude::new(latitude).map_err(|_| AprsError::InvalidCoordinates)?;
    let ambiguity = match ambiguity_start {
        None => PositionAmbiguity::None,
        Some(5) => PositionAmbiguity::OneDigit,
        Some(4) => PositionAmbiguity::TwoDigits,
        Some(3) => PositionAmbiguity::ThreeDigits,
        Some(2) => PositionAmbiguity::FourDigits,
        _ => return Err(AprsError::InvalidCoordinates),
    };

    // --- Longitude from info field ---
    // info[1] = degrees (d+28), info[2] = minutes (m+28), info[3] = hundredths (h+28)
    let d_byte = *lon_bytes.first().ok_or(AprsError::InvalidCoordinates)?;
    let m_byte = *lon_bytes.get(1).ok_or(AprsError::InvalidCoordinates)?;
    let h_byte = *lon_bytes.get(2).ok_or(AprsError::InvalidCoordinates)?;
    let d = i16::from(d_byte) - 28;
    let m = i16::from(m_byte) - 28;
    let h = i16::from(h_byte) - 28;

    let mut lon_deg = d + lon_offset;
    if (180..=189).contains(&lon_deg) {
        lon_deg -= 80;
    } else if (190..=199).contains(&lon_deg) {
        lon_deg -= 190;
    }

    let lon_min = if m >= 60 { m - 60 } else { m };
    let minute_hundredths =
        u16::try_from(lon_min * 100 + h).map_err(|_| AprsError::InvalidCoordinates)?;
    let longitude_abs = f64::from(lon_deg)
        + f64::from(ambiguity.mask_minute_hundredths(minute_hundredths)) / 6_000.0;

    // The final destination indicator selects west versus east.
    let dest5 = *dest_head.get(5).ok_or(AprsError::InvalidCoordinates)?;
    let west = mice_indicator_set(dest5);
    let longitude = Longitude::new(if west { -longitude_abs } else { longitude_abs })
        .map_err(|_| AprsError::InvalidCoordinates)?;

    let motion = decode_mice_speed_course(header)?;

    // Symbol: info[7] = symbol code, info[8] = symbol table
    let symbol_code = char::from(*header.get(7).ok_or(AprsError::InvalidFormat)?);
    let symbol_table = char::from(*header.get(8).ok_or(AprsError::InvalidFormat)?);
    let symbol = AprsSymbol::from_chars(symbol_table, symbol_code)?;

    let optional_bytes = info.get(9..).ok_or(AprsError::InvalidFormat)?;
    let (comment, mice_telemetry) = decode_mice_optional_data(optional_bytes)?;

    // Decode the Mic-E standard message code from destination chars 0-2
    // (APRS 1.0.1 §10.1 Table 10). `None` if any char is in the custom range.
    let [message_a, message_b, message_c, ..] = *dest_head;
    let mice_message = mice_decode_message([message_a, message_b, message_c]);

    // Look for optional altitude in the comment (`<ccc>}` base-91, metres
    // offset from -10000) per APRS 1.0.1 §10.1.1.
    let mice_altitude_m = mice_decode_altitude(&comment);

    Ok(AprsPosition::new(latitude, longitude, symbol)
        .with_motion(motion)
        .with_comment(&comment)?
        .with_mice(mice_message, mice_altitude_m, mice_telemetry, ambiguity))
}

impl AprsPosition {
    /// Decode a Mic-E information field with its exact six-byte destination.
    ///
    /// Ambiguity markers form a contiguous suffix of the four minute digits;
    /// the same precision masks longitude. Returned coordinates retain the
    /// lower magnitude bound of the declared cell (APRS101,pp53-54).
    ///
    /// # Errors
    ///
    /// Returns [`AprsError`] for malformed destination positions, coordinate
    /// bytes/ranges, symbol, motion, optional telemetry or non-ASCII status text.
    pub fn from_mice_bytes(destination: &str, info: &[u8]) -> Result<Self, AprsError> {
        parse_mice_position(destination, info)
    }
}

/// Decode the optional field following a Mic-E symbol-table byte.
///
/// Telemetry is classified before text decoding because the legacy `0x1D`
/// form intentionally contains arbitrary bytes at or above `0x80`.
fn decode_mice_optional_data(bytes: &[u8]) -> Result<(String, Option<MiceTelemetry>), AprsError> {
    let Some(&flag) = bytes.first() else {
        return Ok((String::new(), None));
    };

    let telemetry = match flag {
        b'`' => {
            let payload = bytes.get(1..).unwrap_or(&[]);
            if payload.len() != 4 {
                return Err(AprsError::InvalidMiceTelemetry(
                    MiceTelemetryError::HexadecimalTwoLength {
                        actual: payload.len(),
                    },
                ));
            }
            MiceTelemetry::HexadecimalTwo {
                channel_1: decode_hex_byte(payload, 0)?,
                channel_3: decode_hex_byte(payload, 2)?,
            }
        }
        b'\'' => {
            let payload = bytes.get(1..).unwrap_or(&[]);
            if payload.len() != 10 {
                return Err(AprsError::InvalidMiceTelemetry(
                    MiceTelemetryError::HexadecimalFiveLength {
                        actual: payload.len(),
                    },
                ));
            }
            let mut channels = [0_u8; 5];
            for (index, channel) in channels.iter_mut().enumerate() {
                *channel = decode_hex_byte(payload, index * 2)?;
            }
            MiceTelemetry::HexadecimalFive(channels)
        }
        0x1D => {
            let raw = bytes.get(1..).unwrap_or(&[]);
            let payload: [u8; 5] = raw.try_into().map_err(|_| {
                AprsError::InvalidMiceTelemetry(MiceTelemetryError::BinaryFiveLength {
                    actual: raw.len(),
                })
            })?;
            MiceTelemetry::BinaryFive(payload)
        }
        _ => {
            let comment = decode_wire_ascii(AprsWireTextField::MiceComment, bytes)?.to_owned();
            return Ok((comment, None));
        }
    };

    Ok((String::new(), Some(telemetry)))
}

fn decode_hex_byte(bytes: &[u8], offset: usize) -> Result<u8, AprsError> {
    let high = *bytes.get(offset).ok_or(AprsError::InvalidMiceTelemetry(
        MiceTelemetryError::TruncatedHex,
    ))?;
    let low = *bytes
        .get(offset + 1)
        .ok_or(AprsError::InvalidMiceTelemetry(
            MiceTelemetryError::TruncatedHex,
        ))?;
    let high = hex_nibble(high).ok_or(AprsError::InvalidMiceTelemetry(
        MiceTelemetryError::NonHexadecimal { byte: high },
    ))?;
    let low = hex_nibble(low).ok_or(AprsError::InvalidMiceTelemetry(
        MiceTelemetryError::NonHexadecimal { byte: low },
    ))?;
    Ok((high << 4) | low)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Decode Mic-E speed and course from the 3-byte SP/DC/SE block
/// `info[4..=6]`, per APRS 1.0.1 §10.
///
/// The decoder first computes the raw values as if both fields were
/// unmodulated:
///
/// ```text
///   speed_raw  = (SP-28) × 10 + (DC-28) / 10        (knots, 0..999)
///   course_raw = ((DC-28) mod 10) × 100 + (SE-28)   (degrees, 0..999)
/// ```
///
/// §10 p.52 then specifies two **alt-encoding adjustments**:
///
/// - if `speed_raw  >= 800` → `speed  = speed_raw  - 800`
/// - if `course_raw >= 400` → `course = course_raw - 400`
///
/// The worked example on p.53 (`` `(_fn"Oj/ ``) returns 20 knots and
/// 251 degrees. Each input byte must be in `28..=127`; after folding,
/// speed must fit `0..=799` knots and course must fit `0..=360` degrees.
/// Course zero represents unknown and returns `None`. Other invalid
/// values return [`AprsError`].
fn decode_mice_speed_course(header: &[u8]) -> Result<PositionMotion, AprsError> {
    let (Some(&speed_high), Some(&speed_course), Some(&course_low)) =
        (header.get(4), header.get(5), header.get(6))
    else {
        return Err(AprsError::InvalidFormat);
    };
    if !(28..=127).contains(&speed_high)
        || !(28..=127).contains(&speed_course)
        || !(28..=127).contains(&course_low)
    {
        return Err(AprsError::InvalidFormat);
    }
    let sp = u16::from(speed_high - 28);
    let dc = u16::from(speed_course - 28);
    let se = u16::from(course_low - 28);
    let mut speed_raw = sp * 10 + dc / 10;
    let mut course_raw = (dc % 10) * 100 + se;
    // Alt-encoding fold per §10 p.52.
    if speed_raw >= 800 {
        speed_raw -= 800;
    }
    if course_raw >= 400 {
        course_raw -= 400;
    }
    let _representable = MiceSpeed::new(speed_raw)?;
    let course = if course_raw == 0 {
        None
    } else {
        Some(Course::new(course_raw)?)
    };
    Ok(PositionMotion::new(
        Some(Speed::from_knots(f64::from(speed_raw))?),
        course,
    ))
}

/// Extract a latitude digit (0-9, or 0 for a space) from a Mic-E
/// destination character, plus the North/+100/West indicator flag.
///
/// Returns `(digit, north_west_offset)`. The boolean is the bytes-4-6
/// indicator from the APRS 1.0.1 §10 destination table (document p.44):
/// it is `true` only for the standard-set characters that decode N/S =
/// North, Long Offset = +100, and W/E = West, namely `P`-`Z`. Digits
/// `0`-`9` and the ambiguity space `L` decode South / +0 / East, so the
/// flag is `false` for them. The custom-message range `A`-`K` is never
/// used in bytes 4-6 (the spec's "ASCII characters A-K are not used in
/// address bytes 4-6"), so its flag is `false` as well; those characters
/// still carry a latitude digit (A=0..J=9, K=space) for bytes 1-3.
const fn mice_dest_digit(ch: u8) -> Result<(u8, bool), AprsError> {
    match ch {
        b'0'..=b'9' => Ok((ch - b'0', false)),
        b'A'..=b'J' => Ok((ch - b'A', false)), // A=0..J=9; custom msg, not used in bytes 4-6
        // K = space digit, custom msg (not used in bytes 4-6); L = space
        // digit, Std (South/+0/East per doc p.44). Both clear the flag.
        b'K' | b'L' => Ok((0, false)),
        b'P'..=b'Y' => Ok((ch - b'P', true)), // P=0..Y=9; Std, North/+100/West
        b'Z' => Ok((0, true)),                // space digit; Std, North/+100/West
        _ => Err(AprsError::InvalidCoordinates),
    }
}

/// Check whether a Mic-E destination character carries the
/// North / +100-longitude-offset / West indicator.
///
/// Used by destination chars 3-5 for N/S, +100 lon offset, and W/E flag
/// decoding. Per the APRS 1.0.1 §10 destination table (document p.44),
/// only the standard-set characters `P`-`Z` decode North/+100/West;
/// digits `0`-`9` and the ambiguity space `L` decode South/+0/East. The
/// custom-message range `A`-`K` is not used in bytes 4-6.
const fn mice_indicator_set(ch: u8) -> bool {
    matches!(ch, b'P'..=b'Z')
}

/// Mic-E message-bit classification for destination chars 0-2.
///
/// Per APRS 1.0.1 §10.1, each of the first three destination characters
/// contributes one bit (A, B, or C) to a 3-bit message code via three
/// categories:
///
/// - `Std0`: character is `0`-`9` or `L`, contributes bit `0`
/// - `Std1`: character is `P`-`Y` or `Z`, contributes bit `1`
/// - `Custom`: character is `A`-`K`, marks the entire message as custom
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiceMsgClass {
    Std0,
    Std1,
    Custom,
}

const fn mice_msg_class(ch: u8) -> Option<MiceMsgClass> {
    match ch {
        b'0'..=b'9' | b'L' => Some(MiceMsgClass::Std0),
        b'P'..=b'Y' | b'Z' => Some(MiceMsgClass::Std1),
        b'A'..=b'K' => Some(MiceMsgClass::Custom),
        _ => None,
    }
}

/// Decode the 3-bit Mic-E message code from destination chars 0-2.
///
/// Returns `None` if any of the three chars is in the Custom range
/// (`A`-`K`); those encode user-defined messages the library does not
/// currently interpret. Returns `Some(MiceMessage)` for the 8 standard
/// codes (APRS 1.0.1 Table 10).
fn mice_decode_message(chars: [u8; 3]) -> Option<MiceMessage> {
    let c0 = mice_msg_class(*chars.first()?)?;
    let c1 = mice_msg_class(*chars.get(1)?)?;
    let c2 = mice_msg_class(*chars.get(2)?)?;
    if matches!(
        (c0, c1, c2),
        (MiceMsgClass::Custom, _, _) | (_, MiceMsgClass::Custom, _) | (_, _, MiceMsgClass::Custom)
    ) {
        return None;
    }
    let bit = |c| u8::from(matches!(c, MiceMsgClass::Std1));
    let idx = (bit(c0) << 2) | (bit(c1) << 1) | bit(c2);
    Some(match idx {
        0b111 => MiceMessage::OffDuty,
        0b110 => MiceMessage::EnRoute,
        0b101 => MiceMessage::InService,
        0b100 => MiceMessage::Returning,
        0b011 => MiceMessage::Committed,
        0b010 => MiceMessage::Special,
        0b001 => MiceMessage::Priority,
        _ => MiceMessage::Emergency, // 0b000
    })
}

/// Map a Mic-E standard message code to its 3-bit `(A, B, C)` encoding.
///
/// Per APRS 1.0.1 §10.1 Table 10. Returns `(bit_a, bit_b, bit_c)` where
/// `true` means "standard-1" (uppercase P-Y in the destination char).
///
/// Used by the Mic-E report builder [`crate::MiceReport`].
#[must_use]
pub(crate) const fn mice_message_bits(msg: MiceMessage) -> (bool, bool, bool) {
    match msg {
        MiceMessage::OffDuty => (true, true, true),      // 111
        MiceMessage::EnRoute => (true, true, false),     // 110
        MiceMessage::InService => (true, false, true),   // 101
        MiceMessage::Returning => (true, false, false),  // 100
        MiceMessage::Committed => (false, true, true),   // 011
        MiceMessage::Special => (false, true, false),    // 010
        MiceMessage::Priority => (false, false, true),   // 001
        MiceMessage::Emergency => (false, false, false), // 000
    }
}

/// Decode Mic-E altitude from the comment field.
///
/// Per APRS 1.0.1 §10.1.1, altitude is optionally encoded as three
/// base-91 characters (digits 0-90, ASCII `33..=123`, i.e. `'!'` to
/// `'{'`; value = byte - 33) followed by a literal `}`. The decoded
/// value is metres, offset from -10000 (so the wire value 10000 = sea
/// level).
///
/// Searches the comment for the first occurrence of the `ccc}` pattern
/// where each `c` is a valid base-91 printable character.
fn mice_decode_altitude(comment: &str) -> Option<i32> {
    let bytes = comment.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    for i in 0..=bytes.len() - 4 {
        let window = bytes.get(i..i + 4)?;
        if window.get(3) != Some(&b'}') {
            continue;
        }
        let b0 = *window.first()?;
        let b1 = *window.get(1)?;
        let b2 = *window.get(2)?;
        // base-91 digits 0..=90 encode to ASCII 33..=123 ('!'..='{');
        // bytes above '{' are not valid base-91 and must not be accepted.
        if !(33..=123).contains(&b0) || !(33..=123).contains(&b1) || !(33..=123).contains(&b2) {
            continue;
        }
        let val = i32::from(b0 - 33) * 91 * 91 + i32::from(b1 - 33) * 91 + i32::from(b2 - 33);
        return Some(val - 10_000);
    }
    None
}

/// Parse any APRS data frame, including Mic-E types that require the
/// AX.25 destination address.
///
/// This is the recommended entry point when the full AX.25 packet is
/// available. For Mic-E data type identifiers (`` ` ``, `'`, `0x1C`,
/// `0x1D`), the destination callsign is used to decode the latitude
/// via [`AprsPosition::from_mice_bytes`]. All other types delegate to
/// [`AprsData::from_bytes`].
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or data is invalid.
fn parse_aprs_data_full(info: &[u8], destination: &str) -> Result<AprsData, AprsError> {
    let first = *info.first().ok_or(AprsError::InvalidFormat)?;

    match first {
        // Mic-E current/old data types
        b'`' | b'\'' | 0x1C | 0x1D => {
            parse_mice_position(destination, info).map(AprsData::Position)
        }
        _ => AprsData::from_bytes(info),
    }
}

impl AprsData {
    /// Parse an information field, using the six destination bytes for Mic-E only.
    ///
    /// # Errors
    ///
    /// Propagates [`AprsPosition::from_mice_bytes`] failures for Mic-E identifiers;
    /// all other fields use [`Self::from_bytes`] and ignore the destination.
    pub fn from_bytes_with_destination(info: &[u8], destination: &str) -> Result<Self, AprsError> {
        parse_aprs_data_full(info, destination)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- Mic-E position tests ----

    #[test]
    fn parse_mice_basic() -> TestResult {
        // Destination "SUQU5P" → digits 3,5,1,5,5,0 (Off Duty)
        let dest = "SUQU5P";
        let info: &[u8] = &[
            0x60, // Mic-E current data type
            125,  // longitude degrees + 28 = 97+28
            73,   // longitude minutes + 28 = 45+28
            58,   // longitude hundredths + 28 = 30+28
            40,   // speed/course byte 1
            40,   // speed/course byte 2
            40,   // speed/course byte 3
            b'>', // symbol code
            b'/', // symbol table
        ];

        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.timestamp(), None);
        assert!(
            (pos.latitude().as_degrees() - 35.258).abs() < 0.01,
            "lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - (-97.755)).abs() < 0.01,
            "lon={}",
            pos.longitude().as_degrees()
        );
        assert_eq!(pos.symbol().code_char(), '>');
        assert_eq!(pos.symbol().table_char(), '/');
        assert_eq!(pos.speed().map(Speed::as_knots), Some(121.0));
        assert_eq!(pos.course().map(Course::as_degrees), Some(212));
        Ok(())
    }

    #[test]
    fn parse_mice_rejects_non_ascii_comment_without_replacement() {
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 40, b'>', b'/', b'o', b'k', 0xFF];

        assert_eq!(
            parse_mice_position("SUQU5P", info),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::MiceComment,
                index: 2,
                byte: 0xFF,
            }),
        );
    }

    #[test]
    fn parse_mice_accepts_all_binary_telemetry_octets() -> TestResult {
        let info: &[u8] = &[
            0x60, 125, 73, 58, 40, 40, 40, b'>', b'/', 0x1D, 0x80, 0x00, 0xFF, 0x7F, 0x01,
        ];

        let position = parse_mice_position("SUQU5P", info)?;
        assert_eq!(
            position.mice_telemetry(),
            Some(MiceTelemetry::BinaryFive([0x80, 0x00, 0xFF, 0x7F, 0x01]))
        );
        assert_eq!(position.comment(), "");
        assert_eq!(position.mice_altitude_m(), None);
        Ok(())
    }

    #[test]
    fn parse_mice_decodes_printable_telemetry_forms() -> TestResult {
        let mandatory = [0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];

        let mut two = mandatory.to_vec();
        two.extend_from_slice(b"`00fF");
        assert_eq!(
            parse_mice_position("SUQU5P", &two)?.mice_telemetry(),
            Some(MiceTelemetry::HexadecimalTwo {
                channel_1: 0,
                channel_3: 255,
            })
        );

        let mut five = mandatory.to_vec();
        five.extend_from_slice(b"'00017f80FF");
        assert_eq!(
            parse_mice_position("SUQU5P", &five)?.mice_telemetry(),
            Some(MiceTelemetry::HexadecimalFive([0, 1, 127, 128, 255]))
        );
        Ok(())
    }

    #[test]
    fn parse_mice_rejects_malformed_telemetry_without_reclassifying_it_as_text() {
        let mut info = vec![0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        info.extend_from_slice(&[0x1D, 1, 2, 3, 4]);
        assert!(matches!(
            parse_mice_position("SUQU5P", &info),
            Err(AprsError::InvalidMiceTelemetry(_))
        ));

        info.truncate(9);
        info.extend_from_slice(b"`0X00");
        assert!(matches!(
            parse_mice_position("SUQU5P", &info),
            Err(AprsError::InvalidMiceTelemetry(_))
        ));
    }

    #[test]
    fn mice_dest_char_l_is_standard_not_custom() -> TestResult {
        // APRS 1.0.1 §10 destination table (document p.44): 'L' is the
        // standard-set ambiguity/space code: N/S = South, Long Offset =
        // +0, W/E = East.
        assert!(!mice_indicator_set(b'L'), "L must decode East/South/+0");
        assert_eq!(mice_dest_digit(b'L')?, (0, false), "L: space digit, Std");
        // Spot-check the rest of the table's bytes-4-6 classification.
        assert!(!mice_indicator_set(b'0'), "0-9 decode East/South/+0");
        assert!(!mice_indicator_set(b'9'), "0-9 decode East/South/+0");
        assert!(mice_indicator_set(b'P'), "P-Z decode West/North/+100");
        assert!(mice_indicator_set(b'Z'), "P-Z decode West/North/+100");
        assert_eq!(mice_dest_digit(b'Z')?, (0, true), "Z: space digit, Std-1");
        Ok(())
    }

    #[test]
    fn parse_mice_l_decodes_east_hemisphere() -> TestResult {
        // Same frame as `parse_mice_basic` but destination char 5 is 'L'
        // instead of 'P'. Per document p.44, 'L' = East, so the longitude
        // must come out positive (East); the magnitude matches the West
        // 'P' case from `parse_mice_basic` (~ -97.755) with flipped sign.
        let dest = "SUQU5L";
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert!(
            pos.longitude().as_degrees() > 0.0,
            "expected East (positive) for 'L', got lon={}",
            pos.longitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - 97.755).abs() < 0.01,
            "lon={}",
            pos.longitude().as_degrees()
        );
        Ok(())
    }

    #[test]
    fn parse_mice_l_decodes_south_and_east() -> TestResult {
        // Three trailing ambiguity digits select South, +0 and East.
        let dest = "350LLL";
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert!(
            pos.latitude().as_degrees() < 0.0,
            "expected South (negative) for 'L' at char 3, got lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            pos.longitude().as_degrees() > 0.0,
            "expected East (positive) for 'L' at char 5, got lon={}",
            pos.longitude().as_degrees()
        );
        Ok(())
    }

    #[test]
    fn parse_mice_invalid_type() {
        assert!(
            parse_mice_position("SUQU5P", b"!test data").is_err(),
            "non-mic-e type"
        );
    }

    #[test]
    fn parse_mice_too_short() {
        assert!(
            parse_mice_position("SHORT", &[0x60, 1, 2]).is_err(),
            "too-short rejected",
        );
    }

    #[test]
    fn parse_mice_speed_alt_encoding_subtracts_800() -> TestResult {
        // APRS 1.0.1 §10 p.52 alt encoding: speed_raw ≥ 800 → speed -= 800.
        // SP = 108-28 = 80, DC = 28-28 = 0, SE = 28-28 = 0
        // speed_raw = 80×10 + 0/10 = 800 → after subtract 800 = 0 knots.
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 108, 28, 28, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.speed().map(Speed::as_knots), Some(0.0));
        Ok(())
    }

    #[test]
    fn parse_mice_speed_alt_encoding_decodes_20kt() -> TestResult {
        // Targeted regression for the APRS 1.0.1 §10 p.53 walked example.
        // Byte 4 = `n` (110): SP = 82, speed_raw = 820, after fold = 20 kt.
        // Use a same-row DC byte (still ≥ 28) that keeps course alt-fold
        // exercised: DC = 28 → dc = 0 → course_raw = 0, surfaces as None.
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, b'n', 28, 28, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.speed().map(Speed::as_knots), Some(20.0));
        Ok(())
    }

    #[test]
    fn parse_mice_course_alt_encoding_subtracts_400() -> TestResult {
        // APRS 1.0.1 §10 p.52 alt encoding: course_raw ≥ 400 → course -= 400.
        // From the spec's p.53 walked example: DC = '"' (34), SE = 'O' (79).
        // dc = 6, se = 51, course_raw = 6×100 + 51 = 651,
        // after fold = 651 - 400 = 251°.
        let dest = "SUQU5P";
        // Build a frame whose speed/course bytes are exactly those of the
        // spec example (`n`, `"`, `O` → 110, 34, 79).
        let info: &[u8] = &[0x60, 125, 73, 58, b'n', b'"', b'O', b'j', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(
            pos.speed().map(Speed::as_knots),
            Some(20.0),
            "spec p.53 speed"
        );
        assert_eq!(
            pos.course().map(Course::as_degrees),
            Some(251),
            "spec p.53 course"
        );
        Ok(())
    }

    #[test]
    fn mice_decode_message_off_duty() {
        assert_eq!(mice_decode_message(*b"PPP"), Some(MiceMessage::OffDuty));
    }

    #[test]
    fn mice_decode_message_emergency() {
        assert_eq!(mice_decode_message(*b"000"), Some(MiceMessage::Emergency));
    }

    #[test]
    fn mice_decode_message_in_service() {
        assert_eq!(mice_decode_message(*b"P0P"), Some(MiceMessage::InService));
    }

    #[test]
    fn mice_decode_message_custom_returns_none() {
        assert_eq!(mice_decode_message(*b"APP"), None);
        assert_eq!(mice_decode_message(*b"PKP"), None);
    }

    #[test]
    fn mice_decode_altitude_sea_level() -> TestResult {
        // Sea level = 0 m → wire value 10000. Base-91 of 10000 = ('"', '3', 'r').
        let altitude = mice_decode_altitude("\"3r}").ok_or("missing altitude")?;
        assert_eq!(altitude, 0);
        Ok(())
    }

    #[test]
    fn mice_decode_altitude_absent() {
        assert_eq!(mice_decode_altitude("no altitude here"), None);
        assert_eq!(mice_decode_altitude(""), None);
        assert_eq!(mice_decode_altitude("abc"), None);
    }

    #[test]
    fn parse_mice_populates_message_and_altitude() -> TestResult {
        let mut info = vec![0x60u8, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        info.extend_from_slice(b"\"3r}");
        let pos = parse_mice_position("SUQU5P", &info)?;
        assert_eq!(pos.mice_message(), Some(MiceMessage::OffDuty));
        assert_eq!(pos.mice_altitude_m(), Some(0));
        Ok(())
    }

    #[test]
    fn parse_mice_course_zero_is_none() -> TestResult {
        // SP = 28-28 = 0, DC = 28-28 = 0, SE = 28-28 = 0
        // course 0 = not known → None
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 28, 28, 28, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.speed().map(Speed::as_knots), Some(0.0));
        assert_eq!(pos.course(), None);
        Ok(())
    }

    // ---- Mic-E byte range validation tests ----

    #[test]
    fn mice_rejects_low_longitude_bytes() {
        // Longitude degrees start at byte 38 in the Mic-E wire table.
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 27, 73, 58, 40, 40, 40, b'>', b'/'];
        assert_eq!(
            parse_mice_position(dest, info),
            Err(AprsError::InvalidCoordinates)
        );
    }

    #[test]
    fn mice_rejects_zero_longitude_byte() {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 0, 58, 40, 40, 40, b'>', b'/'];
        assert_eq!(
            parse_mice_position(dest, info),
            Err(AprsError::InvalidCoordinates)
        );
    }

    #[test]
    fn mice_accepts_minimum_valid_byte() {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 38, 38, 28, 40, 40, 40, b'>', b'/'];
        assert!(parse_mice_position(dest, info).is_ok(), "min bytes ok");
    }

    #[test]
    fn mice_speed_byte_below_28_is_rejected() {
        let dest = "SUQU5P";
        // Byte 4 = 27 (SP+28 with SP < 0: impossible, must reject).
        let info: &[u8] = &[0x60, 125, 73, 58, 27, 40, 40, b'>', b'/'];
        assert!(parse_mice_position(dest, info).is_err());
    }

    #[test]
    fn mice_course_byte_below_28_is_rejected() {
        let dest = "SUQU5P";
        // Byte 6 = 27 (SE byte below valid range).
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 27, b'>', b'/'];
        assert!(parse_mice_position(dest, info).is_err());
    }

    // ---- parse_aprs_data_full tests ----

    #[test]
    fn full_dispatch_mice_current() -> TestResult {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest)?;
        assert!(matches!(result, AprsData::Position(_)));
        Ok(())
    }

    #[test]
    fn full_dispatch_mice_old() -> TestResult {
        let dest = "SUQU5P";
        let info: &[u8] = &[b'\'', 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest)?;
        assert!(matches!(result, AprsData::Position(_)));
        Ok(())
    }

    #[test]
    fn full_dispatch_mice_0x1c() -> TestResult {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x1C, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest)?;
        assert!(matches!(result, AprsData::Position(_)));
        Ok(())
    }

    #[test]
    fn full_dispatch_mice_0x1d() -> TestResult {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x1D, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest)?;
        assert!(matches!(result, AprsData::Position(_)));
        Ok(())
    }

    #[test]
    fn full_dispatch_non_mice_delegates() -> TestResult {
        let info = b"!4903.50N/07201.75W-Test";
        let result = parse_aprs_data_full(info, "APRS")?;
        assert!(matches!(result, AprsData::Position(_)));
        Ok(())
    }

    #[test]
    fn full_dispatch_empty_info() {
        assert!(parse_aprs_data_full(b"", "APRS").is_err(), "empty rejected");
    }

    #[test]
    fn mice_ambiguity_masks_both_coordinates_from_the_primary_example() -> TestResult {
        let info = b"`(_fn\"Oj/";
        let position = AprsPosition::from_mice_bytes("T4SQZZ", info)?;
        assert_eq!(position.ambiguity(), PositionAmbiguity::TwoDigits);
        assert!((position.latitude().as_degrees() - (44.0 + 31.0 / 60.0)).abs() < 1e-12);
        assert!((position.longitude().as_degrees() + 112.0 + 7.0 / 60.0).abs() < 1e-12);
        for (destination, ambiguity, minutes) in [
            ("T4SQPZ", PositionAmbiguity::OneDigit, 7.7),
            ("T4SQZZ", PositionAmbiguity::TwoDigits, 7.0),
            ("T4SZZZ", PositionAmbiguity::ThreeDigits, 0.0),
            ("T4ZZZZ", PositionAmbiguity::FourDigits, 0.0),
        ] {
            let position = AprsPosition::from_mice_bytes(destination, info)?;
            assert_eq!(position.ambiguity(), ambiguity);
            assert!(
                (position.longitude().as_degrees() + 112.0 + minutes / 60.0).abs() < 1e-12,
                "{destination}"
            );
        }
        Ok(())
    }

    #[test]
    fn mice_destination_and_coordinate_bytes_enforce_the_wire_table() -> TestResult {
        let valid = [b'`', 125, 73, 58, 40, 40, 40, b'>', b'/'];
        for destination in [
            "SUQU5PX", "KUQU5P", "SUQUKP", "SUQA5P", "SUQULP", "SUQUZP", "960000",
        ] {
            assert!(
                AprsPosition::from_mice_bytes(destination, &valid).is_err(),
                "accepted {destination}"
            );
        }
        for (index, byte) in [
            (1, 37),
            (1, 128),
            (2, 37),
            (2, 98),
            (3, 27),
            (3, 128),
            (4, 128),
            (5, 128),
            (6, 128),
        ] {
            let mut malformed = valid;
            *malformed.get_mut(index).ok_or("coordinate field")? = byte;
            assert!(
                AprsPosition::from_mice_bytes("SUQU5P", &malformed).is_err(),
                "accepted byte {byte} at {index}"
            );
        }
        // Degree 179 is the largest Mic-E longitude degree; 180 is folded by its wire table.
        let edge = [b'`', 107, 87, 127, 28, 28, 28, b'>', b'/'];
        assert!(
            (AprsPosition::from_mice_bytes("SUQUPP", &edge)?
                .longitude()
                .as_degrees()
                + 179.999_833_333_333)
                .abs()
                < 1e-9
        );
        Ok(())
    }
}
