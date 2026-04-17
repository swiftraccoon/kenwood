//! Builders for outgoing APRS info fields and wire frames.
//!
//! Each public entry point has two flavours: the top-level builder
//! returns a KISS-framed byte vector ready for transport write, and the
//! `_packet` variant returns the unencoded [`Ax25Packet`] so callers can
//! inspect, log, or route it before wrapping it in KISS framing.

use ax25_codec::{Ax25Address, Ax25Packet, build_ax25};
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

/// Build a minimal APRS UI frame with the given source, destination, path,
/// and info field. Control = 0x03, PID = 0xF0.
const fn ax25_ui_frame(
    source: Ax25Address,
    destination: Ax25Address,
    path: Vec<Ax25Address>,
    info: Vec<u8>,
) -> Ax25Packet {
    Ax25Packet {
        source,
        destination,
        digipeaters: path,
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
/// like `"950000.00N"`.
fn format_aprs_latitude(lat: f64) -> String {
    let lat = if lat.is_finite() {
        lat.clamp(-90.0, 90.0)
    } else {
        0.0
    };
    let hemisphere = if lat >= 0.0 { 'N' } else { 'S' };
    let lat_abs = lat.abs();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lat_abs is clamped to 0..=90 so the cast to u32 is safe"
    )]
    let degrees = lat_abs as u32;
    let minutes = (lat_abs - f64::from(degrees)) * 60.0;
    format!("{degrees:02}{minutes:05.2}{hemisphere}")
}

/// Format longitude as APRS uncompressed `DDDMM.HHE` (9 bytes).
///
/// Clamps out-of-range or non-finite input to `±180.0`.
fn format_aprs_longitude(lon: f64) -> String {
    let lon = if lon.is_finite() {
        lon.clamp(-180.0, 180.0)
    } else {
        0.0
    };
    let hemisphere = if lon >= 0.0 { 'E' } else { 'W' };
    let lon_abs = lon.abs();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lon_abs is clamped to 0..=180 so the cast to u32 is safe"
    )]
    let degrees = lon_abs as u32;
    let minutes = (lon_abs - f64::from(degrees)) * 60.0;
    format!("{degrees:03}{minutes:05.2}{hemisphere}")
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
) -> Ax25Packet {
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    let info = format!("!{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");
    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
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
/// **truncated** — use [`build_aprs_message_checked`] if you want a
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
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
        Ax25Address::new(APRS_TOCALL, 0),
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
    path: &[Ax25Address],
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
/// The timestamp uses the current UTC time in DHM zulu format.
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
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
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
) -> Ax25Packet {
    let live_char = if live { '!' } else { '_' };
    let lat_str = format_aprs_latitude(lat);
    let lon_str = format_aprs_longitude(lon);
    let info = format!("){name}{live_char}{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");
    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

// ---------------------------------------------------------------------------
// APRS weather builders
// ---------------------------------------------------------------------------

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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
) -> Ax25Packet {
    use std::fmt::Write as _;

    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    // Symbol code is always `_` (weather station) for this format.
    // Wind direction and speed go into the CSE/SPD slot (`DDD/SSS`),
    // with "..." for missing values.
    let wind_dir = weather
        .wind_direction
        .map_or_else(|| "...".to_owned(), |d| format!("{d:03}"));
    let wind_spd = weather
        .wind_speed
        .map_or_else(|| "...".to_owned(), |s| format!("{s:03}"));

    let mut info = format!("!{lat_str}{symbol_table}{lon_str}_{wind_dir}/{wind_spd}");
    if let Some(gust) = weather.wind_gust {
        let _ = write!(info, "g{gust:03}");
    }
    if let Some(temp) = weather.temperature {
        let _ = write!(info, "t{temp:03}");
    }
    if let Some(rain) = weather.rain_1h {
        let _ = write!(info, "r{rain:03}");
    }
    if let Some(rain) = weather.rain_24h {
        let _ = write!(info, "p{rain:03}");
    }
    if let Some(rain) = weather.rain_since_midnight {
        let _ = write!(info, "P{rain:03}");
    }
    if let Some(hum) = weather.humidity {
        let hum_val = if hum == 100 { 0 } else { hum };
        let _ = write!(info, "h{hum_val:02}");
    }
    if let Some(pres) = weather.pressure {
        let _ = write!(info, "b{pres:05}");
    }

    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Like [`build_aprs_weather`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_weather_packet(
    source: &Ax25Address,
    weather: &AprsWeather,
    path: &[Ax25Address],
) -> Ax25Packet {
    use std::fmt::Write as _;

    let mut info = String::from("_00000000");

    if let Some(dir) = weather.wind_direction {
        let _ = write!(info, "c{dir:03}");
    }
    if let Some(spd) = weather.wind_speed {
        let _ = write!(info, "s{spd:03}");
    }
    if let Some(gust) = weather.wind_gust {
        let _ = write!(info, "g{gust:03}");
    }
    if let Some(temp) = weather.temperature {
        let _ = write!(info, "t{temp:03}");
    }
    if let Some(rain) = weather.rain_1h {
        let _ = write!(info, "r{rain:03}");
    }
    if let Some(rain) = weather.rain_24h {
        let _ = write!(info, "p{rain:03}");
    }
    if let Some(rain) = weather.rain_since_midnight {
        let _ = write!(info, "P{rain:03}");
    }
    if let Some(hum) = weather.humidity {
        let hum_val = if hum == 100 { 0 } else { hum };
        let _ = write!(info, "h{hum_val:02}");
    }
    if let Some(pres) = weather.pressure {
        let _ = write!(info, "b{pres:05}");
    }

    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
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
    path: &[Ax25Address],
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

/// Like [`build_aprs_position_compressed`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_position_compressed_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "APRS compressed position encoding scales f64 decimal degrees into u32-ranged integers per APRS101 §9"
    )]
    let lat_val = (380_926.0 * (90.0 - latitude)) as u32;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "APRS compressed position encoding scales f64 decimal degrees into u32-ranged integers per APRS101 §9"
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

    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info,
    )
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
pub fn build_aprs_status(source: &Ax25Address, text: &str, path: &[Ax25Address]) -> Vec<u8> {
    ax25_to_kiss_wire(&build_aprs_status_packet(source, text, path))
}

/// Like [`build_aprs_status`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_status_packet(
    source: &Ax25Address,
    text: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    let mut info = Vec::with_capacity(1 + text.len() + 1);
    info.push(b'>');
    info.extend_from_slice(text.as_bytes());
    info.push(b'\r');
    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info,
    )
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
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
    path: &[Ax25Address],
) -> Ax25Packet {
    // Clamp position so the wire fields never overflow.
    let latitude = latitude.clamp(-90.0, 90.0);
    let longitude = longitude.clamp(-180.0, 180.0);
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
    let lon_offset = lon_abs >= 100.0;
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

    // Longitude degrees encoding per APRS 1.0.1 §10.3.3:
    //   No offset (0-99°):    d = degrees
    //   Offset set (≥100°):
    //     100-109°:           d = degrees - 20    (decoder hits 180-189 → subtract 80)
    //     110-179°:           d = degrees - 100   (decoder passes through)
    //
    // Byte on the wire is always d + 28.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "lon_abs is clamped to 0..=180 so fits u16"
    )]
    let lon_deg_raw = lon_abs as u16;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "lon_deg_raw subtraction always yields a value that fits u8 for valid APRS longitudes"
    )]
    let d = if lon_offset {
        if lon_deg_raw >= 110 {
            (lon_deg_raw - 100) as u8
        } else {
            (lon_deg_raw - 20) as u8
        }
    } else {
        lon_deg_raw as u8
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
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded value is 0..=100"
    )]
    let lon_hundredths = ((lon_min_f - f64::from(lon_min_int)) * 100.0).round() as u8;

    // Minutes encoding: if < 10, add 60.
    let m = if lon_min_int < 10 {
        lon_min_int + 60
    } else {
        lon_min_int
    };

    // Speed/course encoding per APRS101.
    // SP = speed / 10, remainder from DC.
    // DC = (speed % 10) * 10 + course / 100
    // SE = course % 100
    #[expect(
        clippy::cast_possible_truncation,
        reason = "speed_knots is u16, speed_knots / 10 fits u8 for typical APRS speeds"
    )]
    let sp = (speed_knots / 10) as u8;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "combined value stays in u8 range for valid APRS inputs"
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

    ax25_ui_frame(
        source.clone(),
        Ax25Address::new(dest_callsign, 0),
        path.to_vec(),
        info,
    )
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
    path: &[Ax25Address],
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
    use kiss_tnc::{CMD_DATA, decode_kiss_frame};

    use crate::item::{parse_aprs_item, parse_aprs_object};
    use crate::message::parse_aprs_message;
    use crate::mic_e::parse_mice_position;
    use crate::packet::{AprsData, parse_aprs_data};
    use crate::position::parse_aprs_position;
    use crate::weather::parse_aprs_weather_positionless;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_source() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
    }

    /// Default APRS digipeater path: WIDE1-1, WIDE2-1.
    fn default_digipeater_path() -> Vec<Ax25Address> {
        vec![Ax25Address::new("WIDE1", 1), Ax25Address::new("WIDE2", 1)]
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
        assert_eq!(kiss.command, CMD_DATA, "KISS command should be data");

        // Decode the AX.25 packet.
        let packet = parse_ax25(&kiss.data)?;
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.source.ssid, 7);
        assert_eq!(packet.destination.callsign, "APK005");
        assert_eq!(packet.destination.ssid, 0);
        assert_eq!(packet.digipeaters.len(), 2);
        let digi0 = packet.digipeaters.first().ok_or("digipeater 0 missing")?;
        let digi1 = packet.digipeaters.get(1).ok_or("digipeater 1 missing")?;
        assert_eq!(digi0.callsign, "WIDE1");
        assert_eq!(digi0.ssid, 1);
        assert_eq!(digi1.callsign, "WIDE2");
        assert_eq!(digi1.ssid, 1);
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
        // Format: :ADDRESSEE:text — addressee is bytes 1..10.
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
        // 35.258 N, 97.755 W — matches the existing parse_mice test case.
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
