//! Builders for outgoing APRS info fields and wire frames.
//!
//! Each public entry point has two flavours: the top-level builder
//! returns a KISS-framed byte vector ready for transport write, and the
//! `_packet` variant returns the unencoded [`Ax25Packet`] so callers can
//! inspect, log, or route it before wrapping it in KISS framing.

use ax25_codec::{
    Ax25Address, Ax25Packet, Callsign, CommandResponse, RouteEntry, Ssid, build_ax25,
};
use kiss_tnc::{KissFrame, encode_kiss_frame};

use crate::error::AprsError;
use crate::message::MAX_APRS_MESSAGE_TEXT_LEN;
use crate::mic_e::{MiceMessage, mice_message_bits};
use crate::packet::AprsTimestamp;
use crate::weather::AprsWeather;

// ---------------------------------------------------------------------------
// Private constants and helpers
// ---------------------------------------------------------------------------

/// APRS tocall for the Kenwood TH-D75 (per APRS tocall registry).
const APRS_TOCALL: &str = "APK005";

/// Canonical APRS tocall destination address. The callsign and SSID are
/// statically valid, so the panic branch is unreachable.
fn aprs_tocall() -> Ax25Address {
    Ax25Address::new(APRS_TOCALL, 0)
        .unwrap_or_else(|_| unreachable!("APRS_TOCALL is statically valid"))
}

/// Build a minimal APRS UI frame with the given source, destination, path,
/// and info field. Control = 0x03, PID = 0xF0. Marks the frame as a
/// command (APRS convention per APRS 1.0.1).
const fn ax25_ui_frame(
    source: Ax25Address,
    destination: Ax25Address,
    path: Vec<RouteEntry>,
    info: Vec<u8>,
) -> Ax25Packet {
    Ax25Packet {
        source,
        destination,
        digipeaters: path,
        command_or_response: Some(CommandResponse::Command),
        control: 0x03,
        protocol: 0xF0,
        info,
    }
}

/// Encode an [`Ax25Packet`] as a KISS-framed data frame ready for the
/// wire.
fn ax25_to_kiss_wire(packet: &Ax25Packet) -> Vec<u8> {
    let ax25_bytes = build_ax25(packet);
    encode_kiss_frame(&KissFrame::data(ax25_bytes))
}

/// Format latitude as APRS uncompressed `DDMM.HHN` (8 bytes).
///
/// Clamps out-of-range or non-finite input to `±90.0` so the output is
/// always a well-formed 8-byte APRS latitude field instead of garbage
/// like `"950000.00N"`. The `DDMM.hh` core is produced by the shared
/// [`crate::units::format_ddmm_hundredths`] helper, which carries
/// minute/degree overflow so the minutes field is whole minutes
/// `00..=59` plus hundredths `00..=99` per APRS 1.0.1 §6 p.23, never
/// the malformed `60.00` that a fixed-precision `format!` of
/// `59.9999` minutes would emit.
fn format_aprs_latitude(lat: f64) -> String {
    let lat = if lat.is_finite() {
        lat.clamp(-90.0, 90.0)
    } else {
        0.0
    };
    let hemisphere = if lat >= 0.0 { 'N' } else { 'S' };
    let core = crate::units::format_ddmm_hundredths(lat.abs(), 2);
    format!("{core}{hemisphere}")
}

/// Format longitude as APRS uncompressed `DDDMM.HHE` (9 bytes).
///
/// Clamps out-of-range or non-finite input to `±180.0`. The `DDDMM.hh`
/// core is produced by the shared
/// [`crate::units::format_ddmm_hundredths`] helper, which carries
/// minute/degree overflow so the minutes field is whole minutes
/// `00..=59` plus hundredths `00..=99` per APRS 1.0.1 §6 p.24, never
/// the malformed `60.00`.
fn format_aprs_longitude(lon: f64) -> String {
    let lon = if lon.is_finite() {
        lon.clamp(-180.0, 180.0)
    } else {
        0.0
    };
    let hemisphere = if lon >= 0.0 { 'E' } else { 'W' };
    let core = crate::units::format_ddmm_hundredths(lon.abs(), 3);
    format!("{core}{hemisphere}")
}

/// Encode a `u32` value as 4 bytes of base-91.
///
/// Base-91 encoding uses characters 33 (`!`) through 123 (`{`), giving
/// 91 possible values per byte. Four bytes can represent values up to
/// 91^4 - 1 = 68,574,960.
fn encode_base91_4(mut value: u32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for slot in out.iter_mut().rev() {
        // value % 91 is in 0..91 so the truncation to u8 is safe.
        let digit = (value % 91) as u8;
        *slot = digit + 33;
        value /= 91;
    }
    out
}

// ---------------------------------------------------------------------------
// APRS position builder (uncompressed)
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS uncompressed position report.
///
/// Composes an AX.25 UI frame with:
/// - Destination: `APK005-0` (Kenwood TH-D75 tocall)
/// - Digipeater path: WIDE1-1, WIDE2-1
/// - Info field: `!DDMM.HHN/DDDMM.HHEscomment`
///
/// Returns wire-ready bytes (FEND-delimited KISS frame) suitable for
/// direct transport write.
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `latitude`: Decimal degrees, positive = North, negative = South.
/// - `longitude`: Decimal degrees, positive = East, negative = West.
/// - `symbol_table`: APRS symbol table character (`/` for primary, `\\` for alternate).
/// - `symbol_code`: APRS symbol code character (e.g., `>` for car, `-` for house).
/// - `comment`: Free-form comment text appended after the position.
/// - `path`: Digipeater path. Supply an empty slice for direct
///   transmission with no digipeating.
#[must_use]
pub fn build_aprs_position_report(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_position_report_packet(
        source,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Like [`build_aprs_position_report`] but returns the unencoded
/// [`Ax25Packet`] so callers can inspect, log, or route it before
/// wrapping it in KISS framing.
#[must_use]
pub fn build_aprs_position_report_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    let info = format!("!{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");
    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

// ---------------------------------------------------------------------------
// APRS message builders
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS message packet.
///
/// Composes an AX.25 UI frame with the APRS message format:
/// `:ADDRESSEE:text{ID`
///
/// The addressee is padded to exactly 9 characters per the APRS spec.
/// Message text that exceeds [`MAX_APRS_MESSAGE_TEXT_LEN`] (67 bytes) is
/// **truncated**; use [`build_aprs_message_checked`] if you want a
/// hard error on overlong input.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `addressee`: Destination station callsign (up to 9 chars).
/// - `text`: Message text content.
/// - `message_id`: Optional message sequence number for ack/rej tracking.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_message(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_message_packet(
        source, addressee, text, message_id, path,
    ))
}

/// Like [`build_aprs_message`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_message_packet(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
    path: &[RouteEntry],
) -> Ax25Packet {
    // Pad addressee to exactly 9 characters.
    let padded_addressee = format!("{addressee:<9}");
    let padded_addressee = padded_addressee.get(..9).unwrap_or(&padded_addressee);

    // Truncate text to the spec limit on a UTF-8 char boundary.
    let text = if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
        let mut end = MAX_APRS_MESSAGE_TEXT_LEN;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.get(..end).unwrap_or(text)
    } else {
        text
    };

    let info = message_id.map_or_else(
        || format!(":{padded_addressee}:{text}"),
        |id| format!(":{padded_addressee}:{text}{{{id}"),
    );

    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Like [`build_aprs_message`] but returns an error when the text
/// exceeds the APRS 1.0.1 67-byte limit instead of silently truncating.
///
/// # Errors
///
/// Returns [`AprsError::MessageTooLong`] if `text.len() > 67`.
pub fn build_aprs_message_checked(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
    path: &[RouteEntry],
) -> Result<Vec<u8>, AprsError> {
    if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
        return Err(AprsError::MessageTooLong(text.len()));
    }
    Ok(build_aprs_message(
        source, addressee, text, message_id, path,
    ))
}

// ---------------------------------------------------------------------------
// APRS object builders
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS object report.
///
/// Composes an AX.25 UI frame with the APRS object format:
/// `;name_____*DDHHMMzDDMM.HHN/DDDMM.HHEscomment`
///
/// The object name is padded to exactly 9 characters per the APRS spec.
///
/// This convenience builder emits a **placeholder zero DHM-zulu
/// timestamp** (`000000z`, day 0); it is sans-io and cannot read the
/// clock. Day 0 is outside the spec-valid `01..=31` range and is
/// rejected by [`AprsTimestamp::parse`]. Callers that need a real
/// timestamp must use [`build_aprs_object_with_timestamp`], which takes
/// an explicit [`AprsTimestamp`].
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `name`: Object name (up to 9 characters).
/// - `live`: `true` for a live object (`*`), `false` for killed (`_`).
/// - `latitude`: Decimal degrees, positive = North.
/// - `longitude`: Decimal degrees, positive = East.
/// - `symbol_table`: APRS symbol table character.
/// - `symbol_code`: APRS symbol code character.
/// - `comment`: Free-form comment text.
/// - `path`: Digipeater path.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "APRS object wire fields are fundamentally positional"
)]
pub fn build_aprs_object(
    source: &Ax25Address,
    name: &str,
    live: bool,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    // Use a placeholder DHM zulu timestamp `000000z`. Callers needing a
    // real timestamp should use [`build_aprs_object_with_timestamp`].
    build_aprs_object_with_timestamp(
        source,
        name,
        live,
        AprsTimestamp::DhmZulu {
            day: 0,
            hour: 0,
            minute: 0,
        },
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
}

/// Build a KISS-encoded APRS object report with a caller-supplied
/// timestamp.
///
/// Identical to [`build_aprs_object`] but uses the provided
/// [`AprsTimestamp`] instead of the `000000z` placeholder.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "APRS object wire fields are fundamentally positional"
)]
pub fn build_aprs_object_with_timestamp(
    source: &Ax25Address,
    name: &str,
    live: bool,
    timestamp: AprsTimestamp,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_object_with_timestamp_packet(
        source,
        name,
        live,
        timestamp,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Like [`build_aprs_object_with_timestamp`] but returns the unencoded
/// [`Ax25Packet`] for callers that want to inspect or route it.
///
/// The object `name` must satisfy APRS 1.0.1 §11 p.58 "fixed
/// 9-character" rule: 1..=9 bytes of printable ASCII. Names shorter
/// than 9 bytes are space-padded; names longer than 9 bytes are
/// **silently truncated** to the first 9 bytes (the wire frame then
/// identifies a *different* object than the caller intended). Prefer
/// [`build_aprs_object_with_timestamp_checked`] / its `_packet`
/// variant when caller input is uncontrolled; they return
/// [`AprsError::InvalidObjectName`] for malformed input instead.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "APRS object wire fields are fundamentally positional"
)]
pub fn build_aprs_object_with_timestamp_packet(
    source: &Ax25Address,
    name: &str,
    live: bool,
    timestamp: AprsTimestamp,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    let padded_name = format!("{name:<9}");
    let padded_name = padded_name.get(..9).unwrap_or(&padded_name);
    let live_char = if live { '*' } else { '_' };
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    let ts = timestamp.to_wire_string();

    let info = format!(
        ";{padded_name}{live_char}{ts}{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}"
    );

    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Validate that an APRS object name satisfies APRS 1.0.1 §11 p.58:
/// 1..=9 bytes of printable ASCII (0x20..=0x7E).
///
/// Empty names are rejected (the spec implicitly requires at least one
/// non-space character: a 9-space name would render as a blank
/// identifier). Names over 9 bytes are rejected to prevent the silent
/// truncation that the unchecked builder performs.
fn validate_object_name(name: &str) -> Result<(), AprsError> {
    if name.is_empty() {
        return Err(AprsError::InvalidObjectName(
            "object name must not be empty",
        ));
    }
    if name.len() > 9 {
        return Err(AprsError::InvalidObjectName(
            "object name must be at most 9 bytes",
        ));
    }
    if !name.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(AprsError::InvalidObjectName(
            "object name must be printable ASCII",
        ));
    }
    Ok(())
}

/// Validating counterpart of [`build_aprs_object_with_timestamp`].
///
/// Returns [`AprsError::InvalidObjectName`] if `name` fails the
/// 1..=9-byte printable-ASCII contract from APRS 1.0.1 §11 p.58.
///
/// # Errors
///
/// As above.
#[expect(
    clippy::too_many_arguments,
    reason = "APRS object wire fields are fundamentally positional"
)]
pub fn build_aprs_object_with_timestamp_checked(
    source: &Ax25Address,
    name: &str,
    live: bool,
    timestamp: AprsTimestamp,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Result<Vec<u8>, AprsError> {
    validate_object_name(name)?;
    Ok(build_aprs_object_with_timestamp(
        source,
        name,
        live,
        timestamp,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Validating counterpart of [`build_aprs_object_with_timestamp_packet`].
///
/// # Errors
///
/// Returns [`AprsError::InvalidObjectName`] if `name` fails validation.
#[expect(
    clippy::too_many_arguments,
    reason = "APRS object wire fields are fundamentally positional"
)]
pub fn build_aprs_object_with_timestamp_checked_packet(
    source: &Ax25Address,
    name: &str,
    live: bool,
    timestamp: AprsTimestamp,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Result<Ax25Packet, AprsError> {
    validate_object_name(name)?;
    Ok(build_aprs_object_with_timestamp_packet(
        source,
        name,
        live,
        timestamp,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

// ---------------------------------------------------------------------------
// APRS item builders
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS item report.
///
/// Composes an AX.25 UI frame with the APRS item format:
/// `)name!DDMM.HHN/DDDMM.HHEscomment` (live) or
/// `)name_DDMM.HHN/DDDMM.HHEscomment` (killed).
///
/// The item name must be 3-9 characters per APRS101 Chapter 11.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `name`: Item name (3-9 characters).
/// - `live`: `true` for a live item (`!`), `false` for killed (`_`).
/// - `lat`: Decimal degrees, positive = North.
/// - `lon`: Decimal degrees, positive = East.
/// - `symbol_table`: APRS symbol table character.
/// - `symbol_code`: APRS symbol code character.
/// - `comment`: Free-form comment text.
/// - `path`: Digipeater path.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "APRS item wire fields are fundamentally positional"
)]
pub fn build_aprs_item(
    source: &Ax25Address,
    name: &str,
    live: bool,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_item_packet(
        source,
        name,
        live,
        lat,
        lon,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Like [`build_aprs_item`] but returns the unencoded [`Ax25Packet`].
///
/// The `name` must be 3-9 bytes of printable ASCII excluding `!` and
/// `_` per APRS 1.0.1 §11 p.59. This unchecked entry point performs
/// **no validation**: passing a name shorter than 3 chars, longer
/// than 9 chars, or containing `!`/`_` will produce a malformed item
/// that no spec-compliant parser will accept. Prefer
/// [`build_aprs_item_checked`] / its `_packet` variant when caller
/// input is uncontrolled.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "APRS item wire fields are fundamentally positional"
)]
pub fn build_aprs_item_packet(
    source: &Ax25Address,
    name: &str,
    live: bool,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    let live_char = if live { '!' } else { '_' };
    let lat_str = format_aprs_latitude(lat);
    let lon_str = format_aprs_longitude(lon);
    let info = format!("){name}{live_char}{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");
    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Validate that an APRS item name satisfies APRS 1.0.1 §11 p.59:
///
/// - 3..=9 bytes long, and
/// - every byte is printable ASCII (0x20..=0x7E), and
/// - no byte is `!` (0x21) or `_` (0x5F); these would terminate the
///   name field on the wire and produce a malformed item.
fn validate_item_name(name: &str) -> Result<(), AprsError> {
    if name.len() < 3 {
        return Err(AprsError::InvalidItemName(
            "item name must be at least 3 bytes",
        ));
    }
    if name.len() > 9 {
        return Err(AprsError::InvalidItemName(
            "item name must be at most 9 bytes",
        ));
    }
    for b in name.bytes() {
        if !(0x20..=0x7E).contains(&b) {
            return Err(AprsError::InvalidItemName(
                "item name must be printable ASCII",
            ));
        }
        if b == b'!' || b == b'_' {
            return Err(AprsError::InvalidItemName(
                "item name must not contain '!' or '_' (terminator bytes)",
            ));
        }
    }
    Ok(())
}

/// Validating counterpart of [`build_aprs_item`].
///
/// # Errors
///
/// Returns [`AprsError::InvalidItemName`] if `name` violates the
/// APRS 1.0.1 §11 p.59 rules (length 3-9, printable ASCII, no `!`/`_`).
#[expect(
    clippy::too_many_arguments,
    reason = "APRS item wire fields are fundamentally positional"
)]
pub fn build_aprs_item_checked(
    source: &Ax25Address,
    name: &str,
    live: bool,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Result<Vec<u8>, AprsError> {
    validate_item_name(name)?;
    Ok(build_aprs_item(
        source,
        name,
        live,
        lat,
        lon,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Validating counterpart of [`build_aprs_item_packet`].
///
/// # Errors
///
/// Returns [`AprsError::InvalidItemName`] if `name` violates the
/// APRS 1.0.1 §11 p.59 rules.
#[expect(
    clippy::too_many_arguments,
    reason = "APRS item wire fields are fundamentally positional"
)]
pub fn build_aprs_item_checked_packet(
    source: &Ax25Address,
    name: &str,
    live: bool,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Result<Ax25Packet, AprsError> {
    validate_item_name(name)?;
    Ok(build_aprs_item_packet(
        source,
        name,
        live,
        lat,
        lon,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

// ---------------------------------------------------------------------------
// APRS weather builders
// ---------------------------------------------------------------------------

/// Append the shared 7-field weather tail (gust, temperature, rain
/// 1 h / 24 h / since-midnight, humidity, barometric pressure) to an
/// info field, clamping every value to its APRS 1.0.1 §12 p.64 field
/// width before formatting.
///
/// Both the complete-weather-report builder
/// ([`build_aprs_position_weather_packet`]) and the positionless one
/// ([`build_aprs_weather_packet`]) emit a byte-identical tail after
/// their respective wind direction/speed prefixes; this helper is the
/// single place that tail (and its range clamping) lives.
///
/// # Clamping (APRS 1.0.1 §12 p.64)
///
/// - gust `g`, rain `r`/`p`/`P`: 3-digit fields → saturate to `0..=999`.
/// - temperature `t`: 3 columns, negatives written sign + 2 digits, so
///   the spec range is `-99..=999` → saturate to that range.
/// - humidity `h`: 2-digit field where `00` means 100 %; the spec range
///   is `1..=100` → saturate to that range *before* the `100 → "00"`
///   substitution (so `0 → 1 → "01"` and `>100 → 100 → "00"`).
/// - pressure `b`: 5-digit field (tenths of hPa) → saturate to
///   `0..=99999`.
///
/// Unclamped values would overflow the fixed field width and the
/// crate's own parser then drops every following field; clamping keeps
/// each field within spec width.
fn write_weather_tail(info: &mut String, weather: &AprsWeather) {
    use std::fmt::Write as _;

    if let Some(gust) = weather.wind_gust {
        let gust = gust.min(999);
        let _ = write!(info, "g{gust:03}");
    }
    if let Some(temp) = weather.temperature {
        let temp = temp.clamp(-99, 999);
        let _ = write!(info, "t{temp:03}");
    }
    if let Some(rain) = weather.rain_1h {
        let rain = rain.min(999);
        let _ = write!(info, "r{rain:03}");
    }
    if let Some(rain) = weather.rain_24h {
        let rain = rain.min(999);
        let _ = write!(info, "p{rain:03}");
    }
    if let Some(rain) = weather.rain_since_midnight {
        let rain = rain.min(999);
        let _ = write!(info, "P{rain:03}");
    }
    if let Some(hum) = weather.humidity {
        // Spec range 1..=100; APRS encodes 100 % as "00".
        let hum = hum.clamp(1, 100);
        let hum_val = if hum == 100 { 0 } else { hum };
        let _ = write!(info, "h{hum_val:02}");
    }
    if let Some(pres) = weather.pressure {
        let pres = pres.min(99_999);
        let _ = write!(info, "b{pres:05}");
    }
}

/// Build a KISS-encoded positionless APRS weather report.
///
/// Composes an AX.25 UI frame with the APRS positionless weather format:
/// `_MMDDHHMMcSSSsSSS gSSS tTTT rRRR pRRR PRRR hHH bBBBBB`
///
/// Uses a placeholder timestamp (`00000000`). Callers needing a real
/// timestamp should build the info field manually.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `weather`: Weather data to encode. Missing fields are omitted.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_weather(
    source: &Ax25Address,
    weather: &AprsWeather,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_weather_packet(source, weather, path))
}

/// Build a combined APRS position + weather report as a single KISS
/// frame, per APRS 1.0.1 §12.1.
///
/// Uses the uncompressed position format with symbol code `_` (weather
/// station), followed by the `DDD/SSS` CSE/SPD wind direction/speed
/// extension, then the remaining weather fields. This is the "complete
/// weather report" wire form used by most fixed weather stations.
#[must_use]
pub fn build_aprs_position_weather(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    weather: &AprsWeather,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_position_weather_packet(
        source,
        latitude,
        longitude,
        symbol_table,
        weather,
        path,
    ))
}

/// Like [`build_aprs_position_weather`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_position_weather_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    weather: &AprsWeather,
    path: &[RouteEntry],
) -> Ax25Packet {
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    // Symbol code is always `_` (weather station) for this format.
    // Wind direction and speed go into the CSE/SPD slot (`DDD/SSS`),
    // with "..." for missing values. Both are 3-digit fields (APRS
    // 1.0.1 §12 p.65); clamp to `0..=999` so an out-of-range value
    // cannot overflow the field width and corrupt the following tail.
    let wind_dir = weather
        .wind_direction
        .map_or_else(|| "...".to_owned(), |d| format!("{:03}", d.min(999)));
    let wind_spd = weather
        .wind_speed
        .map_or_else(|| "...".to_owned(), |s| format!("{:03}", s.min(999)));

    let mut info = format!("!{lat_str}{symbol_table}{lon_str}_{wind_dir}/{wind_spd}");
    write_weather_tail(&mut info, weather);

    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Like [`build_aprs_weather`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_weather_packet(
    source: &Ax25Address,
    weather: &AprsWeather,
    path: &[RouteEntry],
) -> Ax25Packet {
    use std::fmt::Write as _;

    let mut info = String::from("_00000000");

    // Wind direction `c` and speed `s` are 3-digit fields (APRS 1.0.1
    // §12 p.64); clamp to `0..=999` so an out-of-range value cannot
    // overflow the field width and corrupt the shared tail. The
    // gust→pressure tail is emitted by `write_weather_tail`.
    if let Some(dir) = weather.wind_direction {
        let _ = write!(info, "c{:03}", dir.min(999));
    }
    if let Some(spd) = weather.wind_speed {
        let _ = write!(info, "s{:03}", spd.min(999));
    }
    write_weather_tail(&mut info, weather);

    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

// ---------------------------------------------------------------------------
// APRS compressed position builder
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS compressed position report.
///
/// Compressed format uses base-91 encoding for latitude and longitude,
/// producing smaller packets than the uncompressed `DDMM.HH` format.
/// Encoding follows APRS101 Chapter 9.
///
/// The compressed body is 13 bytes:
/// `sym_table(1) YYYY(4) XXXX(4) sym_code(1) cs(1) s(1) t(1)`
///
/// Where `cs`, `s`, and `t` are set to indicate no course/speed/altitude
/// data (space characters).
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `latitude`: Decimal degrees, positive = North, negative = South.
/// - `longitude`: Decimal degrees, positive = East, negative = West.
/// - `symbol_table`: APRS symbol table character (`/` for primary, `\\` for alternate).
/// - `symbol_code`: APRS symbol code character (e.g., `>` for car, `-` for house).
/// - `comment`: Free-form comment text appended after the compressed position.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_position_compressed(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_position_compressed_packet(
        source,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Sanitize a coordinate to a finite value clamped to `[min, max]`.
///
/// Used as the input filter to the compressed-position encoder so that
/// out-of-range or non-finite (`NaN`, `±∞`) input never silently
/// produces a wrong-but-valid-looking wire encoding. Non-finite input
/// falls through to `0.0` (the geographic centre on the relevant axis)
/// rather than the clamped extremum to surface "value missing" more
/// distinctively in downstream decoding.
const fn sanitize_coord(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

/// Like [`build_aprs_position_compressed`] but returns the unencoded
/// [`Ax25Packet`].
///
/// # Input sanitisation
///
/// `latitude` and `longitude` are clamped to `±90` / `±180`
/// respectively, matching the sibling uncompressed builder. Non-finite
/// inputs (`NaN`, `±∞`) fall through to `0.0`. This is the equivalent
/// of pre-validating via [`crate::Latitude`] / [`crate::Longitude`]
/// newtypes; the boundary here exists because this builder accepts
/// `f64` for ergonomic interop with raw GPS samples.
#[must_use]
pub fn build_aprs_position_compressed_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    let latitude = sanitize_coord(latitude, -90.0, 90.0);
    let longitude = sanitize_coord(longitude, -180.0, 180.0);
    // After clamping, both expressions are bounded:
    //   lat_val ∈ [0, 380_926 × 180]  = [0, 68_566_680]
    //   lon_val ∈ [0, 190_463 × 360]  = [0, 68_566_680]
    // Both fit comfortably in u32. The casts cannot truncate or
    // sign-flip because the input is non-negative by construction.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "input is clamped to a non-negative range that fits u32"
    )]
    let lat_val = (380_926.0 * (90.0 - latitude)) as u32;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "input is clamped to a non-negative range that fits u32"
    )]
    let lon_val = (190_463.0 * (longitude + 180.0)) as u32;
    let lat_encoded = encode_base91_4(lat_val);
    let lon_encoded = encode_base91_4(lon_val);

    let mut info = Vec::with_capacity(1 + 13 + comment.len());
    info.push(b'!');
    info.push(symbol_table as u8);
    info.extend_from_slice(&lat_encoded);
    info.extend_from_slice(&lon_encoded);
    info.push(symbol_code as u8);
    info.push(b' '); // cs: no course/speed data
    info.push(b' ');
    info.push(b' '); // t: compression type = no data
    info.extend_from_slice(comment.as_bytes());

    ax25_ui_frame(source.clone(), aprs_tocall(), path.to_vec(), info)
}

// ---------------------------------------------------------------------------
// APRS status builders
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS status report.
///
/// Composes an AX.25 UI frame with the APRS status format:
/// `>text\r`
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `text`: Status text content.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_status(source: &Ax25Address, text: &str, path: &[RouteEntry]) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_status_packet(source, text, path))
}

/// Like [`build_aprs_status`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_status_packet(
    source: &Ax25Address,
    text: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    let mut info = Vec::with_capacity(1 + text.len() + 1);
    info.push(b'>');
    info.extend_from_slice(text.as_bytes());
    info.push(b'\r');
    ax25_ui_frame(source.clone(), aprs_tocall(), path.to_vec(), info)
}

// ---------------------------------------------------------------------------
// Mic-E builders (APRS101 Chapter 10)
// ---------------------------------------------------------------------------

/// Build a Mic-E encoded APRS position report for KISS transmission.
///
/// Mic-E is the most compact position format and the native format
/// used by Kenwood HTs including the TH-D75. The latitude is encoded
/// in the AX.25 destination address, and longitude + speed/course
/// are in the info field.
///
/// Encoding per APRS101 Chapter 10:
/// - Destination address: 6 chars encoding latitude digits + N/S + lon offset + W/E flags
/// - Info field: type byte (`0x60` for current Mic-E) + 3 lon bytes + 3 speed/course bytes
///   + symbol code + symbol table + comment
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `latitude`: Decimal degrees, positive = North, negative = South.
/// - `longitude`: Decimal degrees, positive = East, negative = West.
/// - `speed_knots`: Speed in knots (0-799).
/// - `course_deg`: Course in degrees (0-360; 0 = unknown).
/// - `symbol_table`: APRS symbol table character (`/` for primary, `\\` for alternate).
/// - `symbol_code`: APRS symbol code character (e.g., `>` for car).
/// - `comment`: Free-form comment text.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Mic-E wire fields are fundamentally positional"
)]
pub fn build_aprs_mice(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    speed_knots: u16,
    course_deg: u16,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    // Default to Off Duty for backwards compat with the old signature.
    build_aprs_mice_with_message(
        source,
        latitude,
        longitude,
        speed_knots,
        course_deg,
        MiceMessage::OffDuty,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
}

/// Build a Mic-E encoded APRS position report with a specific
/// [`MiceMessage`] status code.
///
/// Per APRS 1.0.1 §10.1 Table 10, the 8 standard codes are encoded in
/// the message bits of the first three destination characters. The
/// other Mic-E encoder entrypoint, [`build_aprs_mice`], uses Off Duty
/// for backwards compatibility.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "Mic-E wire fields are fundamentally positional"
)]
pub fn build_aprs_mice_with_message(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    speed_knots: u16,
    course_deg: u16,
    message: MiceMessage,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_mice_with_message_packet(
        source,
        latitude,
        longitude,
        speed_knots,
        course_deg,
        message,
        symbol_table,
        symbol_code,
        comment,
        path,
    ))
}

/// Like [`build_aprs_mice_with_message`] but returns the unencoded
/// [`Ax25Packet`] for callers that want to inspect or route it.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Mic-E wire fields are fundamentally positional; packing all steps in one function keeps the APRS101 §10 cross-reference readable"
)]
pub fn build_aprs_mice_with_message_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    speed_knots: u16,
    course_deg: u16,
    message: MiceMessage,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    // Sanitise position so the wire fields never overflow and a
    // non-finite input (`NaN`, `±∞`) cannot saturate the integer
    // casts below into a wrong-looking-but-encodable value. This
    // matches the policy of [`build_aprs_position_report_packet`]
    // and [`build_aprs_position_compressed_packet`]; the Mic-E
    // builder used to clamp without the `is_finite()` guard, which
    // silently produced byte 0 for both `NaN` inputs.
    let latitude = sanitize_coord(latitude, -90.0, 90.0);
    let longitude = sanitize_coord(longitude, -180.0, 180.0);
    let north = latitude >= 0.0;
    let west = longitude < 0.0;
    let lat_abs = latitude.abs();
    let lon_abs = longitude.abs();

    // Decompose latitude into digits: DD MM.HH. Clamp the rounding so
    // hundredths == 100 rolls into minutes correctly.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lat_abs is clamped to 0..=90"
    )]
    let lat_deg = lat_abs as u32;
    let lat_min_f = (lat_abs - f64::from(lat_deg)) * 60.0;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lat_min_f is in 0..60"
    )]
    let lat_min = lat_min_f as u32;
    let lat_hundredths_f = ((lat_min_f - f64::from(lat_min)) * 100.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lat_hundredths_f rounds to an integer in 0..=100"
    )]
    let lat_hundredths = (lat_hundredths_f as u32).min(99);

    // All digit casts are safe: the u32 values are bounded to 0..=9 (or
    // 0..=99 for hundredths) by the division/min chains above.
    let d0 = (lat_deg / 10).min(9) as u8;
    let d1 = (lat_deg % 10) as u8;
    let d2 = (lat_min / 10).min(9) as u8;
    let d3 = (lat_min % 10) as u8;
    let d4 = (lat_hundredths / 10) as u8;
    let d5 = (lat_hundredths % 10) as u8;

    // Message bits (A, B, C) from the 3-bit index. Per APRS 1.0.1 §10.1
    // Table 10, bit = 1 (Std1, uppercase P-Y range) when set.
    let (msg_a, msg_b, msg_c) = mice_message_bits(message);

    // Encode destination address characters. Chars 0-2 carry message
    // bits A/B/C: if the bit is 1, pick from P-Y; otherwise 0-9.
    //
    // The "longitude offset" flag carried on char 4 of the destination
    // is **set** in two non-contiguous ranges per APRS 1.0.1 §10 p.47:
    //
    //   - longitude in 0..10°    (so the info-field d-byte can use
    //                            the high column 118-127 / `v`-`DEL`),
    //   - longitude in 100..180° (so the info-field d-byte can use the
    //                            low column 108-117 or 38-107 per the
    //                            1.1 correction).
    //
    // Earlier code generations only set the bit for ≥100°, which left
    // 0-9° longitudes encoded into the info-field byte range 28..37,
    // outside the spec-listed valid encodings. Spec-strict receivers
    // would mis-decode.
    let lon_offset = !(10.0..100.0).contains(&lon_abs);
    let dest_chars: [u8; 6] = [
        if msg_a { b'P' + d0 } else { b'0' + d0 },
        if msg_b { b'P' + d1 } else { b'0' + d1 },
        if msg_c { b'P' + d2 } else { b'0' + d2 },
        if north { b'P' + d3 } else { b'0' + d3 },
        if lon_offset { b'P' + d4 } else { b'0' + d4 },
        if west { b'P' + d5 } else { b'0' + d5 },
    ];
    // Every byte in `dest_chars` is in the range 0x30-0x59 (P-Y for
    // custom, 0-9 for standard) by construction above, all valid ASCII.
    let Ok(dest_callsign) = std::str::from_utf8(&dest_chars) else {
        unreachable!("Mic-E destination chars are ASCII by construction")
    };

    // Longitude-degrees encoding per APRS 1.0.1 §10 p.47 (as corrected
    // by addendum 1.1):
    //
    //   - 0..=9°    offset set, `d = degrees + 90`,  byte 118-127
    //   - 10..=99°  no offset,  `d = degrees`,       byte 38-127
    //   - 100..=109° offset set, `d = degrees - 20`, byte 108-117
    //   - 110..=179° offset set, `d = degrees - 100`, byte 38-107
    //
    // Byte on the wire is always `d + 28`. Each range is laid out so
    // the d-byte is in 0..=99 (fits u8 without truncation).
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lon_abs is clamped to 0..=180 so fits u16"
    )]
    let lon_deg_raw = lon_abs as u16;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "every branch yields a value in 0..=99 by spec-table construction; the casts cannot truncate"
    )]
    let d = if lon_deg_raw < 10 {
        // 0-9° with offset bit set: d = degrees + 90.
        (lon_deg_raw + 90) as u8
    } else if lon_deg_raw < 100 {
        // 10-99° no offset: d = degrees.
        lon_deg_raw as u8
    } else if lon_deg_raw < 110 {
        // 100-109° with offset: d = degrees - 20.
        (lon_deg_raw - 20) as u8
    } else {
        // 110-179° with offset: d = degrees - 100.
        (lon_deg_raw - 100) as u8
    };

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lon_abs is clamped to 0..=180 so the u32 cast fits"
    )]
    let lon_min_f = (lon_abs - f64::from(lon_abs as u32)) * 60.0;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lon_min_f is in 0..60"
    )]
    let lon_min_int = lon_min_f as u8;
    // Hundredths of a minute. Rounding `0.999... * 100.0` can yield 100,
    // which when added to the wire offset of 28 would produce byte 128,
    // outside the spec-mandated Mic-E receivable range of 28..=127
    // (APRS 1.0.1 §10.3.3). Clamp to 0..=99 to keep the wire byte legal;
    // the latitude-side computation at `lat_hundredths` uses the same
    // pattern.
    let lon_hundredths_f = ((lon_min_f - f64::from(lon_min_int)) * 100.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lon_hundredths_f rounds to an integer in 0..=100; clamped to 0..=99 below"
    )]
    let lon_hundredths = (lon_hundredths_f as u32).min(99) as u8;

    // Minutes encoding: if < 10, add 60.
    let m = if lon_min_int < 10 {
        lon_min_int + 60
    } else {
        lon_min_int
    };

    // Speed/course encoding per APRS 1.0.1 §10 p.52.
    // SP = speed / 10, remainder from DC.
    // DC = (speed % 10) * 10 + course / 100
    // SE = course % 100
    //
    // Clamp to the spec-legal ranges *before* the arithmetic: speed
    // 0..=799 knots and course 0..=360°. The decode side adjusts "if
    // speed >= 800 subtract 800" / "if course >= 400 subtract 400", so
    // these are the maximum representable values. Without the clamp an
    // out-of-range speed (e.g. 2280 kt → SP = 228, byte 228 + 28 = 256)
    // overflows the `+ 28` wire offset: a debug panic under
    // overflow-checks and an undecodable byte > 127 in release. This
    // mirrors the longitude-hundredths `.min(99)` clamp above.
    let speed_knots = speed_knots.min(799);
    let course_deg = course_deg.min(360);
    // After the clamp, `speed_knots / 10` is in 0..=79 and fits u8
    // without truncation, so no cast suppression is needed here.
    let sp = (speed_knots / 10) as u8;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "speed_knots % 10 is 0..=9 and course_deg / 100 is 0..=3 (course clamped to 360), \
                  so the combined value is in 0..=93 and fits u8"
    )]
    let dc = ((speed_knots % 10) * 10 + course_deg / 100) as u8;
    // course_deg % 100 is in 0..100 so truncating to u8 is safe.
    let se = (course_deg % 100) as u8;

    // Build info field.
    let mut info = Vec::with_capacity(9 + comment.len());
    info.push(0x60); // Current Mic-E data type.
    info.push(d + 28);
    info.push(m + 28);
    info.push(lon_hundredths + 28);
    info.push(sp + 28);
    info.push(dc + 28);
    info.push(se + 28);
    info.push(symbol_code as u8);
    info.push(symbol_table as u8);
    info.extend_from_slice(comment.as_bytes());

    // The 6 destination bytes were assembled above from `b'0' + digit`
    // (ranges 0x30..=0x39) and `b'P' + digit` (ranges 0x50..=0x59), so
    // every byte is in the AX.25 v2.2 §3.12.2 callsign-character set
    // (uppercase ASCII alphanumeric). `Callsign::new` therefore cannot
    // fail; the `unreachable!` guards the invariant explicitly rather
    // than silently substituting a wrong destination, which would
    // corrupt the Mic-E latitude carried in the address slot.
    let dest_callsign_typed = Callsign::new(dest_callsign).unwrap_or_else(|err| {
        unreachable!("Mic-E destination digits violated their construction invariant: {err}")
    });
    let destination = Ax25Address::from_parts(dest_callsign_typed, Ssid::ZERO);
    ax25_ui_frame(source.clone(), destination, path.to_vec(), info)
}

// ---------------------------------------------------------------------------
// APRS telemetry builders (APRS 1.0.1 §13 pp.68-70)
// ---------------------------------------------------------------------------

/// Maximum sequence number for an APRS telemetry frame (§13 p.68: "the
/// data sequence number may be in the range 000 to 999").
const APRS_TELEMETRY_MAX_SEQUENCE: u16 = 999;

/// Maximum analog value per APRS 1.2 expansion (§13 p.68 + addendum:
/// "the analog values may be in the range 000 to 999"). The 1.0.1 base
/// spec uses 000-255 but addendum 1.2 widens this to 000-999; the
/// parser at [`crate::parse_aprs_telemetry`] already accepts the
/// expanded range so the builder matches.
const APRS_TELEMETRY_MAX_ANALOG: u16 = 999;

/// Maximum analog-channel label length per APRS 1.0.1 §13 p.69 (field
/// widths A1=7, A2=7, A3=6, A4=6, A5=5; the longest is 7). The
/// builder uses 7 as the conservative cap for all analog labels.
const APRS_TELEMETRY_MAX_ANALOG_LABEL: usize = 7;

/// Maximum digital-channel label length per APRS 1.0.1 §13 p.69 (B1=6,
/// B2=5, B3=4, B4=4, B5=4, B6=3, B7=3, B8=3; the longest is 6).
const APRS_TELEMETRY_MAX_DIGITAL_LABEL: usize = 6;

/// Build a KISS-encoded APRS telemetry frame.
///
/// Composes an AX.25 UI frame with the APRS telemetry format from
/// §13 p.68:
///
/// ```text
/// T#NNN,aaa,aaa,aaa,aaa,aaa,bbbbbbbb
/// ```
///
/// where `NNN` is the sequence number (000-999 or the literal `MIC`
/// for the spec's "Manual Input Command" sentinel), `aaa` is each
/// analog channel value (000-999 per addendum 1.2; base spec
/// 000-255), and `bbbbbbbb` is the digital field (exactly 8 ASCII
/// `0`/`1` characters).
///
/// `sequence` is clamped to `0..=999`. `analogs` provides up to five
/// channel values; values are clamped to `0..=999`. `digital` is the
/// 8-bit binary status word; only the low 8 bits are formatted.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame). The parser
/// counterpart is [`crate::parse_aprs_telemetry`].
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `sequence`: Sequence number `0..=999`.
/// - `analogs`: Up to five analog channel values; missing channels
///   become `000` on the wire.
/// - `digital`: 8-bit digital status word.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_telemetry(
    source: &Ax25Address,
    sequence: u16,
    analogs: [u16; 5],
    digital: u8,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_telemetry_packet(
        source, sequence, analogs, digital, path,
    ))
}

/// Like [`build_aprs_telemetry`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_telemetry_packet(
    source: &Ax25Address,
    sequence: u16,
    analogs: [u16; 5],
    digital: u8,
    path: &[RouteEntry],
) -> Ax25Packet {
    let seq = sequence.min(APRS_TELEMETRY_MAX_SEQUENCE);
    let a = analogs.map(|v| v.min(APRS_TELEMETRY_MAX_ANALOG));
    // §13 p.68: digital is exactly 8 bits, MSB first.
    let info = format!(
        "T#{seq:03},{:03},{:03},{:03},{:03},{:03},{:08b}",
        a[0], a[1], a[2], a[3], a[4], digital,
    );
    ax25_ui_frame(
        source.clone(),
        aprs_tocall(),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Build a KISS-encoded APRS telemetry **parameter-name** definition
/// message (`:DEST    :PARM.A1,A2,A3,A4,A5,B1,B2,B3,B4,B5,B6,B7,B8`).
///
/// Per APRS 1.0.1 §13 p.69, parameter-name definitions are sent as
/// regular APRS messages addressed to the originator's *own* callsign
/// (so other stations infer ownership from the source field). The
/// helper accepts a separate `addressee` to make this explicit; pass
/// the source callsign for the canonical form. Each label is
/// truncated to its spec-mandated max length per channel position.
///
/// `analog_labels` carries up to 5 entries (A1..A5); `digital_labels`
/// up to 8 (B1..B8). Missing entries become empty fields on the wire.
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `addressee`: 1..=9 char callsign the message is addressed to
///   (typically equal to the source callsign).
/// - `analog_labels`: Up to 5 channel labels.
/// - `digital_labels`: Up to 8 channel labels.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_telemetry_parm(
    source: &Ax25Address,
    addressee: &str,
    analog_labels: &[&str],
    digital_labels: &[&str],
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_telemetry_parm_packet(
        source,
        addressee,
        analog_labels,
        digital_labels,
        path,
    ))
}

/// Like [`build_aprs_telemetry_parm`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_telemetry_parm_packet(
    source: &Ax25Address,
    addressee: &str,
    analog_labels: &[&str],
    digital_labels: &[&str],
    path: &[RouteEntry],
) -> Ax25Packet {
    let body = format_telemetry_definition_body(
        "PARM.",
        analog_labels,
        digital_labels,
        APRS_TELEMETRY_MAX_ANALOG_LABEL,
        APRS_TELEMETRY_MAX_DIGITAL_LABEL,
    );
    build_aprs_message_packet(source, addressee, &body, None, path)
}

/// Build a KISS-encoded APRS telemetry **unit-label** definition message
/// (`:DEST    :UNIT.unit1,unit2,…`). See [`build_aprs_telemetry_parm`].
#[must_use]
pub fn build_aprs_telemetry_unit(
    source: &Ax25Address,
    addressee: &str,
    analog_units: &[&str],
    digital_units: &[&str],
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_telemetry_unit_packet(
        source,
        addressee,
        analog_units,
        digital_units,
        path,
    ))
}

/// Like [`build_aprs_telemetry_unit`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_telemetry_unit_packet(
    source: &Ax25Address,
    addressee: &str,
    analog_units: &[&str],
    digital_units: &[&str],
    path: &[RouteEntry],
) -> Ax25Packet {
    let body = format_telemetry_definition_body(
        "UNIT.",
        analog_units,
        digital_units,
        APRS_TELEMETRY_MAX_ANALOG_LABEL,
        APRS_TELEMETRY_MAX_DIGITAL_LABEL,
    );
    build_aprs_message_packet(source, addressee, &body, None, path)
}

/// Build a KISS-encoded APRS telemetry **equation-coefficients**
/// definition message
/// (`:DEST    :EQNS.a1,b1,c1,a2,b2,c2,a3,b3,c3,a4,b4,c4,a5,b5,c5`).
///
/// `equations` provides up to 5 `(a, b, c)` tuples for the linear
/// equation `y = a·v² + b·v + c` applied to each analog channel.
/// `None` slots emit empty fields on the wire.
#[must_use]
pub fn build_aprs_telemetry_eqns(
    source: &Ax25Address,
    addressee: &str,
    equations: [Option<(f64, f64, f64)>; 5],
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_telemetry_eqns_packet(
        source, addressee, equations, path,
    ))
}

/// Like [`build_aprs_telemetry_eqns`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_telemetry_eqns_packet(
    source: &Ax25Address,
    addressee: &str,
    equations: [Option<(f64, f64, f64)>; 5],
    path: &[RouteEntry],
) -> Ax25Packet {
    // Flatten the 5 (a, b, c) tuples to 15 individual coefficients,
    // formatting each one. Missing slots emit `0,0,0`.
    let mut coeffs: Vec<f64> = Vec::with_capacity(15);
    for slot in equations {
        let abc = slot.unwrap_or((0.0, 0.0, 0.0));
        coeffs.push(abc.0);
        coeffs.push(abc.1);
        coeffs.push(abc.2);
    }
    let formatted: Vec<String> = coeffs.iter().copied().map(format_telemetry_float).collect();
    let parts: Vec<String> = vec!["EQNS.".to_owned(), formatted.join(",")];
    let body: String = parts.concat();
    build_aprs_message_packet(source, addressee, &body, None, path)
}

/// Build a KISS-encoded APRS telemetry **bit-sense + project-name**
/// definition message (`:DEST    :BITS.11111111,Project name`).
///
/// `bit_sense` is the 8-bit polarity word: each bit indicates whether
/// the corresponding digital channel is normally `1` (set bit) or `0`
/// (clear bit). `project` is a free-form ≤23-byte title per §13 p.70.
#[must_use]
pub fn build_aprs_telemetry_bits(
    source: &Ax25Address,
    addressee: &str,
    bit_sense: u8,
    project: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_telemetry_bits_packet(
        source, addressee, bit_sense, project, path,
    ))
}

/// Like [`build_aprs_telemetry_bits`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_telemetry_bits_packet(
    source: &Ax25Address,
    addressee: &str,
    bit_sense: u8,
    project: &str,
    path: &[RouteEntry],
) -> Ax25Packet {
    let body = format!("BITS.{bit_sense:08b},{project}");
    build_aprs_message_packet(source, addressee, &body, None, path)
}

/// Format a single coefficient for the EQNS message body. APRS 1.0.1
/// §13 p.70 doesn't mandate a specific float format; we emit the
/// shortest round-trip-faithful representation (`{:?}`) trimmed of
/// trailing zeros after the decimal point.
fn format_telemetry_float(v: f64) -> String {
    // `{:?}` produces e.g. "0.5", "3.0", "1.23e10". For the common
    // case of small integer-valued coefficients we want "0" rather
    // than "0.0" to match the spec's example syntax.
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "v.fract() == 0 and abs < 1e15 implies v fits an i64 exactly"
        )]
        let i = v as i64;
        return i.to_string();
    }
    format!("{v}")
}

/// Compose a `KIND.f1,f2,…,fN` definition body where the first 5
/// fields are analog labels and the next 8 are digital labels, each
/// truncated to its spec maximum.
fn format_telemetry_definition_body(
    kind: &str,
    analog_labels: &[&str],
    digital_labels: &[&str],
    analog_max: usize,
    digital_max: usize,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(13);
    for i in 0..5 {
        let label = analog_labels.get(i).copied().unwrap_or("");
        parts.push(truncate_at_char_boundary(label, analog_max));
    }
    for i in 0..8 {
        let label = digital_labels.get(i).copied().unwrap_or("");
        parts.push(truncate_at_char_boundary(label, digital_max));
    }
    let joined: String = parts.join(",");
    format!("{kind}{joined}")
}

/// Truncate `s` to `max_bytes` while staying on a UTF-8 character
/// boundary. APRS telemetry labels are spec-restricted to printable
/// ASCII so this is normally a no-op, but the boundary check guards
/// against mid-codepoint truncation if a caller passes non-ASCII text.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("").to_owned()
}

// ---------------------------------------------------------------------------
// APRS query response builder
// ---------------------------------------------------------------------------

/// Build a position query response as a KISS-encoded APRS position report.
///
/// When a station receives a `?APRSP` or `?APRS?` query, it should respond
/// with its current position. This builds that response as a KISS frame
/// ready for transmission.
#[must_use]
pub fn build_query_response_position(
    source: &Ax25Address,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[RouteEntry],
) -> Vec<u8> {
    // A query response is just a normal position report.
    build_aprs_position_report(source, lat, lon, symbol_table, symbol_code, comment, path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ax25_codec::parse_ax25;
    use kiss_tnc::{KissCommand, decode_kiss_frame};

    use crate::item::{parse_aprs_item, parse_aprs_object};
    use crate::message::parse_aprs_message;
    use crate::mic_e::parse_mice_position;
    use crate::packet::{AprsData, parse_aprs_data};
    use crate::position::parse_aprs_position;
    use crate::weather::parse_aprs_weather_positionless;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_source() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
            .unwrap_or_else(|_| unreachable!("N0CALL-7 is statically valid"))
    }

    /// Default APRS digipeater path: WIDE1-1, WIDE2-1.
    fn default_digipeater_path() -> Vec<RouteEntry> {
        vec![
            RouteEntry::new("WIDE1", 1)
                .unwrap_or_else(|_| unreachable!("WIDE1-1 is statically valid")),
            RouteEntry::new("WIDE2", 1)
                .unwrap_or_else(|_| unreachable!("WIDE2-1 is statically valid")),
        ]
    }

    // ---- format_aprs_latitude / format_aprs_longitude ----

    #[test]
    fn format_latitude_north() {
        let s = format_aprs_latitude(49.058_333);
        // 49 degrees, 3.50 minutes North
        assert_eq!(s.len(), 8, "latitude wire field is 8 bytes");
        assert!(s.ends_with('N'), "north hemisphere should suffix 'N'");
        assert!(s.starts_with("49"), "49-degree prefix preserved");
    }

    #[test]
    fn format_latitude_south() {
        let s = format_aprs_latitude(-33.856);
        assert!(s.ends_with('S'), "south hemisphere should suffix 'S'");
        assert!(s.starts_with("33"), "33-degree prefix preserved");
    }

    #[test]
    fn format_longitude_east() {
        let s = format_aprs_longitude(151.209);
        assert_eq!(s.len(), 9, "longitude wire field is 9 bytes");
        assert!(s.ends_with('E'), "east hemisphere should suffix 'E'");
        assert!(s.starts_with("151"), "151-degree prefix preserved");
    }

    #[test]
    fn format_longitude_west() {
        let s = format_aprs_longitude(-72.029_166);
        assert!(s.ends_with('W'), "west hemisphere should suffix 'W'");
        assert!(s.starts_with("072"), "zero-padded 72-degree prefix");
    }

    #[test]
    fn format_latitude_normal_value_exact() {
        // Spec worked example: 49.058333° → 4903.50N.
        let s = format_aprs_latitude(49.058_333);
        assert_eq!(s, "4903.50N", "expected 4903.50N, got {s}");
    }

    #[test]
    fn format_latitude_carry_boundary_no_60_minutes() {
        // 33.999999° must carry to 3400.00N, never the malformed
        // 3360.00N (minutes rounding to 60.00 with no carry).
        let s = format_aprs_latitude(33.999_999);
        assert_eq!(s, "3400.00N", "expected carry to 3400.00N, got {s}");
        // 89.999999° must carry to the pole.
        let s = format_aprs_latitude(89.999_999);
        assert_eq!(s, "9000.00N", "expected carry to 9000.00N, got {s}");
    }

    #[test]
    fn format_longitude_carry_boundary_no_60_minutes() {
        // 97.999983° must carry to 09800.00, never the malformed
        // 09760.00.
        let s = format_aprs_longitude(97.999_983);
        assert_eq!(s, "09800.00E", "expected carry to 09800.00E, got {s}");
        // 179.999999° must carry to the date line.
        let s = format_aprs_longitude(179.999_999);
        assert_eq!(s, "18000.00E", "expected carry to 18000.00E, got {s}");
    }

    // ---- build_aprs_position_report ----

    #[test]
    fn build_position_report_roundtrip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_position_report(
            &source,
            49.058_333,
            -72.029_166,
            '/',
            '-',
            "Test",
            &default_digipeater_path(),
        );

        // Decode the KISS frame.
        let kiss = decode_kiss_frame(&wire)?;
        assert_eq!(
            kiss.command,
            KissCommand::Data,
            "KISS command should be data"
        );

        // Decode the AX.25 packet.
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.source.ssid, 7);
        assert_eq!(packet.destination.callsign, "APK005");
        assert_eq!(packet.destination.ssid, 0);
        assert_eq!(packet.digipeaters.len(), 2);
        let digi0 = packet.digipeaters.first().ok_or("digipeater 0 missing")?;
        let digi1 = packet.digipeaters.get(1).ok_or("digipeater 1 missing")?;
        assert_eq!(digi0.address.callsign, "WIDE1");
        assert_eq!(digi0.address.ssid, 1);
        assert_eq!(digi1.address.callsign, "WIDE2");
        assert_eq!(digi1.address.ssid, 1);
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);

        // Parse the APRS position from the info field.
        let pos = parse_aprs_position(&packet.info)?;
        assert!((pos.latitude - 49.058_333).abs() < 0.01);
        assert!((pos.longitude - (-72.029_166)).abs() < 0.01);
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '-');
        assert!(pos.comment.contains("Test"), "comment preserved");
        Ok(())
    }

    // ---- build_aprs_object ----

    #[test]
    fn build_aprs_object_with_real_timestamp() -> TestResult {
        let source = test_source();
        let wire = build_aprs_object_with_timestamp(
            &source,
            "EVENT",
            true,
            AprsTimestamp::DhmZulu {
                day: 15,
                hour: 14,
                minute: 30,
            },
            35.0,
            -97.0,
            '/',
            '-',
            "real",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let obj = parse_aprs_object(&packet.info)?;
        assert_eq!(obj.timestamp, "151430z");
        Ok(())
    }

    #[test]
    fn build_object_roundtrip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_object(
            &source,
            "TORNADO",
            true,
            49.058_333,
            -72.029_166,
            '/',
            '-',
            "Wrn",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.destination.callsign, "APK005");

        let obj = parse_aprs_object(&packet.info)?;
        assert_eq!(obj.name, "TORNADO");
        assert!(obj.live, "object is alive");
        assert!((obj.position.latitude - 49.058_333).abs() < 0.01);
        assert!((obj.position.longitude - (-72.029_166)).abs() < 0.01);
        assert_eq!(obj.position.symbol_table, '/');
        assert_eq!(obj.position.symbol_code, '-');
        assert!(obj.position.comment.contains("Wrn"), "comment preserved");
        Ok(())
    }

    #[test]
    fn build_object_killed() -> TestResult {
        let source = test_source();
        let wire = build_aprs_object(
            &source,
            "EVENT",
            false,
            35.0,
            -97.0,
            '/',
            'E',
            "Done",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let obj = parse_aprs_object(&packet.info)?;
        assert_eq!(obj.name, "EVENT");
        assert!(!obj.live, "killed object should not be live");
        Ok(())
    }

    // ---- build_aprs_message ----

    #[test]
    fn build_message_roundtrip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_message(
            &source,
            "KQ4NIT",
            "Hello 73!",
            Some("42"),
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.destination.callsign, "APK005");

        let msg = parse_aprs_message(&packet.info)?;
        assert_eq!(msg.addressee, "KQ4NIT");
        assert_eq!(msg.text, "Hello 73!");
        assert_eq!(msg.message_id, Some("42".to_string()));
        Ok(())
    }

    #[test]
    fn build_message_no_id() -> TestResult {
        let source = test_source();
        let wire = build_aprs_message(
            &source,
            "W1AW",
            "Test msg",
            None,
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let msg = parse_aprs_message(&packet.info)?;
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "Test msg");
        assert_eq!(msg.message_id, None);
        Ok(())
    }

    #[test]
    fn build_message_pads_short_addressee() -> TestResult {
        let source = test_source();
        let wire = build_aprs_message(&source, "AB", "Hi", None, &default_digipeater_path());

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        // The info field should have the addressee padded to 9 chars.
        let info_str = String::from_utf8_lossy(&packet.info);
        // Format: :ADDRESSEE:text, where addressee is bytes 1..10.
        let addressee_field = info_str.get(1..10).ok_or("addressee field missing")?;
        assert_eq!(addressee_field, "AB       ");
        Ok(())
    }

    #[test]
    fn build_aprs_message_truncates_long_text() -> TestResult {
        let source = test_source();
        let text = "X".repeat(80);
        let wire = build_aprs_message(&source, "N0CALL", &text, None, &default_digipeater_path());

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let msg = parse_aprs_message(&packet.info)?;
        assert_eq!(
            msg.text.len(),
            MAX_APRS_MESSAGE_TEXT_LEN,
            "long text should be truncated to the 67-byte spec limit",
        );
        Ok(())
    }

    #[test]
    fn build_aprs_message_checked_rejects_long_text() {
        let source = test_source();
        let text = "Y".repeat(80);
        let result =
            build_aprs_message_checked(&source, "N0CALL", &text, None, &default_digipeater_path());
        assert!(
            matches!(result, Err(AprsError::MessageTooLong(80))),
            "long text should be rejected: {result:?}",
        );
    }

    // ---- build_aprs_item ----

    #[test]
    fn build_item_live_roundtrip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_item(
            &source,
            "MARKER",
            true,
            49.058_333,
            -72.029_166,
            '/',
            '-',
            "Test item",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.destination.callsign, "APK005");

        let item = parse_aprs_item(&packet.info)?;
        assert_eq!(item.name, "MARKER");
        assert!(item.live, "item is alive");
        assert!((item.position.latitude - 49.058_333).abs() < 0.01);
        assert!((item.position.longitude - (-72.029_166)).abs() < 0.01);
        assert_eq!(item.position.symbol_table, '/');
        assert_eq!(item.position.symbol_code, '-');
        assert!(
            item.position.comment.contains("Test item"),
            "comment preserved",
        );
        Ok(())
    }

    #[test]
    fn build_item_killed() -> TestResult {
        let source = test_source();
        let wire = build_aprs_item(
            &source,
            "GONE",
            false,
            35.0,
            -97.0,
            '/',
            'E',
            "Removed",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let item = parse_aprs_item(&packet.info)?;
        assert_eq!(item.name, "GONE");
        assert!(!item.live, "killed item should not be live");
        Ok(())
    }

    // ---- build_aprs_weather ----

    #[test]
    fn build_weather_full_roundtrip() -> TestResult {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: Some(180),
            wind_speed: Some(10),
            wind_gust: Some(25),
            temperature: Some(72),
            rain_1h: Some(5),
            rain_24h: Some(50),
            rain_since_midnight: Some(100),
            humidity: Some(55),
            pressure: Some(10132),
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.destination.callsign, "APK005");

        // Parse it back.
        let parsed = parse_aprs_weather_positionless(&packet.info)?;
        assert_eq!(parsed.wind_direction, Some(180));
        assert_eq!(parsed.wind_speed, Some(10));
        assert_eq!(parsed.wind_gust, Some(25));
        assert_eq!(parsed.temperature, Some(72));
        assert_eq!(parsed.rain_1h, Some(5));
        assert_eq!(parsed.rain_24h, Some(50));
        assert_eq!(parsed.rain_since_midnight, Some(100));
        assert_eq!(parsed.humidity, Some(55));
        assert_eq!(parsed.pressure, Some(10132));
        Ok(())
    }

    #[test]
    fn build_weather_partial_fields() -> TestResult {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: None,
            wind_speed: None,
            wind_gust: None,
            temperature: Some(32),
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: None,
            humidity: None,
            pressure: Some(10200),
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        let parsed = parse_aprs_weather_positionless(&packet.info)?;
        assert_eq!(parsed.temperature, Some(32));
        assert_eq!(parsed.pressure, Some(10200));
        assert_eq!(parsed.wind_direction, None);
        assert_eq!(parsed.humidity, None);
        Ok(())
    }

    #[test]
    fn build_aprs_position_weather_roundtrip() -> TestResult {
        let wx = AprsWeather {
            wind_direction: Some(90),
            wind_speed: Some(10),
            wind_gust: Some(15),
            temperature: Some(72),
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: Some(20),
            humidity: Some(55),
            pressure: Some(10135),
        };
        let wire = build_aprs_position_weather(
            &test_source(),
            35.25,
            -97.75,
            '/',
            &wx,
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_aprs_position(&packet.info)?;
        assert_eq!(pos.symbol_code, '_');
        let weather = pos.weather.ok_or("embedded weather missing")?;
        assert_eq!(weather.wind_direction, Some(90));
        assert_eq!(weather.wind_speed, Some(10));
        assert_eq!(weather.wind_gust, Some(15));
        assert_eq!(weather.temperature, Some(72));
        assert_eq!(weather.humidity, Some(55));
        assert_eq!(weather.pressure, Some(10135));
        Ok(())
    }

    #[test]
    fn build_weather_clamps_overflowing_fields() -> TestResult {
        // Regression guard (APRS 1.0.1 §12 p.64): out-of-range weather
        // values must clamp to their spec field width before formatting.
        // Pre-fix, temperature=-100 emitted "t-100" (4 chars, overflows
        // the 3-digit field) and pressure=100000 emitted "b100000"
        // (6 chars), causing the crate's own parser to drop every
        // following field. After the clamp the downstream parse must
        // recover ALL fields with each within spec width.
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: Some(180),
            wind_speed: Some(10),
            wind_gust: Some(25),
            temperature: Some(-100), // below the -99 spec floor
            rain_1h: Some(5),
            rain_24h: Some(50),
            rain_since_midnight: Some(20),
            humidity: Some(55),
            pressure: Some(100_000), // above the 5-digit field max
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        // Inspect the raw info field: temperature must be exactly "t-99"
        // (sign + 2 digits) and pressure exactly "b99999" (5 digits).
        let info = std::str::from_utf8(&packet.info)?;
        assert!(
            info.contains("t-99"),
            "temperature must clamp to -99: {info}"
        );
        assert!(
            info.contains("b99999"),
            "pressure must clamp to 5-digit 99999: {info}",
        );

        // Every field must still round-trip; nothing dropped.
        let parsed = parse_aprs_weather_positionless(&packet.info)?;
        assert_eq!(parsed.wind_direction, Some(180));
        assert_eq!(parsed.wind_speed, Some(10));
        assert_eq!(parsed.wind_gust, Some(25));
        assert_eq!(parsed.temperature, Some(-99), "temp clamped to -99");
        assert_eq!(parsed.rain_1h, Some(5));
        assert_eq!(parsed.rain_24h, Some(50));
        assert_eq!(parsed.rain_since_midnight, Some(20));
        assert_eq!(parsed.humidity, Some(55));
        assert_eq!(parsed.pressure, Some(99_999), "pressure clamped to 99999");
        Ok(())
    }

    #[test]
    fn build_weather_humidity_zero_does_not_become_100() -> TestResult {
        // Regression guard (BUG 4): humidity Some(0) must NOT round-trip
        // to 100. Pre-fix the builder emitted 0 verbatim ("h00") and the
        // parser maps "00" → 100. Clamping to the 1..=100 spec range maps
        // 0 → 1 → "h01" → parses back to 1.
        let source = test_source();
        let wx = AprsWeather {
            humidity: Some(0),
            ..AprsWeather::default()
        };
        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let parsed = parse_aprs_weather_positionless(&packet.info)?;
        assert_eq!(
            parsed.humidity,
            Some(1),
            "humidity 0 must clamp to 1, not round-trip to 100",
        );
        Ok(())
    }

    #[test]
    fn build_weather_humidity_over_100_clamps() -> TestResult {
        // Humidity Some(150) must clamp to 100 (encoded "h00", which the
        // parser maps back to 100) rather than emitting a 3-digit field.
        let source = test_source();
        let wx = AprsWeather {
            humidity: Some(150),
            ..AprsWeather::default()
        };
        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let info = std::str::from_utf8(&packet.info)?;
        assert!(
            info.contains("h00"),
            "humidity 150 must clamp to 100 → h00: {info}"
        );
        let parsed = parse_aprs_weather_positionless(&packet.info)?;
        assert_eq!(parsed.humidity, Some(100), "humidity clamps to 100");
        Ok(())
    }

    #[test]
    fn build_weather_humidity_100_encodes_as_00() -> TestResult {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: None,
            wind_speed: None,
            wind_gust: None,
            temperature: None,
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: None,
            humidity: Some(100),
            pressure: None,
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        let parsed = parse_aprs_weather_positionless(&packet.info)?;
        // APRS encodes humidity 100% as "h00", parser converts back to 100.
        assert_eq!(parsed.humidity, Some(100));
        Ok(())
    }

    // ---- build_aprs_position_compressed ----

    #[test]
    fn build_compressed_position_round_trip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            35.3,
            -84.233,
            '/',
            '>',
            "test",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.destination.callsign, "APK005");
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);

        // Parse it back through the existing compressed parser.
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        // Compressed encoding has some rounding; check within tolerance.
        assert!((pos.latitude - 35.3).abs() < 0.01, "lat: {}", pos.latitude);
        assert!(
            (pos.longitude - (-84.233)).abs() < 0.01,
            "lon: {}",
            pos.longitude,
        );
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '>');
        assert!(pos.comment.contains("test"), "comment preserved");
        Ok(())
    }

    #[test]
    fn build_compressed_position_equator_prime_meridian() -> TestResult {
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            0.0,
            0.0,
            '/',
            '-',
            "",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        assert!(pos.latitude.abs() < 0.01, "lat: {}", pos.latitude);
        assert!(pos.longitude.abs() < 0.01, "lon: {}", pos.longitude);
        Ok(())
    }

    #[test]
    fn build_compressed_position_southern_hemisphere() -> TestResult {
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            -33.86,
            151.21,
            '/',
            '>',
            "sydney",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        assert!(
            (pos.latitude - (-33.86)).abs() < 0.01,
            "lat: {}",
            pos.latitude,
        );
        assert!(
            (pos.longitude - 151.21).abs() < 0.01,
            "lon: {}",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn base91_encoding_known_value() {
        // APRS101 example: 90 degrees latitude encodes as "!!!!".
        let encoded = encode_base91_4(0);
        assert_eq!(encoded, [b'!', b'!', b'!', b'!']);
    }

    #[test]
    fn build_compressed_position_clamps_out_of_range() -> TestResult {
        // Regression guard: out-of-range coordinates must clamp
        // rather than wrap. Pre-fix, a latitude of 200° would compute
        // 380_926 × (90 - 200) = -41_901_860 which `as u32` reinterprets
        // to ~4.25 billion, silently producing a "valid" but absurd
        // wire encoding. Post-fix the input clamps to 90.0 first.
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            200.0,  // out of range high
            -300.0, // out of range low
            '/',
            '>',
            "clamped",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        // 200 → clamped to 90; 380_926 × 0 → lat_val 0 → decodes to 90.
        assert!(
            (pos.latitude - 90.0).abs() < 0.01,
            "out-of-range lat should clamp to 90; got {}",
            pos.latitude,
        );
        // -300 → clamped to -180; 190_463 × 0 → lon_val 0 → decodes to -180.
        assert!(
            (pos.longitude - (-180.0)).abs() < 0.01,
            "out-of-range lon should clamp to -180; got {}",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn build_compressed_position_handles_nan() -> TestResult {
        // NaN / ±∞ inputs must fall through to 0.0 (the geographic
        // centre on each axis) rather than producing garbage bytes via
        // `as u32` saturation.
        let source = test_source();
        let cases: &[(f64, f64, &str)] = &[
            (f64::NAN, 12.0, "nan-lat"),
            (45.0, f64::NAN, "nan-lon"),
            (f64::INFINITY, 0.0, "inf-lat"),
            (0.0, f64::NEG_INFINITY, "neg-inf-lon"),
        ];
        for (lat_in, lon_in, label) in cases {
            let wire = build_aprs_position_compressed(
                &source,
                *lat_in,
                *lon_in,
                '/',
                '>',
                label,
                &default_digipeater_path(),
            );
            let kiss = decode_kiss_frame(&wire)?;
            let packet = parse_ax25(&kiss.data)?;
            let data = parse_aprs_data(&packet.info)?;
            let AprsData::Position(_) = data else {
                return Err(format!("{label}: expected Position, got {data:?}").into());
            };
        }
        Ok(())
    }

    // ---- build_aprs_telemetry ----

    #[test]
    fn build_telemetry_frame_round_trip() -> TestResult {
        // Builder ↔ parser symmetry: a T#NNN frame written by the
        // builder must decode back to the same channel values via
        // the existing `parse_aprs_telemetry` parser.
        let source = test_source();
        let wire = build_aprs_telemetry(
            &source,
            42,
            [100, 200, 300, 400, 500],
            0b1010_1100,
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Telemetry(t) = data else {
            return Err(format!("expected Telemetry, got {data:?}").into());
        };
        assert_eq!(t.sequence, "042");
        assert_eq!(
            t.analog,
            [Some(100), Some(200), Some(300), Some(400), Some(500)]
        );
        assert_eq!(t.digital, 0b1010_1100);
        Ok(())
    }

    #[test]
    fn build_telemetry_clamps_sequence_and_analogs() -> TestResult {
        // Sequence > 999 must clamp to 999; analog > 999 must clamp.
        // Tests the spec-mandated range enforcement on encode.
        let source = test_source();
        let wire = build_aprs_telemetry(
            &source,
            12_345,
            [9_999, 9_999, 9_999, 9_999, 9_999],
            0xFF,
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let info = std::str::from_utf8(&packet.info)?;
        assert!(
            info.starts_with("T#999,999,999,999,999,999,11111111"),
            "out-of-range inputs must clamp to spec maxima: {info}",
        );
        Ok(())
    }

    // ---- build_aprs_telemetry_parm / unit / eqns / bits ----

    #[test]
    fn build_telemetry_parm_emits_canonical_form() -> TestResult {
        let source = test_source();
        let wire = build_aprs_telemetry_parm(
            &source,
            "N0CALL-7",
            &["Vbatt", "Temp", "RSSI"],
            &["Door", "PIR"],
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Message(msg) = data else {
            return Err(format!("expected Message, got {data:?}").into());
        };
        // Spec form: PARM.A1,A2,A3,A4,A5,B1,B2,B3,B4,B5,B6,B7,B8
        // Trailing slots are empty (caller provided only 3 analog + 2 digital).
        assert_eq!(msg.text, "PARM.Vbatt,Temp,RSSI,,,Door,PIR,,,,,,");
        Ok(())
    }

    #[test]
    fn build_telemetry_eqns_emits_coefficients() -> TestResult {
        let source = test_source();
        let wire = build_aprs_telemetry_eqns(
            &source,
            "N0CALL-7",
            [
                Some((0.0, 0.1, 0.0)),
                Some((0.0, 0.5, 0.0)),
                None,
                None,
                None,
            ],
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Message(msg) = data else {
            return Err(format!("expected Message, got {data:?}").into());
        };
        assert_eq!(msg.text, "EQNS.0,0.1,0,0,0.5,0,0,0,0,0,0,0,0,0,0");
        Ok(())
    }

    #[test]
    fn build_telemetry_bits_emits_canonical_form() -> TestResult {
        let source = test_source();
        let wire = build_aprs_telemetry_bits(
            &source,
            "N0CALL-7",
            0b1010_1100,
            "Weather station",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Message(msg) = data else {
            return Err(format!("expected Message, got {data:?}").into());
        };
        assert_eq!(msg.text, "BITS.10101100,Weather station");
        Ok(())
    }

    // ---- build_aprs_status ----

    #[test]
    fn build_status_round_trip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_status(&source, "On the air in FM18", &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.destination.callsign, "APK005");

        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Status(status) = data else {
            return Err(format!("expected Status, got {data:?}").into());
        };
        assert_eq!(status.text, "On the air in FM18");
        Ok(())
    }

    #[test]
    fn build_status_empty_text() -> TestResult {
        let source = test_source();
        let wire = build_aprs_status(&source, "", &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Status(status) = data else {
            return Err(format!("expected Status, got {data:?}").into());
        };
        assert_eq!(status.text, "");
        Ok(())
    }

    #[test]
    fn build_status_info_field_format() -> TestResult {
        let source = test_source();
        let wire = build_aprs_status(&source, "Hello", &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        // Info field should be: >Hello\r
        assert_eq!(packet.info.first().copied(), Some(b'>'));
        assert_eq!(packet.info.get(1..6), Some(b"Hello".as_slice()));
        assert_eq!(packet.info.get(6).copied(), Some(b'\r'));
        Ok(())
    }

    // ---- build_aprs_mice ----

    #[test]
    fn build_mice_roundtrip_oklahoma() -> TestResult {
        // 35.258 N, 97.755 W; matches the existing parse_mice test case.
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.258,
            -97.755,
            121,
            212,
            '/',
            '>',
            "test",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        // Destination should encode the latitude.
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!((pos.latitude - 35.258).abs() < 0.02, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - (-97.755)).abs() < 0.02,
            "lon={}",
            pos.longitude,
        );
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '>');
        assert!(pos.comment.contains("test"), "comment preserved");
        Ok(())
    }

    #[test]
    fn build_mice_roundtrip_north_east() -> TestResult {
        // 51.5 N, 0.1 W (London area)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            51.5,
            -0.1,
            0,
            0,
            '/',
            '-',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!((pos.latitude - 51.5).abs() < 0.02, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - (-0.1)).abs() < 0.02,
            "lon={}",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn build_mice_roundtrip_southern_hemisphere() -> TestResult {
        // -33.86 S, 151.21 E (Sydney)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            -33.86,
            151.21,
            50,
            180,
            '/',
            '>',
            "sydney",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!(
            (pos.latitude - (-33.86)).abs() < 0.02,
            "lat={}",
            pos.latitude,
        );
        assert!(
            (pos.longitude - 151.21).abs() < 0.02,
            "lon={}",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn build_mice_speed_course_roundtrip() -> TestResult {
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -97.0,
            55,
            270,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert_eq!(pos.speed_knots, Some(55));
        assert_eq!(pos.course_degrees, Some(270));
        Ok(())
    }

    #[test]
    fn build_mice_speed_course_clamped_no_overflow() -> TestResult {
        // Regression guard (APRS 1.0.1 §10 p.52): speed > 799 kt and
        // course > 360° must clamp before the SP/DC/SE arithmetic.
        // Pre-fix, speed_knots=2280 computed SP=228, 228 + 28 = 256
        // which panics under debug overflow-checks (and emits an
        // undecodable byte > 127 in release). The build must not panic
        // and every Mic-E info byte must land in the legal 28..=127.
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -97.0,
            2280, // out-of-range high speed
            720,  // out-of-range high course
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        // info[1..7] are the d/m/hundredths/SP/DC/SE Mic-E bytes.
        for (idx, byte) in packet
            .info
            .get(1..7)
            .ok_or("info too short")?
            .iter()
            .enumerate()
        {
            assert!(
                (28..=127).contains(byte),
                "info[{}] = {byte} outside Mic-E range 28..=127 after clamp",
                idx + 1,
            );
        }
        // The clamped speed (799) and course (360) should decode back to
        // their saturated values.
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert_eq!(pos.speed_knots, Some(799), "speed should clamp to 799");
        assert_eq!(pos.course_degrees, Some(360), "course should clamp to 360");
        Ok(())
    }

    #[test]
    fn build_mice_normal_speed_course_unaffected_by_clamp() -> TestResult {
        // A normal in-range speed/course must still encode losslessly
        // after adding the clamp.
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -97.0,
            55,
            270,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert_eq!(pos.speed_knots, Some(55));
        assert_eq!(pos.course_degrees, Some(270));
        Ok(())
    }

    #[test]
    fn build_mice_zero_speed_course() -> TestResult {
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            40.0,
            -74.0,
            0,
            0,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert_eq!(pos.speed_knots, Some(0));
        // Course 0 = unknown → None in the decoder.
        assert_eq!(pos.course_degrees, None);
        Ok(())
    }

    #[test]
    fn build_mice_lon_0_to_9_sets_offset_and_high_column() -> TestResult {
        // Regression guard (APRS 1.0.1 §10 p.47): a longitude
        // in 0..10° must (a) set the offset bit on destination char 4,
        // and (b) emit an info-field d-byte in the high column 118-127
        // (`v` through `DEL`). Pre-fix the builder emitted bytes 28-37
        // and left the offset bit clear, outside the spec table.
        let source = test_source();
        // London-area longitude 0.1°W: lon_abs = 0.1, lon_deg_raw = 0,
        // expected d = 0 + 90 = 90, expected info[1] = 90 + 28 = 118 (`v`).
        let wire = build_aprs_mice(
            &source,
            51.5,
            -0.1,
            0,
            0,
            '/',
            '-',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        // info[1] is the lon-degrees d-byte. Must sit in 118..=127 for
        // a 0..10° longitude.
        let d_byte = *packet.info.get(1).ok_or("info[1] missing")?;
        assert!(
            (118..=127).contains(&d_byte),
            "lon d-byte {d_byte} outside spec column 118..=127 for 0-9°",
        );

        // Destination char 4 carries the lon-offset flag. For a
        // 0..10° longitude the offset bit must be set, so the char
        // must be in `P..=Y`.
        let dest_str = packet.destination.callsign.as_str();
        let char_4 = dest_str
            .as_bytes()
            .get(4)
            .copied()
            .ok_or("dest[4] missing")?;
        assert!(
            (b'P'..=b'Y').contains(&char_4),
            "dest char 4 {} not P-Y; lon-offset bit not set for 0-9°",
            char_4 as char,
        );

        // Round-trip the decode to confirm the parser recovers 0.1°W.
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!(
            (pos.longitude - (-0.1)).abs() < 0.02,
            "decoded lon={} should be within 0.02° of -0.1",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn build_mice_handles_non_finite_inputs() -> TestResult {
        // Regression guard: Mic-E builder must reject NaN/inf
        // via the shared `sanitize_coord` helper rather than silently
        // saturating to 0 through `as u32`. Equivalent to the
        // compressed-position out-of-range clamp guard.
        let source = test_source();
        let path = default_digipeater_path();
        let cases: &[(f64, f64, &str)] = &[
            (f64::NAN, 12.0, "nan-lat"),
            (45.0, f64::NAN, "nan-lon"),
            (f64::INFINITY, 0.0, "inf-lat"),
            (0.0, f64::NEG_INFINITY, "neg-inf-lon"),
        ];
        for (lat_in, lon_in, label) in cases {
            // Build must not panic and must produce a valid Mic-E frame.
            let wire = build_aprs_mice(&source, *lat_in, *lon_in, 0, 0, '/', '>', label, &path);
            let kiss = decode_kiss_frame(&wire)?;
            let packet = parse_ax25(&kiss.data)?;
            // Every Mic-E byte (info[1..7]) must be in the spec range
            // 28..=127 regardless of the bad input.
            for (idx, byte) in packet
                .info
                .get(1..7)
                .ok_or("info too short")?
                .iter()
                .enumerate()
            {
                assert!(
                    (28..=127).contains(byte),
                    "{label}: info[{}] = {byte} outside Mic-E range 28..=127",
                    idx + 1,
                );
            }
        }
        Ok(())
    }

    #[test]
    fn build_mice_lon_hundredths_boundary_clamped() -> TestResult {
        // Regression guard: a longitude whose minutes-fraction
        // rounds to 100 hundredths must not produce a wire byte outside
        // the spec-mandated Mic-E range of 28..=127 (APRS 1.0.1 §10.3.3).
        //
        // For lon = -97.999_983_3°:
        //   lon_abs       = 97.999_983_3
        //   lon_deg       = 97
        //   lon_min_f     = 59.998_998   (= 0.999_983_3 × 60)
        //   lon_min_int   = 59
        //   hundredths_f  = round(99.899_8) = 100  ← triggers the clamp
        //
        // Pre-fix this would emit 100 + 28 = 128 (illegal). Post-fix
        // it emits 99 + 28 = 127 ('DEL'), which is valid spec-side.
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -97.999_983_3,
            0,
            0,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;

        // info[3] is the longitude-hundredths byte. Verify it sits in
        // the valid Mic-E receivable range.
        let hundredths_byte = *packet.info.get(3).ok_or("info[3] missing")?;
        assert!(
            (28..=127).contains(&hundredths_byte),
            "lon_hundredths byte {hundredths_byte} outside spec range 28..=127",
        );

        // The decoded longitude should still be within ~0.001° of the
        // input (we lose at most one hundredth of a minute ≈ 18 m).
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!(
            (pos.longitude - (-97.999_983_3)).abs() < 0.001,
            "decoded lon={} should be within 0.001° of input",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn build_mice_high_longitude() -> TestResult {
        // 35.0 N, 140.0 E (Tokyo area)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            140.0,
            10,
            90,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!((pos.latitude - 35.0).abs() < 0.02, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - 140.0).abs() < 0.02,
            "lon={}",
            pos.longitude,
        );
        Ok(())
    }

    #[test]
    fn build_mice_with_message_roundtrip() -> TestResult {
        // Encode each standard message code, decode it back, verify.
        let cases = [
            MiceMessage::OffDuty,
            MiceMessage::EnRoute,
            MiceMessage::InService,
            MiceMessage::Returning,
            MiceMessage::Committed,
            MiceMessage::Special,
            MiceMessage::Priority,
            MiceMessage::Emergency,
        ];
        for msg in cases {
            let source = test_source();
            let wire = build_aprs_mice_with_message(
                &source,
                35.25,
                -97.75,
                10,
                90,
                msg,
                '/',
                '>',
                "",
                &default_digipeater_path(),
            );
            let kiss = decode_kiss_frame(&wire)?;
            let packet = parse_ax25(&kiss.data)?;
            let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
            assert_eq!(pos.mice_message, Some(msg), "round trip for {msg:?}");
        }
        Ok(())
    }

    #[test]
    fn build_mice_lon_100_109() -> TestResult {
        // 35.0 N, 105.5 W (New Mexico)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -105.5,
            0,
            0,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info)?;
        assert!(
            (pos.longitude - (-105.5)).abs() < 0.02,
            "lon={}",
            pos.longitude,
        );
        Ok(())
    }

    // ---- build_query_response_position ----

    #[test]
    fn build_query_response_roundtrip() -> TestResult {
        let source = test_source();
        let wire = build_query_response_position(
            &source,
            35.258,
            -97.755,
            '/',
            '>',
            "QRY resp",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let data = parse_aprs_data(&packet.info)?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        assert!((pos.latitude - 35.258).abs() < 0.01);
        assert!((pos.longitude - (-97.755)).abs() < 0.01);
        assert!(pos.comment.contains("QRY resp"), "comment preserved");
        Ok(())
    }
}
