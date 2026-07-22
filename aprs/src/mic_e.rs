//! Mic-E compressed position encoding (APRS 1.0.1 ch. 10).
//!
//! Mic-E is a compact encoding used by Kenwood HTs (including the TH-D75)
//! that splits the position across two fields: latitude is encoded in the
//! 6-character AX.25 destination address, and longitude + speed/course
//! are in the info field body.

use crate::error::AprsError;
use crate::packet::{AprsData, PositionAmbiguity, parse_aprs_extensions};
use crate::position::AprsPosition;
use crate::weather::extract_position_weather;

/// Mic-E standard message code.
///
/// Per APRS 1.0.1 §10.1, the three message bits (A/B/C) encoded by the
/// "custom" status of destination chars 0-2 select one of 8 standard
/// messages, or the eighth code indicates that a custom status is carried
/// in the comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MiceMessage {
    /// M0: "Off Duty" (111 standard, 000 custom).
    OffDuty,
    /// M1: "En Route" (110, 001).
    EnRoute,
    /// M2: "In Service" (101, 010).
    InService,
    /// M3: "Returning" (100, 011).
    Returning,
    /// M4: "Committed" (011, 100).
    Committed,
    /// M5: "Special" (010, 101).
    Special,
    /// M6: "Priority" (001, 110).
    Priority,
    /// Emergency: (000, 111) always means emergency.
    Emergency,
}

/// Parse a Mic-E encoded APRS position (APRS101.PDF Chapter 10).
///
/// Mic-E is a compact encoding used by Kenwood HTs (including the TH-D75)
/// that splits the position across two fields:
/// - **Latitude** is encoded in the 6-character AX.25 destination address
/// - **Longitude** and speed/course are in the info field body
///
/// Data type identifiers: `` ` `` (0x60, current Mic-E) or `'` (0x27, old Mic-E).
/// The TH-D75 uses current Mic-E (`` ` ``).
///
/// # Parameters
///
/// - `destination`: The AX.25 destination callsign (e.g., "T4SP0R")
/// - `info`: The full AX.25 information field (including the type byte)
///
/// # Errors
///
/// Returns [`AprsError`] if the Mic-E encoding is invalid.
pub fn parse_mice_position(destination: &str, info: &[u8]) -> Result<AprsPosition, AprsError> {
    let header = info.get(..9).ok_or(AprsError::InvalidFormat)?;
    let dest = destination.as_bytes();
    let dest_head = dest.get(..6).ok_or(AprsError::InvalidFormat)?;

    let data_type = *header.first().ok_or(AprsError::InvalidFormat)?;
    if data_type != b'`' && data_type != b'\'' && data_type != 0x1C && data_type != 0x1D {
        return Err(AprsError::InvalidFormat);
    }

    // Validate Mic-E longitude bytes are in valid range (28-127 per APRS101).
    let lon_bytes = header.get(1..4).ok_or(AprsError::InvalidFormat)?;
    for &b in lon_bytes {
        if b < 28 {
            return Err(AprsError::InvalidCoordinates);
        }
    }

    // --- Latitude from destination address ---
    // Each of the 6 destination chars encodes a latitude digit plus
    // N/S and longitude offset flags. Chars 0-9 and A-L map to digits.
    let mut lat_digits = [0u8; 6];
    let mut north = true;
    let mut lon_offset = 0i16;

    // Per APRS 1.0.1 §10 destination table (document p.44), chars 3-5
    // carry hemisphere/offset flags. `mice_dest_digit` returns the
    // North/+100/West indicator (true only for the std-set chars P-Z).
    for (i, &ch) in dest_head.iter().enumerate() {
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
    let mut latitude = lat_deg + lat_min / 60.0;
    if !north {
        latitude = -latitude;
    }

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
    let longitude_abs = f64::from(lon_deg) + (f64::from(lon_min) + f64::from(h) / 100.0) / 60.0;

    // Char 5 of destination: custom = West
    let dest5 = *dest_head.get(5).ok_or(AprsError::InvalidCoordinates)?;
    let west = mice_dest_is_custom(dest5);
    let longitude = if west { -longitude_abs } else { longitude_abs };

    let (speed_knots, course_degrees) = decode_mice_speed_course(header);

    // Symbol: info[7] = symbol code, info[8] = symbol table
    let symbol_code = header.get(7).map_or('/', |&b| b as char);
    let symbol_table = header.get(8).map_or('/', |&b| b as char);

    let comment = info.get(9..).map_or_else(String::new, |rest| {
        String::from_utf8_lossy(rest).into_owned()
    });

    // Decode the Mic-E standard message code from destination chars 0-2
    // (APRS 1.0.1 §10.1 Table 10). `None` if any char is in the custom range.
    let c0 = *dest_head.first().ok_or(AprsError::InvalidCoordinates)?;
    let c1 = *dest_head.get(1).ok_or(AprsError::InvalidCoordinates)?;
    let c2 = *dest_head.get(2).ok_or(AprsError::InvalidCoordinates)?;
    let mice_message = mice_decode_message([c0, c1, c2]);

    // Look for optional altitude in the comment (`<ccc>}` base-91, metres
    // offset from -10000) per APRS 1.0.1 §10.1.1.
    let mice_altitude_m = mice_decode_altitude(&comment);

    let weather = extract_position_weather(symbol_code, &comment);
    let extensions = parse_aprs_extensions(&comment);
    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
        weather,
        extensions,
        mice_message,
        mice_altitude_m,
        // Mic-E positions are not subject to §8.1.6 ambiguity masking.
        ambiguity: PositionAmbiguity::None,
    })
}

/// Decode Mic-E speed and course from the 3-byte SP/DC/SE block
/// `info[4..=6]`, per APRS 1.0.1 §10.
///
/// The decoder first computes the raw values as if both fields were
/// unmodulated:
///
/// ```text
///   speed_raw  = (SP-28) × 10 + (DC-28) / 10        (knots, 0..1999)
///   course_raw = ((DC-28) mod 10) × 100 + (SE-28)   (degrees, 0..999)
/// ```
///
/// §10 p.52 then specifies two **alt-encoding adjustments**:
///
/// - if `speed_raw  >= 800` → `speed  = speed_raw  - 800`
/// - if `course_raw >= 400` → `course = course_raw - 400`
///
/// These are not optional. The spec's own decoding worked example on
/// p.53 (info bytes `` `(_fn"Oj/ `` decoding to speed = 20 kt and
/// course = 251°) depends on subtracting 800 and 400 respectively.
/// Spec-compliant transmitters routinely use the alt range to expand
/// the encodable space for low speeds and back-quadrant courses;
/// refusing to apply the subtraction silently drops these decodes
/// (earlier code generations did exactly that).
///
/// After adjustment, the returned tuple has:
///
/// - `speed`  is `Some(0..=799)` knots, or `None` if the raw value is
///   outside the spec's representable range (impossible from a
///   spec-conformant transmitter).
/// - `course` is `Some(1..=360)` degrees, or `None` if the value is
///   `0` (the spec's "unknown / not relevant" sentinel, §10 p.49).
///
/// The byte-range guard (each of SP/DC/SE must be ≥ 28 per §10 p.47)
/// is enforced; a byte < 28 yields `(None, None)` rather than wrapping
/// via `saturating_sub`, which would silently produce a junk decode.
fn decode_mice_speed_course(header: &[u8]) -> (Option<u16>, Option<u16>) {
    let (Some(&speed_high), Some(&speed_course), Some(&course_low)) =
        (header.get(4), header.get(5), header.get(6))
    else {
        return (None, None);
    };
    if speed_high < 28 || speed_course < 28 || course_low < 28 {
        return (None, None);
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
    let speed_opt = (speed_raw < 800).then_some(speed_raw);
    let course_opt = (course_raw > 0 && course_raw <= 360).then_some(course_raw);
    (speed_opt, course_opt)
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
const fn mice_dest_is_custom(ch: u8) -> bool {
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
/// Used by the Mic-E TX builder [`crate::build_aprs_mice`].
#[must_use]
pub const fn mice_message_bits(msg: MiceMessage) -> (bool, bool, bool) {
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
/// via [`parse_mice_position`]. All other types delegate to
/// [`crate::packet::parse_aprs_data`].
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or data is invalid.
pub fn parse_aprs_data_full(info: &[u8], destination: &str) -> Result<AprsData, AprsError> {
    let first = *info.first().ok_or(AprsError::InvalidFormat)?;

    match first {
        // Mic-E current/old data types
        b'`' | b'\'' | 0x1C | 0x1D => {
            parse_mice_position(destination, info).map(AprsData::Position)
        }
        _ => crate::packet::parse_aprs_data(info),
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
        assert!((pos.latitude - 35.258).abs() < 0.01, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - (-97.755)).abs() < 0.01,
            "lon={}",
            pos.longitude
        );
        assert_eq!(pos.symbol_code, '>');
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.speed_knots, Some(121));
        assert_eq!(pos.course_degrees, Some(212));
        Ok(())
    }

    #[test]
    fn mice_dest_char_l_is_standard_not_custom() -> TestResult {
        // APRS 1.0.1 §10 destination table (document p.44): 'L' is the
        // standard-set ambiguity/space code: N/S = South, Long Offset =
        // +0, W/E = East. It is NOT a North/+100/West (custom) indicator.
        assert!(!mice_dest_is_custom(b'L'), "L must decode East/South/+0");
        assert_eq!(mice_dest_digit(b'L')?, (0, false), "L: space digit, Std");
        // Spot-check the rest of the table's bytes-4-6 classification.
        assert!(!mice_dest_is_custom(b'0'), "0-9 decode East/South/+0");
        assert!(!mice_dest_is_custom(b'9'), "0-9 decode East/South/+0");
        assert!(mice_dest_is_custom(b'P'), "P-Z decode West/North/+100");
        assert!(mice_dest_is_custom(b'Z'), "P-Z decode West/North/+100");
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
            pos.longitude > 0.0,
            "expected East (positive) for 'L', got lon={}",
            pos.longitude
        );
        assert!(
            (pos.longitude - 97.755).abs() < 0.01,
            "lon={}",
            pos.longitude
        );
        Ok(())
    }

    #[test]
    fn parse_mice_l_decodes_south_and_east() -> TestResult {
        // Destination "350L5L": char 3 = 'L' (N/S = South), char 5 = 'L'
        // (W/E = East) per document p.44. Latitude must be negative
        // (South) and longitude positive (East).
        let dest = "350L5L";
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert!(
            pos.latitude < 0.0,
            "expected South (negative) for 'L' at char 3, got lat={}",
            pos.latitude
        );
        assert!(
            pos.longitude > 0.0,
            "expected East (positive) for 'L' at char 5, got lon={}",
            pos.longitude
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
        assert_eq!(pos.speed_knots, Some(0));
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
        assert_eq!(pos.speed_knots, Some(20));
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
        assert_eq!(pos.speed_knots, Some(20), "spec p.53 speed");
        assert_eq!(pos.course_degrees, Some(251), "spec p.53 course");
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
        assert_eq!(pos.mice_message, Some(MiceMessage::OffDuty));
        assert_eq!(pos.mice_altitude_m, Some(0));
        Ok(())
    }

    #[test]
    fn parse_mice_course_zero_is_none() -> TestResult {
        // SP = 28-28 = 0, DC = 28-28 = 0, SE = 28-28 = 0
        // course 0 = not known → None
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 28, 28, 28, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.speed_knots, Some(0));
        assert_eq!(pos.course_degrees, None);
        Ok(())
    }

    // ---- Mic-E byte range validation tests ----

    #[test]
    fn mice_rejects_low_longitude_bytes() {
        // info[1] = 27 (below valid Mic-E range of 28)
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
        let info: &[u8] = &[0x60, 28, 28, 28, 40, 40, 40, b'>', b'/'];
        assert!(parse_mice_position(dest, info).is_ok(), "min bytes ok");
    }

    #[test]
    fn mice_speed_byte_below_28_yields_none() -> TestResult {
        // §10 p.47 IMPORTANT NOTE: SP/DC/SE bytes must be ≥ 28
        // (the wire offset). Earlier code used `saturating_sub`
        // which would silently fold an invalid byte to 0 and produce
        // a fake decode. Post-fix the parser surfaces these as
        // `None` for the affected field.
        let dest = "SUQU5P";
        // Byte 4 = 27 (SP+28 with SP < 0: impossible, must reject).
        let info: &[u8] = &[0x60, 125, 73, 58, 27, 40, 40, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.speed_knots, None);
        assert_eq!(pos.course_degrees, None);
        Ok(())
    }

    #[test]
    fn mice_course_byte_below_28_yields_none() -> TestResult {
        let dest = "SUQU5P";
        // Byte 6 = 27 (SE byte below valid range).
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 27, b'>', b'/'];
        let pos = parse_mice_position(dest, info)?;
        assert_eq!(pos.speed_knots, None);
        assert_eq!(pos.course_degrees, None);
        Ok(())
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
}
