//! Builders for outgoing APRS info fields and wire frames.
//!
//! Each public entry point has two flavours: the top-level builder
//! returns a KISS-framed byte vector ready for transport write, and the
//! `_packet` variant returns the unencoded [`Ax25Packet`] so callers can
//! inspect, log, or route it before wrapping it in KISS framing.
//!
//! Shared workflows cover [packet construction](crate#packet-construction-and-parsing),
//! [messages](crate#messages-and-transmission-state) and
//! [weather and telemetry](crate#weather-and-telemetry).

use ax25_codec::{
    Ax25Address, Ax25Header, Ax25Packet, Ax25Pid, Callsign, CommandResponse, DigipeaterPath,
    PollFinal, Ssid,
};
use kiss_tnc::KissFrame;

#[cfg(test)]
use crate::error::AprsError;
use crate::message::{BulletinAddressee, DirectMessageAddressee};
use crate::mic_e::mice_message_bits;
use crate::reports::{AprsFrameHeader, ItemReport, MiceReport, ObjectReport};
use crate::status::AprsStatusTimestamp;
use crate::telemetry::{
    AprsTelemetry, TelemetryBits, TelemetryEquationCoefficients, TelemetryLabels,
    TelemetryProjectTitle,
};
use crate::text::{
    BulletinText, CompressedPositionText, MessageAddressee, MessageText, PositionReportText,
    StatusText, TimestampedStatusText,
};
use crate::units::{AprsSymbol, Latitude, Longitude, MessageId, SymbolTable};
use crate::weather::{AprsPositionlessWeatherReport, AprsWeather};

// ---------------------------------------------------------------------------
// Private constants and helpers
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
    fn into_packet(self, information: Vec<u8>) -> Ax25Packet {
        ax25_ui_frame(
            self.source,
            Ax25Address::from_parts(self.tocall.into(), Ssid::ZERO),
            self.path,
            information,
        )
    }
}

/// Build a minimal APRS UI frame with the given source, destination, path,
/// and info field. Control = 0x03, PID = 0xF0. Marks the frame as a
/// command (APRS convention per APRS 1.0.1).
const fn ax25_ui_frame(
    source: Ax25Address,
    destination: Ax25Address,
    path: DigipeaterPath,
    info: Vec<u8>,
) -> Ax25Packet {
    Ax25Packet::unnumbered_information(
        Ax25Header {
            source,
            destination,
            digipeaters: path,
            command_response: CommandResponse::Command,
        },
        PollFinal::Clear,
        Ax25Pid::NoLayer3,
        info,
    )
}

/// Encode an [`Ax25Packet`] as a KISS-framed data frame ready for the
/// wire.
fn ax25_to_kiss_wire(packet: &Ax25Packet) -> Vec<u8> {
    KissFrame::data(packet.to_bytes()).to_bytes()
}

/// Format a validated latitude as APRS uncompressed `DDMM.HHN`.
fn format_aprs_latitude(latitude: Latitude) -> String {
    latitude.to_aprs_uncompressed()
}

/// Format a validated longitude as APRS uncompressed `DDDMM.HHE`.
fn format_aprs_longitude(longitude: Longitude) -> String {
    longitude.to_aprs_uncompressed()
}

/// Split non-negative decimal degrees into rounded degrees, minutes, and
/// hundredths of a minute, carrying a rounded `60.00` into the degree field.
fn split_degree_minutes(value: f64) -> (u16, u8, u8) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "callers pass the absolute value of a validated coordinate, whose rounded minute count fits u32"
    )]
    let total_hundredths = (value * 6_000.0).round() as u32;
    let degrees = total_hundredths / 6_000;
    let minutes = (total_hundredths / 100) % 60;
    let hundredths = total_hundredths % 100;
    (
        u16::try_from(degrees)
            .unwrap_or_else(|_| unreachable!("validated APRS coordinate degrees fit in u16")),
        u8::try_from(minutes).unwrap_or_else(|_| unreachable!("minute component is in 0..=59")),
        u8::try_from(hundredths)
            .unwrap_or_else(|_| unreachable!("hundredths component is in 0..=99")),
    )
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

impl AprsFrameHeader {
    /// Encodes checked uncompressed position fields as a KISS data frame.
    /// Consumes the header and uses its selected software identity and relay path.
    #[must_use]
    pub fn position_report(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol: AprsSymbol,
        comment: &PositionReportText,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.position_report_packet(latitude, longitude, symbol, comment))
    }

    /// Constructs the unencoded packet for [`Self::position_report`].
    #[must_use]
    pub fn position_report_packet(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol: AprsSymbol,
        comment: &PositionReportText,
    ) -> Ax25Packet {
        let lat_str = format_aprs_latitude(latitude);
        let lon_str = format_aprs_longitude(longitude);
        let info = format!(
            "!{lat_str}{}{lon_str}{}{comment}",
            symbol.table_char(),
            symbol.code_char()
        );
        self.into_packet(info.into_bytes())
    }
}

// ---------------------------------------------------------------------------
// APRS message builders
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
    /// Encodes a direct message as a KISS data frame.
    /// The destination is padded to nine bytes; the optional validated ID uses
    /// the `{ID` trailer. Bulletin destinations require [`Self::bulletin`].
    #[must_use]
    pub fn message(
        self,
        addressee: &DirectMessageAddressee,
        text: &MessageText,
        message_id: Option<&MessageId>,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.message_packet(addressee, text, message_id))
    }

    /// Constructs the unencoded packet for [`Self::message`].
    #[must_use]
    pub fn message_packet(
        self,
        addressee: &DirectMessageAddressee,
        text: &MessageText,
        message_id: Option<&MessageId>,
    ) -> Ax25Packet {
        self.message_packet_fields(
            addressee.addressee(),
            text.as_str(),
            message_id.map(MessageId::as_str),
        )
    }

    /// Encodes a bulletin or announcement as a KISS data frame.
    /// Checked destination roles preserve literal braces in [`BulletinText`];
    /// no acknowledgement trailer is appended.
    #[must_use]
    pub fn bulletin(self, addressee: &BulletinAddressee, text: &BulletinText) -> Vec<u8> {
        ax25_to_kiss_wire(&self.bulletin_packet(addressee, text))
    }

    /// Constructs the unencoded packet for [`Self::bulletin`].
    #[must_use]
    pub fn bulletin_packet(self, addressee: &BulletinAddressee, text: &BulletinText) -> Ax25Packet {
        self.message_packet_fields(addressee.addressee(), text.as_str(), None)
    }

    /// Assemble a message-format info field from values already validated by a
    /// public field type or by a structured telemetry builder.
    fn message_packet_fields(
        self,
        addressee: &MessageAddressee,
        text: &str,
        message_id: Option<&str>,
    ) -> Ax25Packet {
        let padded_addressee = format!("{:<9}", addressee.as_str());

        let info = message_id.map_or_else(
            || format!(":{padded_addressee}:{text}"),
            |id| format!(":{padded_addressee}:{text}{{{id}"),
        );

        self.into_packet(info.into_bytes())
    }
}

// ---------------------------------------------------------------------------
// APRS object builders
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
    /// Encodes a named, timestamped object as a KISS data frame.
    #[must_use]
    pub fn object(self, report: &ObjectReport) -> Vec<u8> {
        ax25_to_kiss_wire(&self.object_packet(report))
    }

    /// Constructs an object packet with a padded nine-byte name and checked
    /// live/killed marker, timestamp, position, symbol and text.
    #[must_use]
    pub fn object_packet(self, report: &ObjectReport) -> Ax25Packet {
        let padded_name = format!("{:<9}", report.name.as_str());
        let marker = if report.state.is_live() { '*' } else { '_' };
        let position = &report.position;
        let latitude = format_aprs_latitude(position.latitude);
        let longitude = format_aprs_longitude(position.longitude);
        let timestamp = report.timestamp.to_wire_string();
        let info = format!(
            ";{padded_name}{marker}{timestamp}{latitude}{}{longitude}{}{}",
            position.symbol.table_char(),
            position.symbol.code_char(),
            position.comment
        );
        self.into_packet(info.into_bytes())
    }
}

// ---------------------------------------------------------------------------
// APRS item builders
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
    /// Encodes a named item as a KISS data frame.
    #[must_use]
    pub fn item(self, report: &ItemReport) -> Vec<u8> {
        ax25_to_kiss_wire(&self.item_packet(report))
    }

    /// Constructs an item packet with its checked variable-length name,
    /// live/killed delimiter, position, symbol and trailing text.
    #[must_use]
    pub fn item_packet(self, report: &ItemReport) -> Ax25Packet {
        let marker = if report.state.is_live() { '!' } else { '_' };
        let position = &report.position;
        let latitude = format_aprs_latitude(position.latitude);
        let longitude = format_aprs_longitude(position.longitude);
        let info = format!(
            "){}{marker}{latitude}{}{longitude}{}{}",
            report.name.as_str(),
            position.symbol.table_char(),
            position.symbol.code_char(),
            position.comment
        );
        self.into_packet(info.into_bytes())
    }
}

// ---------------------------------------------------------------------------
// APRS weather builders
// ---------------------------------------------------------------------------

/// Append the shared weather tail (gust, temperature, rain, humidity,
/// pressure, and luminosity) to an info field.
///
/// Both the complete-weather-report builder
/// ([`AprsFrameHeader::position_weather_packet`]) and the positionless one
/// ([`AprsFrameHeader::weather_packet`]) emit a byte-identical tail after
/// their respective wind direction/speed prefixes. Every value arrives
/// through a private-field validated type, so formatting is exact and never
/// clamps or substitutes invalid caller input.
fn write_weather_tail(info: &mut String, weather: &AprsWeather) {
    use std::fmt::Write as _;

    let gust = weather
        .wind_gust()
        .map_or_else(|| "...".to_owned(), |value| format!("{:03}", value.value()));
    let _ = write!(info, "g{gust}");
    let temperature = weather
        .temperature()
        .map_or_else(|| "...".to_owned(), |value| format!("{:03}", value.get()));
    let _ = write!(info, "t{temperature}");
    if let Some(rain) = weather.rain_1h() {
        let _ = write!(info, "r{:03}", rain.value());
    }
    if let Some(rain) = weather.rain_24h() {
        let _ = write!(info, "p{:03}", rain.value());
    }
    if let Some(rain) = weather.rain_since_midnight() {
        let _ = write!(info, "P{:03}", rain.value());
    }
    if let Some(humidity) = weather.humidity() {
        let humidity = humidity.percent();
        let hum_val = if humidity == 100 { 0 } else { humidity };
        let _ = write!(info, "h{hum_val:02}");
    }
    if let Some(pressure) = weather.pressure() {
        let _ = write!(info, "b{:05}", pressure.tenths_hpa());
    }
    if let Some(luminosity) = weather.luminosity() {
        let watts = luminosity.watts_per_square_meter();
        if watts < 1_000 {
            let _ = write!(info, "L{watts:03}");
        } else {
            let _ = write!(info, "l{:03}", watts - 1_000);
        }
    }
}

impl AprsFrameHeader {
    /// Build a KISS-encoded positionless APRS weather report.
    ///
    /// Composes an AX.25 UI frame with the complete APRS positionless-weather
    /// format: `_MMDDHHMM` followed by any typed measurements in canonical tag
    /// order (`c`, `s`, `g`, `t`, `r`, `p`, `P`, `h`, `b`, `L`/`l`) and the
    /// validated trailing comment.
    ///
    /// Returns wire-ready bytes (FEND-delimited KISS frame).
    ///
    #[must_use]
    pub fn weather(self, report: &AprsPositionlessWeatherReport) -> Vec<u8> {
        ax25_to_kiss_wire(&self.weather_packet(report))
    }

    /// Build a combined APRS position + weather report as a single KISS
    /// frame, per APRS 1.0.1 §12.1.
    ///
    /// Uses the uncompressed position format with symbol code `_` (weather
    /// station), followed by the `DDD/SSS` CSE/SPD wind direction/speed
    /// extension, then the remaining weather fields. This is the "complete
    /// weather report" wire form used by most fixed weather stations.
    #[must_use]
    pub fn position_weather(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol_table: SymbolTable,
        weather: &AprsWeather,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.position_weather_packet(latitude, longitude, symbol_table, weather))
    }

    /// Like [`Self::position_weather`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn position_weather_packet(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol_table: SymbolTable,
        weather: &AprsWeather,
    ) -> Ax25Packet {
        let lat_str = format_aprs_latitude(latitude);
        let lon_str = format_aprs_longitude(longitude);
        // Symbol code is always `_` (weather station) for this format.
        // Wind direction and speed go into the CSE/SPD slot (`DDD/SSS`),
        // with "..." for missing values. Both are 3-digit fields (APRS
        // 1.0.1 §12 p.65). Typed inputs guarantee the exact field widths.
        let wind_dir = weather
            .wind_direction()
            .map_or_else(|| "...".to_owned(), |d| format!("{:03}", d.degrees()));
        let wind_spd = weather
            .wind_speed()
            .map_or_else(|| "...".to_owned(), |s| format!("{:03}", s.value()));

        let mut info = format!(
            "!{lat_str}{}{lon_str}_{wind_dir}/{wind_spd}",
            char::from(symbol_table.as_byte()),
        );
        write_weather_tail(&mut info, weather);

        self.into_packet(info.into_bytes())
    }

    /// Like [`Self::weather`] but returns the unencoded [`Ax25Packet`].
    #[must_use]
    pub fn weather_packet(self, report: &AprsPositionlessWeatherReport) -> Ax25Packet {
        use std::fmt::Write as _;

        let mut info = format!("_{}", report.timestamp());

        let direction = report.weather().wind_direction().map_or_else(
            || "...".to_owned(),
            |value| format!("{:03}", value.degrees()),
        );
        let _ = write!(info, "c{direction}");
        let speed = report
            .weather()
            .wind_speed()
            .map_or_else(|| "...".to_owned(), |value| format!("{:03}", value.value()));
        let _ = write!(info, "s{speed}");
        write_weather_tail(&mut info, report.weather());
        info.push_str(report.comment());

        self.into_packet(info.into_bytes())
    }
}

// ---------------------------------------------------------------------------
// APRS compressed position builder
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
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
    /// - `latitude`: Decimal degrees, positive = North, negative = South.
    /// - `longitude`: Decimal degrees, positive = East, negative = West.
    /// - `symbol`: Validated symbol code and primary, alternate or overlay selector.
    /// - `comment`: Validated text appended after the compressed position's fixed `csT` bytes.
    #[must_use]
    pub fn position_compressed(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol: AprsSymbol,
        comment: &CompressedPositionText,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.position_compressed_packet(latitude, longitude, symbol, comment))
    }

    /// Like [`Self::position_compressed`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn position_compressed_packet(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol: AprsSymbol,
        comment: &CompressedPositionText,
    ) -> Ax25Packet {
        let latitude = latitude.as_degrees();
        let longitude = longitude.as_degrees();
        // Validated coordinates bound both expressions:
        //   lat_val ∈ [0, 380_926 × 180]  = [0, 68_566_680]
        //   lon_val ∈ [0, 190_463 × 360]  = [0, 68_566_680]
        // Both fit comfortably in u32. The casts cannot truncate or
        // sign-flip because the input is non-negative by construction.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "validated latitude makes the non-negative result fit u32"
        )]
        let lat_val = (380_926.0 * (90.0 - latitude)) as u32;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "validated longitude makes the non-negative result fit u32"
        )]
        let lon_val = (190_463.0 * (longitude + 180.0)) as u32;
        let lat_encoded = encode_base91_4(lat_val);
        let lon_encoded = encode_base91_4(lon_val);

        let mut info = Vec::with_capacity(1 + 13 + comment.as_str().len());
        info.push(b'!');
        info.push(symbol.table().compressed_byte());
        info.extend_from_slice(&lat_encoded);
        info.extend_from_slice(&lon_encoded);
        info.push(symbol.code());
        info.push(b' '); // cs: no course/speed data
        info.push(b' ');
        info.push(b' '); // t: compression type = no data
        info.extend_from_slice(comment.as_str().as_bytes());

        self.into_packet(info)
    }
}

// ---------------------------------------------------------------------------
// APRS status builders
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
    /// Build a KISS-encoded APRS status report.
    ///
    /// Composes an AX.25 UI frame with the APRS status format:
    /// `>text\r`
    ///
    /// Returns wire-ready bytes (FEND-delimited KISS frame).
    ///
    #[must_use]
    pub fn status(self, text: &StatusText) -> Vec<u8> {
        ax25_to_kiss_wire(&self.status_packet(text))
    }

    /// Like [`Self::status`] but returns the unencoded [`Ax25Packet`].
    #[must_use]
    pub fn status_packet(self, text: &StatusText) -> Ax25Packet {
        let mut info = Vec::with_capacity(1 + text.as_str().len() + 1);
        info.push(b'>');
        info.extend_from_slice(text.as_str().as_bytes());
        info.push(b'\r');
        self.into_packet(info)
    }

    /// Build a KISS-encoded APRS status report with a DHM-UTC timestamp.
    ///
    /// Composes the APRS timestamped-status format `>DDHHMMztext\r`.
    /// [`AprsStatusTimestamp`] makes the status-only DHM-UTC constraint explicit,
    /// while [`TimestampedStatusText`] enforces the remaining 55-byte text limit.
    ///
    /// Returns wire-ready bytes (FEND-delimited KISS frame).
    #[must_use]
    pub fn timestamped_status(
        self,
        timestamp: AprsStatusTimestamp,
        text: &TimestampedStatusText,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.timestamped_status_packet(timestamp, text))
    }

    /// Like [`Self::timestamped_status`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn timestamped_status_packet(
        self,
        timestamp: AprsStatusTimestamp,
        text: &TimestampedStatusText,
    ) -> Ax25Packet {
        let timestamp_wire = timestamp.to_wire_string();
        let mut info = Vec::with_capacity(1 + timestamp_wire.len() + text.as_str().len() + 1);
        info.push(b'>');
        info.extend_from_slice(timestamp_wire.as_bytes());
        info.extend_from_slice(text.as_str().as_bytes());
        info.push(b'\r');
        self.into_packet(info)
    }
}

// ---------------------------------------------------------------------------
// Mic-E builders (APRS101 Chapter 10)
// ---------------------------------------------------------------------------

impl MiceReport {
    /// Encodes a checked Mic-E report as a KISS data frame.
    /// Source and relay-path ownership transfer into the intermediate packet.
    #[must_use]
    pub fn to_wire(&self, source: Ax25Address, path: DigipeaterPath) -> Vec<u8> {
        ax25_to_kiss_wire(&self.to_packet(source, path))
    }

    /// Constructs the Mic-E UI packet with its six destination bytes carrying
    /// latitude, hemisphere, longitude offset and the selected standard message.
    /// The information field carries longitude, motion, symbol and status text.
    /// Uses destination SSID zero and the supplied conventional relay path.
    #[must_use]
    pub fn to_packet(&self, source: Ax25Address, path: DigipeaterPath) -> Ax25Packet {
        let Self {
            latitude,
            longitude,
            speed,
            course,
            message,
            symbol,
            status_text,
        } = self;
        let latitude = latitude.as_degrees();
        let longitude = longitude.as_degrees();
        let north = latitude >= 0.0;
        let west = longitude < 0.0;
        let lat_abs = latitude.abs();
        let lon_abs = longitude.abs();

        let (lat_deg, lat_min, lat_hundredths) = split_degree_minutes(lat_abs);
        let (lon_deg_raw, lon_min_int, lon_hundredths) = split_degree_minutes(lon_abs);

        // All digit casts are safe: the u32 values are bounded to 0..=9 (or
        // 0..=99 for hundredths) by the division/min chains above.
        let d0 = (lat_deg / 10).min(9) as u8;
        let d1 = (lat_deg % 10) as u8;
        let d2 = lat_min / 10;
        let d3 = lat_min % 10;
        let d4 = lat_hundredths / 10;
        let d5 = lat_hundredths % 10;

        // Message bits (A, B, C) from the 3-bit index. Per APRS 1.0.1 §10.1
        // Table 10, bit = 1 (Std1, uppercase P-Y range) when set.
        let (msg_a, msg_b, msg_c) = mice_message_bits(*message);

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
        let lon_offset = !(10..100).contains(&lon_deg_raw);
        let dest_chars: [u8; 6] = [
            if msg_a { b'P' + d0 } else { b'0' + d0 },
            if msg_b { b'P' + d1 } else { b'0' + d1 },
            if msg_c { b'P' + d2 } else { b'0' + d2 },
            if north { b'P' + d3 } else { b'0' + d3 },
            if lon_offset { b'P' + d4 } else { b'0' + d4 },
            if west { b'P' + d5 } else { b'0' + d5 },
        ];
        // Standard message bits select P-Y for one and 0-9 for zero;
        // every destination byte is uppercase ASCII alphanumeric.
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
        let speed_knots = speed.as_knots();
        let course_deg = course.as_degrees();
        // Validated `speed_knots / 10` is in 0..=79 and fits u8
        // without truncation, so no cast suppression is needed here.
        let sp = u8::try_from(speed_knots / 10)
            .unwrap_or_else(|_| unreachable!("validated Mic-E speed tens fit in u8"));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "speed_knots % 10 is 0..=9 and course_deg / 100 is 0..=3 (course validated through 360), \
                  so the combined value is in 0..=93 and fits u8"
        )]
        let dc = ((speed_knots % 10) * 10 + course_deg / 100) as u8;
        // course_deg % 100 is in 0..100 so truncating to u8 is safe.
        let se = (course_deg % 100) as u8;

        // Build info field.
        let mut info = Vec::with_capacity(9 + status_text.as_str().len());
        info.push(0x60); // Current Mic-E data type.
        info.push(d + 28);
        info.push(m + 28);
        info.push(lon_hundredths + 28);
        info.push(sp + 28);
        info.push(dc + 28);
        info.push(se + 28);
        info.push(symbol.code());
        info.push(symbol.table_byte());
        info.extend_from_slice(status_text.as_str().as_bytes());

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
        ax25_ui_frame(source, destination, path, info)
    }
}

// ---------------------------------------------------------------------------
// APRS telemetry builders (APRS 1.0.1 §13 pp.68-70)
// ---------------------------------------------------------------------------

impl AprsFrameHeader {
    /// Encode a checked APRS telemetry report as a KISS data frame.
    ///
    /// Composes an AX.25 UI frame with the APRS telemetry format from
    /// §13 p.68:
    ///
    /// ```text
    /// T#NNN,aaa,aaa,aaa,aaa,aaa,bbbbbbbbcomment
    /// ```
    ///
    /// where `NNN` is a numeric sequence or the literal `MIC`, `aaa` is each
    /// analog channel value, `bbbbbbbb` is the digital field (exactly eight
    /// ASCII `0`/`1` bytes), and `comment` is optional printable ASCII.
    ///
    /// APRS 1.0.1 defines analog readings through 255. The APRS 1.2 proposal
    /// describes using the full three-digit range through 999; this crate retains
    /// that existing compatibility. Typed inputs reject values over 999 instead
    /// of silently clamping them.
    ///
    /// Returns wire-ready bytes (FEND-delimited KISS frame). The parser
    /// counterpart is [`crate::AprsTelemetry::from_bytes`].
    ///
    #[must_use]
    pub fn telemetry(self, report: &AprsTelemetry) -> Vec<u8> {
        ax25_to_kiss_wire(&self.telemetry_packet(report))
    }

    /// Like [`Self::telemetry`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn telemetry_packet(self, report: &AprsTelemetry) -> Ax25Packet {
        let [analog_1, analog_2, analog_3, analog_4, analog_5] = report.analog();
        let sequence = report.sequence();
        let digital = report.digital();
        // §13 p.68: digital is exactly 8 bits, MSB first.
        let mut info = format!(
            "T#{sequence},{analog_1},{analog_2},{analog_3},{analog_4},{analog_5},{digital}",
        );
        if let Some(comment) = report.comment() {
            info.push_str(comment.as_str());
        }
        self.into_packet(info.into_bytes())
    }

    /// Build a KISS-encoded APRS telemetry **parameter-name** definition
    /// message (`:DEST    :PARM.A1,A2,A3,A4,A5,B1,B2,B3,B4,B5,B6,B7,B8`).
    ///
    /// Per APRS 1.0.1 §13 p.69, parameter-name definitions are sent as
    /// regular APRS messages addressed to the originator's *own* callsign
    /// (so other stations infer ownership from the source field). The
    /// helper accepts a separate `addressee` to make this explicit; pass
    /// the source callsign for the canonical form. [`TelemetryLabels`]
    /// preserves an exact A1-through-B8 prefix and validates every field's
    /// position-specific width without truncation or synthesized trailing fields.
    ///
    #[must_use]
    pub fn telemetry_parm(
        self,
        addressee: &DirectMessageAddressee,
        labels: &TelemetryLabels,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.telemetry_parm_packet(addressee, labels))
    }

    /// Like [`Self::telemetry_parm`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn telemetry_parm_packet(
        self,
        addressee: &DirectMessageAddressee,
        labels: &TelemetryLabels,
    ) -> Ax25Packet {
        let body = format!("PARM.{labels}");
        self.message_packet_fields(addressee.addressee(), &body, None)
    }

    /// Build a KISS-encoded APRS telemetry **unit-label** definition message
    /// (`:DEST    :UNIT.unit1,unit2,…`). See [`Self::telemetry_parm`].
    #[must_use]
    pub fn telemetry_unit(
        self,
        addressee: &DirectMessageAddressee,
        labels: &TelemetryLabels,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.telemetry_unit_packet(addressee, labels))
    }

    /// Like [`Self::telemetry_unit`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn telemetry_unit_packet(
        self,
        addressee: &DirectMessageAddressee,
        labels: &TelemetryLabels,
    ) -> Ax25Packet {
        let body = format!("UNIT.{labels}");
        self.message_packet_fields(addressee.addressee(), &body, None)
    }

    /// Build a KISS-encoded APRS telemetry **equation-coefficients**
    /// definition message
    /// (`:DEST    :EQNS.a1,b1,c1,a2,b2,c2,a3,b3,c3,a4,b4,c4,a5,b5,c5`).
    ///
    /// `coefficients` is an exact validated prefix in A1 `(a,b,c)` through A5
    /// order. APRS permits the list to stop after any field, so omitted values are
    /// not replaced with zeroes.
    #[must_use]
    pub fn telemetry_eqns(
        self,
        addressee: &DirectMessageAddressee,
        coefficients: &TelemetryEquationCoefficients,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.telemetry_eqns_packet(addressee, coefficients))
    }

    /// Like [`Self::telemetry_eqns`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn telemetry_eqns_packet(
        self,
        addressee: &DirectMessageAddressee,
        coefficients: &TelemetryEquationCoefficients,
    ) -> Ax25Packet {
        let body = format!("EQNS.{coefficients}");
        self.message_packet_fields(addressee.addressee(), &body, None)
    }

    /// Build a KISS-encoded APRS telemetry **bit-sense + project-name**
    /// definition message (`:DEST    :BITS.11111111,Project name`).
    ///
    /// `bit_sense` is the 8-bit polarity word: each bit indicates whether
    /// the corresponding digital channel is normally `1` (set bit) or `0`
    /// (clear bit). `project` is a validated zero-to-23-byte title per §13 p.70.
    #[must_use]
    pub fn telemetry_bits(
        self,
        addressee: &DirectMessageAddressee,
        bit_sense: TelemetryBits,
        project: &TelemetryProjectTitle,
    ) -> Vec<u8> {
        ax25_to_kiss_wire(&self.telemetry_bits_packet(addressee, bit_sense, project))
    }

    /// Like [`Self::telemetry_bits`] but returns the unencoded
    /// [`Ax25Packet`].
    #[must_use]
    pub fn telemetry_bits_packet(
        self,
        addressee: &DirectMessageAddressee,
        bit_sense: TelemetryBits,
        project: &TelemetryProjectTitle,
    ) -> Ax25Packet {
        let body = format!("BITS.{bit_sense},{project}");
        self.message_packet_fields(addressee.addressee(), &body, None)
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
    pub fn query_response_position(
        self,
        latitude: Latitude,
        longitude: Longitude,
        symbol: AprsSymbol,
        comment: &PositionReportText,
    ) -> Vec<u8> {
        // A query response is just a normal position report.
        self.position_report(latitude, longitude, symbol, comment)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ax25_codec::RouteEntry;
    use kiss_tnc::KissCommand;

    use crate::item::{AprsItem, AprsObject, ReportState};
    use crate::message::AprsMessage;
    use crate::mic_e::MiceMessage;
    use crate::packet::{AprsData, AprsReportTimestamp, AprsWeatherTimestamp};
    use crate::position::AprsPosition;
    use crate::reports::PositionReport;
    use crate::telemetry::{TelemetryAnalogValue, TelemetryComment, TelemetrySequence};
    use crate::text::{ItemName, MiceStatusText, ObjectName, WeatherComment};
    use crate::units::{Course, Fahrenheit, MiceSpeed, Speed, Tocall};
    use crate::weather::{
        BarometricPressure, Humidity, Luminosity, ThreeDigitWeatherValue, WindDirection,
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn header(source: Ax25Address, path: DigipeaterPath) -> AprsFrameHeader {
        AprsFrameHeader::new(source, Tocall::th_d75(), path)
    }

    fn test_source() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
            .unwrap_or_else(|_| unreachable!("N0CALL-7 is statically valid"))
    }

    fn latitude(degrees: f64) -> Latitude {
        Latitude::new(degrees)
            .unwrap_or_else(|_| unreachable!("test fixture latitude is statically valid"))
    }

    fn longitude(degrees: f64) -> Longitude {
        Longitude::new(degrees)
            .unwrap_or_else(|_| unreachable!("test fixture longitude is statically valid"))
    }

    fn symbol(table: char, code: char) -> AprsSymbol {
        AprsSymbol::from_chars(table, code)
            .unwrap_or_else(|_| unreachable!("test fixture symbol is statically valid"))
    }

    fn mice_speed(knots: u16) -> MiceSpeed {
        MiceSpeed::new(knots)
            .unwrap_or_else(|_| unreachable!("test fixture Mic-E speed is statically valid"))
    }

    fn course(degrees: u16) -> Course {
        Course::new(degrees)
            .unwrap_or_else(|_| unreachable!("test fixture course is statically valid"))
    }

    fn item_name(value: &str) -> ItemName {
        ItemName::new(value)
            .unwrap_or_else(|_| unreachable!("test fixture item name is statically valid"))
    }

    fn position_text(value: &str) -> PositionReportText {
        PositionReportText::new(value)
            .unwrap_or_else(|_| unreachable!("test fixture position text is statically valid"))
    }

    fn compressed_text(value: &str) -> CompressedPositionText {
        CompressedPositionText::new(value).unwrap_or_else(|_| {
            unreachable!("test fixture compressed-position text is statically valid")
        })
    }

    fn mice_status_text(value: &str) -> MiceStatusText {
        MiceStatusText::new(value)
            .unwrap_or_else(|_| unreachable!("test fixture Mic-E status text is statically valid"))
    }

    /// Default APRS digipeater path: WIDE1-1, WIDE2-1.
    fn default_digipeater_path() -> DigipeaterPath {
        let entries = vec![
            RouteEntry::new("WIDE1", 1)
                .unwrap_or_else(|_| unreachable!("WIDE1-1 is statically valid")),
            RouteEntry::new("WIDE2", 1)
                .unwrap_or_else(|_| unreachable!("WIDE2-1 is statically valid")),
        ];
        DigipeaterPath::new(entries)
            .unwrap_or_else(|_| unreachable!("two entries fit in an AX.25 path"))
    }

    fn weather_timestamp() -> Result<AprsWeatherTimestamp, AprsError> {
        AprsWeatherTimestamp::month_day_hour_minute_utc(10, 9, 23, 45)
    }

    fn populated_weather() -> TestResult<AprsWeather> {
        let mut weather = AprsWeather::new();
        weather.set_wind_direction(Some(WindDirection::new(180)?));
        weather.set_wind_speed(Some(ThreeDigitWeatherValue::new(10)?));
        weather.set_wind_gust(Some(ThreeDigitWeatherValue::new(25)?));
        weather.set_temperature(Some(Fahrenheit::new(72)?));
        weather.set_rain_1h(Some(ThreeDigitWeatherValue::new(5)?));
        weather.set_rain_24h(Some(ThreeDigitWeatherValue::new(50)?));
        weather.set_rain_since_midnight(Some(ThreeDigitWeatherValue::new(100)?));
        weather.set_humidity(Some(Humidity::new(55)?));
        weather.set_pressure(Some(BarometricPressure::new(10_132)?));
        weather.set_luminosity(Some(Luminosity::new(1_234)?));
        Ok(weather)
    }

    // ---- format_aprs_latitude / format_aprs_longitude ----

    #[test]
    fn format_latitude_north() {
        let s = format_aprs_latitude(latitude(49.058_333));
        // 49 degrees, 3.50 minutes North
        assert_eq!(s.len(), 8, "latitude wire field is 8 bytes");
        assert!(s.ends_with('N'), "north hemisphere should suffix 'N'");
        assert!(s.starts_with("49"), "49-degree prefix preserved");
    }

    #[test]
    fn format_latitude_south() {
        let s = format_aprs_latitude(latitude(-33.856));
        assert!(s.ends_with('S'), "south hemisphere should suffix 'S'");
        assert!(s.starts_with("33"), "33-degree prefix preserved");
    }

    #[test]
    fn format_longitude_east() {
        let s = format_aprs_longitude(longitude(151.209));
        assert_eq!(s.len(), 9, "longitude wire field is 9 bytes");
        assert!(s.ends_with('E'), "east hemisphere should suffix 'E'");
        assert!(s.starts_with("151"), "151-degree prefix preserved");
    }

    #[test]
    fn format_longitude_west() {
        let s = format_aprs_longitude(longitude(-72.029_166));
        assert!(s.ends_with('W'), "west hemisphere should suffix 'W'");
        assert!(s.starts_with("072"), "zero-padded 72-degree prefix");
    }

    #[test]
    fn format_latitude_normal_value_exact() {
        // Spec worked example: 49.058333° → 4903.50N.
        let s = format_aprs_latitude(latitude(49.058_333));
        assert_eq!(s, "4903.50N", "expected 4903.50N, got {s}");
    }

    #[test]
    fn format_latitude_carry_boundary_no_60_minutes() {
        // 33.999999° must carry to 3400.00N, never the malformed
        // 3360.00N (minutes rounding to 60.00 with no carry).
        let s = format_aprs_latitude(latitude(33.999_999));
        assert_eq!(s, "3400.00N", "expected carry to 3400.00N, got {s}");
        // 89.999999° must carry to the pole.
        let s = format_aprs_latitude(latitude(89.999_999));
        assert_eq!(s, "9000.00N", "expected carry to 9000.00N, got {s}");
    }

    #[test]
    fn format_longitude_carry_boundary_no_60_minutes() {
        // 97.999983° must carry to 09800.00, never the malformed
        // 09760.00.
        let s = format_aprs_longitude(longitude(97.999_983));
        assert_eq!(s, "09800.00E", "expected carry to 09800.00E, got {s}");
        // 179.999999° must carry to the date line.
        let s = format_aprs_longitude(longitude(179.999_999));
        assert_eq!(s, "18000.00E", "expected carry to 18000.00E, got {s}");
    }

    // ---- build_aprs_position_report ----

    #[test]
    fn build_position_report_roundtrip() -> TestResult {
        let source = test_source();
        let wire = header(source, default_digipeater_path()).position_report(
            latitude(49.058_333),
            longitude(-72.029_166),
            AprsSymbol::HOUSE,
            &position_text("Test"),
        );

        // Decode the KISS frame.
        let kiss = KissFrame::from_bytes(&wire)?;
        assert_eq!(
            kiss.command,
            KissCommand::Data,
            "KISS command should be data"
        );

        // Decode the AX.25 packet.
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.source.callsign, "N0CALL");
        assert_eq!(packet.header.source.ssid, 7);
        assert_eq!(packet.header.destination.callsign, "APK005");
        assert_eq!(packet.header.destination.ssid, 0);
        assert_eq!(packet.header.digipeaters.len(), 2);
        let digi0 = packet
            .header
            .digipeaters
            .first()
            .ok_or("digipeater 0 missing")?;
        let digi1 = packet
            .header
            .digipeaters
            .get(1)
            .ok_or("digipeater 1 missing")?;
        assert_eq!(digi0.address.callsign, "WIDE1");
        assert_eq!(digi0.address.ssid, 1);
        assert_eq!(digi1.address.callsign, "WIDE2");
        assert_eq!(digi1.address.ssid, 1);
        assert_eq!(packet.control_byte(), 0x03);
        assert_eq!(packet.protocol_identifier(), Some(Ax25Pid::NoLayer3));

        // Parse the APRS position from the info field.
        let pos = AprsPosition::from_bytes(packet.information())?;
        assert!((pos.latitude().as_degrees() - 49.058_333).abs() < 0.01);
        assert!((pos.longitude().as_degrees() - (-72.029_166)).abs() < 0.01);
        assert_eq!(pos.symbol().table_char(), '/');
        assert_eq!(pos.symbol().code_char(), '-');
        assert!(pos.comment().contains("Test"), "comment preserved");
        Ok(())
    }

    // ---- build_aprs_object ----

    #[test]
    fn build_object_emits_supplied_timestamp() -> TestResult {
        let source = test_source();
        let name = ObjectName::new("EVENT")?;
        let report = ObjectReport::new(
            name,
            ReportState::Live,
            AprsReportTimestamp::day_hour_minute_utc(15, 14, 30)?,
            PositionReport::new(
                Latitude::new(35.0)?,
                Longitude::new(-97.0)?,
                AprsSymbol::from_chars('/', '-')?,
                position_text("real"),
            ),
        );
        let wire = header(source, default_digipeater_path()).object(&report);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let obj = AprsObject::from_bytes(packet.information())?;
        assert_eq!(obj.timestamp().to_wire_string(), "151430z");
        Ok(())
    }

    #[test]
    fn build_object_roundtrip() -> TestResult {
        let source = test_source();
        let name = ObjectName::new("TORNADO")?;
        let report = ObjectReport::new(
            name,
            ReportState::Live,
            AprsReportTimestamp::day_hour_minute_utc(15, 14, 30)?,
            PositionReport::new(
                Latitude::new(49.058_333)?,
                Longitude::new(-72.029_166)?,
                AprsSymbol::from_chars('/', '-')?,
                position_text("Wrn"),
            ),
        );
        let wire = header(source, default_digipeater_path()).object(&report);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.destination.callsign, "APK005");

        let obj = AprsObject::from_bytes(packet.information())?;
        assert_eq!(obj.name().as_str(), "TORNADO");
        assert_eq!(obj.state(), ReportState::Live);
        assert!((obj.position().latitude().as_degrees() - 49.058_333).abs() < 0.01);
        assert!((obj.position().longitude().as_degrees() - (-72.029_166)).abs() < 0.01);
        assert_eq!(obj.position().symbol().table_char(), '/');
        assert_eq!(obj.position().symbol().code_char(), '-');
        assert!(
            obj.position().comment().contains("Wrn"),
            "comment preserved"
        );
        Ok(())
    }

    #[test]
    fn build_object_killed() -> TestResult {
        let source = test_source();
        let name = ObjectName::new("EVENT")?;
        let report = ObjectReport::new(
            name,
            ReportState::Killed,
            AprsReportTimestamp::day_hour_minute_utc(15, 14, 30)?,
            PositionReport::new(
                Latitude::new(35.0)?,
                Longitude::new(-97.0)?,
                AprsSymbol::from_chars('/', 'E')?,
                position_text("Done"),
            ),
        );
        let wire = header(source, default_digipeater_path()).object(&report);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let obj = AprsObject::from_bytes(packet.information())?;
        assert_eq!(obj.name().as_str(), "EVENT");
        assert_eq!(obj.state(), ReportState::Killed);
        Ok(())
    }

    #[test]
    fn maximum_position_text_fits_object_information_field_exactly() -> TestResult {
        let text = PositionReportText::new(&"x".repeat(PositionReportText::MAX_LEN))?;
        let report = ObjectReport::new(
            ObjectName::new("OBJECT")?,
            ReportState::Live,
            AprsReportTimestamp::day_hour_minute_utc(15, 14, 30)?,
            PositionReport::new(
                Latitude::new(35.0)?,
                Longitude::new(-97.0)?,
                AprsSymbol::CAR,
                text.clone(),
            ),
        );
        let packet = header(test_source(), DigipeaterPath::empty()).object_packet(&report);

        assert_eq!(packet.information().len(), 37 + PositionReportText::MAX_LEN);
        assert!(packet.information().ends_with(text.as_str().as_bytes()));
        Ok(())
    }

    // ---- build_aprs_message ----

    #[test]
    fn build_message_roundtrip() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("KQ4NIT")?;
        let text = MessageText::new("Hello 73!")?;
        let message_id = MessageId::new("42")?;
        let wire =
            header(source, default_digipeater_path()).message(&addressee, &text, Some(&message_id));

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.destination.callsign, "APK005");

        let msg = AprsMessage::from_bytes(packet.information())?;
        assert_eq!(msg.addressee().as_str(), "KQ4NIT");
        assert_eq!(msg.text(), "Hello 73!");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("42"));
        Ok(())
    }

    #[test]
    fn build_message_no_id() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("W1AW")?;
        let text = MessageText::new("Test msg")?;
        let wire = header(source, default_digipeater_path()).message(&addressee, &text, None);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let msg = AprsMessage::from_bytes(packet.information())?;
        assert_eq!(msg.addressee().as_str(), "W1AW");
        assert_eq!(msg.text(), "Test msg");
        assert_eq!(msg.message_id(), None);
        Ok(())
    }

    #[test]
    fn build_bulletin_preserves_brace_as_body_text() -> TestResult {
        let packet = header(test_source(), DigipeaterPath::empty()).bulletin_packet(
            &BulletinAddressee::new("NWS-WARN")?,
            &BulletinText::new("AR_ASHLEY,{S9JbA")?,
        );

        assert_eq!(packet.information(), b":NWS-WARN :AR_ASHLEY,{S9JbA");
        Ok(())
    }

    #[test]
    fn build_message_pads_short_addressee() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("AB")?;
        let text = MessageText::new("Hi")?;
        let wire = header(source, default_digipeater_path()).message(&addressee, &text, None);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        // The info field should have the addressee padded to 9 chars.
        let info_str = String::from_utf8_lossy(packet.information());
        // Format: :ADDRESSEE:text, where addressee is bytes 1..10.
        let addressee_field = info_str.get(1..10).ok_or("addressee field missing")?;
        assert_eq!(addressee_field, "AB       ");
        Ok(())
    }

    #[test]
    fn build_message_emits_maximum_length_text() -> TestResult {
        let source = test_source();
        let body = "X".repeat(MessageText::MAX_LEN);
        let addressee = DirectMessageAddressee::new("N0CALL")?;
        let text = MessageText::new(&body)?;
        let wire = header(source, default_digipeater_path()).message(&addressee, &text, None);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let msg = AprsMessage::from_bytes(packet.information())?;
        assert_eq!(
            msg.text(),
            body,
            "the full validated 67-byte message body should be emitted",
        );
        Ok(())
    }

    // ---- build_aprs_item ----

    #[test]
    fn build_item_live_roundtrip() -> TestResult {
        let source = test_source();
        let report = ItemReport::new(
            item_name("MARKER"),
            ReportState::Live,
            PositionReport::new(
                latitude(49.058_333),
                longitude(-72.029_166),
                AprsSymbol::HOUSE,
                position_text("Test item"),
            ),
        );
        let wire = header(source, default_digipeater_path()).item(&report);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.destination.callsign, "APK005");

        let item = AprsItem::from_bytes(packet.information())?;
        assert_eq!(item.name().as_str(), "MARKER");
        assert_eq!(item.state(), ReportState::Live);
        assert!((item.position().latitude().as_degrees() - 49.058_333).abs() < 0.01);
        assert!((item.position().longitude().as_degrees() - (-72.029_166)).abs() < 0.01);
        assert_eq!(item.position().symbol().table_char(), '/');
        assert_eq!(item.position().symbol().code_char(), '-');
        assert!(
            item.position().comment().contains("Test item"),
            "comment preserved",
        );
        Ok(())
    }

    #[test]
    fn build_item_killed() -> TestResult {
        let source = test_source();
        let report = ItemReport::new(
            item_name("GONE"),
            ReportState::Killed,
            PositionReport::new(
                latitude(35.0),
                longitude(-97.0),
                symbol('/', 'E'),
                position_text("Removed"),
            ),
        );
        let wire = header(source, default_digipeater_path()).item(&report);

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let item = AprsItem::from_bytes(packet.information())?;
        assert_eq!(item.name().as_str(), "GONE");
        assert_eq!(item.state(), ReportState::Killed);
        Ok(())
    }

    // ---- build_aprs_weather ----

    #[test]
    fn build_weather_full_roundtrip() -> TestResult {
        let source = test_source();
        let wx = populated_weather()?;
        let timestamp = weather_timestamp()?;
        let report = AprsPositionlessWeatherReport::with_comment(
            timestamp,
            wx.clone(),
            WeatherComment::new("Davis VP2")?,
        );
        let wire = header(source, default_digipeater_path()).weather(&report);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.destination.callsign, "APK005");
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            "_10092345c180s010g025t072r005p050P100h55b10132l234Davis VP2",
        );

        let parsed = AprsPositionlessWeatherReport::from_bytes(packet.information())?;
        assert_eq!(parsed.timestamp(), timestamp);
        assert_eq!(parsed.weather(), &wx);
        assert_eq!(parsed.comment(), "Davis VP2");
        Ok(())
    }

    #[test]
    fn build_weather_partial_fields() -> TestResult {
        let source = test_source();
        let mut wx = AprsWeather::new();
        wx.set_temperature(Some(Fahrenheit::new(32)?));
        wx.set_pressure(Some(BarometricPressure::new(10_200)?));
        let timestamp = weather_timestamp()?;
        let report = AprsPositionlessWeatherReport::new(timestamp, wx.clone());
        let wire = header(source, default_digipeater_path()).weather(&report);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            "_10092345c...s...g...t032b10200",
        );

        let parsed = AprsPositionlessWeatherReport::from_bytes(packet.information())?;
        assert_eq!(parsed.timestamp(), timestamp);
        assert_eq!(parsed.weather(), &wx);
        assert_eq!(parsed.comment(), "");
        Ok(())
    }

    #[test]
    fn build_aprs_position_weather_roundtrip() -> TestResult {
        let mut wx = AprsWeather::new();
        wx.set_wind_direction(Some(WindDirection::new(90)?));
        wx.set_wind_speed(Some(ThreeDigitWeatherValue::new(10)?));
        wx.set_wind_gust(Some(ThreeDigitWeatherValue::new(15)?));
        wx.set_temperature(Some(Fahrenheit::new(72)?));
        wx.set_rain_since_midnight(Some(ThreeDigitWeatherValue::new(20)?));
        wx.set_humidity(Some(Humidity::new(55)?));
        wx.set_pressure(Some(BarometricPressure::new(10_135)?));
        wx.set_luminosity(Some(Luminosity::new(875)?));
        let wire = header(test_source(), default_digipeater_path()).position_weather(
            Latitude::new(35.25)?,
            Longitude::new(-97.75)?,
            SymbolTable::Primary,
            &wx,
        );
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            "!3515.00N/09745.00W_090/010g015t072P020h55b10135L875",
        );
        let pos = AprsPosition::from_bytes(packet.information())?;
        assert_eq!(pos.symbol().code_char(), '_');
        let weather = pos.weather().ok_or("embedded weather missing")?;
        assert_eq!(weather, &wx);
        Ok(())
    }

    #[test]
    fn build_empty_weather_uses_required_absence_placeholders() -> TestResult {
        let source = test_source();
        let weather = AprsWeather::default();
        let report = AprsPositionlessWeatherReport::new(weather_timestamp()?, weather.clone());
        let packet = header(source.clone(), DigipeaterPath::empty()).weather_packet(&report);
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            "_10092345c...s...g...t...",
        );
        let parsed = AprsPositionlessWeatherReport::from_bytes(packet.information())?;
        assert_eq!(parsed.weather(), &weather);

        let positioned = header(source, DigipeaterPath::empty()).position_weather_packet(
            Latitude::EQUATOR,
            Longitude::PRIME_MERIDIAN,
            SymbolTable::Primary,
            &weather,
        );
        assert_eq!(
            std::str::from_utf8(positioned.information())?,
            "!0000.00N/00000.00E_.../...g...t...",
        );
        let positioned = AprsPosition::from_bytes(positioned.information())?;
        assert_eq!(positioned.weather(), Some(&weather));
        Ok(())
    }

    #[test]
    fn build_weather_exact_boundaries_without_normalization() -> TestResult {
        let source = test_source();
        let mut wx = AprsWeather::new();
        wx.set_wind_direction(Some(WindDirection::new(360)?));
        wx.set_wind_speed(Some(ThreeDigitWeatherValue::new(999)?));
        wx.set_wind_gust(Some(ThreeDigitWeatherValue::new(999)?));
        wx.set_temperature(Some(Fahrenheit::new(-99)?));
        wx.set_rain_1h(Some(ThreeDigitWeatherValue::new(999)?));
        wx.set_rain_24h(Some(ThreeDigitWeatherValue::new(999)?));
        wx.set_rain_since_midnight(Some(ThreeDigitWeatherValue::new(999)?));
        wx.set_humidity(Some(Humidity::new(100)?));
        wx.set_pressure(Some(BarometricPressure::new(99_999)?));
        wx.set_luminosity(Some(Luminosity::new(1_999)?));
        let report = AprsPositionlessWeatherReport::new(weather_timestamp()?, wx.clone());
        let wire = header(source, default_digipeater_path()).weather(&report);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            "_10092345c360s999g999t-99r999p999P999h00b99999l999",
        );
        let parsed = AprsPositionlessWeatherReport::from_bytes(packet.information())?;
        assert_eq!(parsed.weather(), &wx);
        Ok(())
    }

    #[test]
    fn invalid_weather_values_never_reach_builder_inputs() {
        assert!(WindDirection::new(361).is_err());
        assert!(ThreeDigitWeatherValue::new(1_000).is_err());
        assert!(Fahrenheit::new(-100).is_err());
        assert!(Humidity::new(0).is_err());
        assert!(Humidity::new(101).is_err());
        assert!(BarometricPressure::new(100_000).is_err());
        assert!(Luminosity::new(2_000).is_err());
    }

    // ---- build_aprs_position_compressed ----

    #[test]
    fn build_compressed_position_round_trip() -> TestResult {
        let source = test_source();
        let wire = header(source, default_digipeater_path()).position_compressed(
            latitude(35.3),
            longitude(-84.233),
            AprsSymbol::CAR,
            &compressed_text("test"),
        );
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.destination.callsign, "APK005");
        assert_eq!(packet.control_byte(), 0x03);
        assert_eq!(packet.protocol_identifier(), Some(Ax25Pid::NoLayer3));

        // Parse it back through the existing compressed parser.
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        // Compressed encoding has some rounding; check within tolerance.
        assert!(
            (pos.latitude().as_degrees() - 35.3).abs() < 0.01,
            "lat: {}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - (-84.233)).abs() < 0.01,
            "lon: {}",
            pos.longitude().as_degrees(),
        );
        assert_eq!(pos.symbol().table_char(), '/');
        assert_eq!(pos.symbol().code_char(), '>');
        assert!(pos.comment().contains("test"), "comment preserved");
        Ok(())
    }

    #[test]
    fn build_compressed_position_equator_prime_meridian() -> TestResult {
        let source = test_source();
        let wire = header(source, default_digipeater_path()).position_compressed(
            Latitude::EQUATOR,
            Longitude::PRIME_MERIDIAN,
            AprsSymbol::HOUSE,
            &compressed_text(""),
        );
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        assert!(
            pos.latitude().as_degrees().abs() < 0.01,
            "lat: {}",
            pos.latitude().as_degrees()
        );
        assert!(
            pos.longitude().as_degrees().abs() < 0.01,
            "lon: {}",
            pos.longitude().as_degrees()
        );
        Ok(())
    }

    #[test]
    fn build_compressed_position_southern_hemisphere() -> TestResult {
        let source = test_source();
        let wire = header(source, default_digipeater_path()).position_compressed(
            latitude(-33.86),
            longitude(151.21),
            AprsSymbol::CAR,
            &compressed_text("sydney"),
        );
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        assert!(
            (pos.latitude().as_degrees() - (-33.86)).abs() < 0.01,
            "lat: {}",
            pos.latitude().as_degrees(),
        );
        assert!(
            (pos.longitude().as_degrees() - 151.21).abs() < 0.01,
            "lon: {}",
            pos.longitude().as_degrees(),
        );
        Ok(())
    }

    #[test]
    fn compressed_position_uses_all_40_trailing_text_bytes() -> TestResult {
        let text = CompressedPositionText::new(&"c".repeat(CompressedPositionText::MAX_LEN))?;
        let packet = header(test_source(), DigipeaterPath::empty()).position_compressed_packet(
            Latitude::EQUATOR,
            Longitude::PRIME_MERIDIAN,
            AprsSymbol::CAR,
            &text,
        );

        assert_eq!(packet.information().len(), 1 + 13 + 40);
        assert!(packet.information().ends_with(text.as_str().as_bytes()));
        assert_eq!(
            CompressedPositionText::new(&"c".repeat(41)),
            Err(crate::text::AprsTextError::TooLong {
                field: crate::text::AprsTextField::CompressedPositionText,
                maximum: 40,
                actual: 41,
            }),
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
    fn compressed_position_rejects_out_of_range_coordinates() {
        assert!(Latitude::new(200.0).is_err());
        assert!(Latitude::new(-91.0).is_err());
        assert!(Longitude::new(181.0).is_err());
        assert!(Longitude::new(-300.0).is_err());
    }

    #[test]
    fn position_types_reject_non_finite_coordinates() {
        assert!(Latitude::new(f64::NAN).is_err());
        assert!(Latitude::new(f64::INFINITY).is_err());
        assert!(Longitude::new(f64::NAN).is_err());
        assert!(Longitude::new(f64::NEG_INFINITY).is_err());
    }

    // ---- build_aprs_telemetry ----

    #[test]
    fn build_telemetry_frame_round_trip() -> TestResult {
        let source = test_source();
        let sequence = TelemetrySequence::new(42)?;
        let analog = [
            TelemetryAnalogValue::new(100)?,
            TelemetryAnalogValue::new(200)?,
            TelemetryAnalogValue::new(300)?,
            TelemetryAnalogValue::new(400)?,
            TelemetryAnalogValue::new(500)?,
        ];
        let comment = TelemetryComment::new(" pump,OK")?;
        let report = AprsTelemetry::new(
            sequence,
            analog,
            TelemetryBits::from_bits(0b1010_1100),
            Some(comment.clone()),
        );
        let wire = header(source, default_digipeater_path()).telemetry(&report);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Telemetry(t) = data else {
            return Err(format!("expected Telemetry, got {data:?}").into());
        };
        assert_eq!(t.sequence(), sequence);
        assert_eq!(t.analog(), &analog);
        assert_eq!(t.digital().bits(), 0b1010_1100);
        assert_eq!(t.comment(), Some(&comment));
        Ok(())
    }

    #[test]
    fn build_telemetry_emits_mic_and_maximum_analogs_losslessly() -> TestResult {
        let source = test_source();
        let maximum = TelemetryAnalogValue::new(999)?;
        let report = AprsTelemetry::new(
            TelemetrySequence::MIC,
            [maximum; 5],
            TelemetryBits::from_bits(0xFF),
            None,
        );
        let wire = header(source, default_digipeater_path()).telemetry(&report);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let info = std::str::from_utf8(packet.information())?;
        assert_eq!(
            info, "T#MIC,999,999,999,999,999,11111111",
            "builder should emit the canonical comma-bearing MIC form",
        );
        Ok(())
    }

    // ---- build_aprs_telemetry_parm / unit / eqns / bits ----

    #[test]
    fn build_telemetry_parm_emits_canonical_form() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("N0CALL-7")?;
        let labels = TelemetryLabels::new(&[
            "Vbatt", "Temp", "RSSI", "", "", "Door", "PIR", "", "", "", "", "", "",
        ])?;
        let wire = header(source, default_digipeater_path()).telemetry_parm(&addressee, &labels);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Message(msg) = data else {
            return Err(format!("expected Message, got {data:?}").into());
        };
        // Spec form: PARM.A1,A2,A3,A4,A5,B1,B2,B3,B4,B5,B6,B7,B8
        // The caller explicitly supplied empty A4/A5 and trailing B3-B8
        // fields, preserving the established canonical bytes without the
        // builder silently inventing them.
        assert_eq!(msg.text(), "PARM.Vbatt,Temp,RSSI,,,Door,PIR,,,,,,");
        Ok(())
    }

    #[test]
    fn build_telemetry_definition_prefixes_are_not_default_filled() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("N0CALL-7")?;

        let parameters = TelemetryLabels::new(&["Battery", "Btemp"])?;
        let packet = header(source.clone(), default_digipeater_path())
            .telemetry_parm_packet(&addressee, &parameters);
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            ":N0CALL-7 :PARM.Battery,Btemp"
        );

        let units = TelemetryLabels::new(&[
            "v/100", "deg.F", "deg.F", "Mbar", "Kft", "Click", "OPEN", "on", "on", "hi",
        ])?;
        let packet =
            header(source, default_digipeater_path()).telemetry_unit_packet(&addressee, &units);
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            ":N0CALL-7 :UNIT.v/100,deg.F,deg.F,Mbar,Kft,Click,OPEN,on,on,hi"
        );
        Ok(())
    }

    #[test]
    fn build_telemetry_eqns_emits_coefficients() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("N0CALL-7")?;
        let coefficients = TelemetryEquationCoefficients::new(&[
            0.0, 0.1, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ])?;
        let wire = header(source.clone(), default_digipeater_path())
            .telemetry_eqns(&addressee, &coefficients);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Message(msg) = data else {
            return Err(format!("expected Message, got {data:?}").into());
        };
        assert_eq!(msg.text(), "EQNS.0,0.1,0,0,0.5,0,0,0,0,0,0,0,0,0,0");

        let prefix = TelemetryEquationCoefficients::new(&[0.0, 0.1, 0.0])?;
        let packet =
            header(source, default_digipeater_path()).telemetry_eqns_packet(&addressee, &prefix);
        assert_eq!(
            std::str::from_utf8(packet.information())?,
            ":N0CALL-7 :EQNS.0,0.1,0"
        );
        Ok(())
    }

    #[test]
    fn build_telemetry_bits_emits_canonical_form() -> TestResult {
        let source = test_source();
        let addressee = DirectMessageAddressee::new("N0CALL-7")?;
        let project = TelemetryProjectTitle::new("Weather station")?;
        let wire = header(source, default_digipeater_path()).telemetry_bits(
            &addressee,
            TelemetryBits::from_bits(0b1010_1100),
            &project,
        );
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Message(msg) = data else {
            return Err(format!("expected Message, got {data:?}").into());
        };
        assert_eq!(msg.text(), "BITS.10101100,Weather station");
        Ok(())
    }

    // ---- build_aprs_status ----

    #[test]
    fn build_status_round_trip() -> TestResult {
        let source = test_source();
        let text = StatusText::new("On the air in FM18")?;
        let wire = header(source, default_digipeater_path()).status(&text);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        assert_eq!(packet.header.destination.callsign, "APK005");

        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Status(status) = data else {
            return Err(format!("expected Status, got {data:?}").into());
        };
        assert_eq!(status.text(), "On the air in FM18");
        Ok(())
    }

    #[test]
    fn build_status_empty_text() -> TestResult {
        let source = test_source();
        let text = StatusText::new("")?;
        let wire = header(source, default_digipeater_path()).status(&text);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Status(status) = data else {
            return Err(format!("expected Status, got {data:?}").into());
        };
        assert_eq!(status.text(), "");
        Ok(())
    }

    #[test]
    fn build_status_info_field_format() -> TestResult {
        let source = test_source();
        let text = StatusText::new("Hello")?;
        let wire = header(source, default_digipeater_path()).status(&text);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        // Info field should be: >Hello\r
        assert_eq!(packet.information().first().copied(), Some(b'>'));
        assert_eq!(packet.information().get(1..6), Some(b"Hello".as_slice()));
        assert_eq!(packet.information().get(6).copied(), Some(b'\r'));
        Ok(())
    }

    #[test]
    fn build_timestamped_status_round_trip() -> TestResult {
        let source = test_source();
        let timestamp = AprsStatusTimestamp::day_hour_minute_utc(9, 23, 45)?;
        let text = TimestampedStatusText::new("On the air")?;
        let wire = header(source, default_digipeater_path()).timestamped_status(timestamp, &text);
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        assert_eq!(packet.information(), b">092345zOn the air\r");
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Status(status) = data else {
            return Err(format!("expected Status, got {data:?}").into());
        };
        assert_eq!(status.timestamp(), Some(timestamp));
        assert_eq!(status.text(), "On the air");
        Ok(())
    }

    #[test]
    fn build_timestamped_status_accepts_maximum_text() -> TestResult {
        let source = test_source();
        let timestamp = AprsStatusTimestamp::day_hour_minute_utc(31, 23, 59)?;
        let text = TimestampedStatusText::new(&"s".repeat(TimestampedStatusText::MAX_LEN))?;
        let packet =
            header(source, default_digipeater_path()).timestamped_status_packet(timestamp, &text);

        assert_eq!(packet.information().len(), 1 + 7 + 55 + 1);
        assert_eq!(packet.information().first(), Some(&b'>'));
        assert_eq!(packet.information().last(), Some(&b'\r'));
        Ok(())
    }

    // ---- build_aprs_mice ----

    #[test]
    fn build_mice_roundtrip_oklahoma() -> TestResult {
        // 35.258 N, 97.755 W; matches the existing parse_mice test case.
        let source = test_source();
        let report = MiceReport::new(
            latitude(35.258),
            longitude(-97.755),
            mice_speed(121),
            course(212),
            AprsSymbol::CAR,
            mice_status_text("test"),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        // Destination should encode the latitude.
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.latitude().as_degrees() - 35.258).abs() < 0.02,
            "lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - (-97.755)).abs() < 0.02,
            "lon={}",
            pos.longitude().as_degrees(),
        );
        assert_eq!(pos.symbol().table_char(), '/');
        assert_eq!(pos.symbol().code_char(), '>');
        assert!(pos.comment().contains("test"), "comment preserved");
        Ok(())
    }

    #[test]
    fn mice_status_text_fills_but_cannot_exceed_ax25_information_field() -> TestResult {
        let status_text = MiceStatusText::new(&"m".repeat(MiceStatusText::MAX_LEN))?;
        let report = MiceReport::new(
            latitude(35.258),
            longitude(-97.755),
            mice_speed(121),
            course(212),
            AprsSymbol::CAR,
            status_text.clone(),
        )?
        .with_message(MiceMessage::OffDuty);
        let packet = report.to_packet(test_source(), DigipeaterPath::empty());

        assert_eq!(packet.information().len(), 256);
        assert!(
            packet
                .information()
                .ends_with(status_text.as_str().as_bytes())
        );
        assert_eq!(
            MiceStatusText::new(&"m".repeat(MiceStatusText::MAX_LEN + 1)),
            Err(crate::text::AprsTextError::TooLong {
                field: crate::text::AprsTextField::MiceStatusText,
                maximum: 247,
                actual: 248,
            }),
            "a 248-byte status would produce a forbidden 257-byte information field",
        );
        Ok(())
    }

    #[test]
    fn build_mice_roundtrip_north_east() -> TestResult {
        // 51.5 N, 0.1 W (London area)
        let source = test_source();
        let report = MiceReport::new(
            latitude(51.5),
            longitude(-0.1),
            mice_speed(0),
            course(0),
            AprsSymbol::HOUSE,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.latitude().as_degrees() - 51.5).abs() < 0.02,
            "lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - (-0.1)).abs() < 0.02,
            "lon={}",
            pos.longitude().as_degrees(),
        );
        Ok(())
    }

    #[test]
    fn build_mice_roundtrip_southern_hemisphere() -> TestResult {
        // -33.86 S, 151.21 E (Sydney)
        let source = test_source();
        let report = MiceReport::new(
            latitude(-33.86),
            longitude(151.21),
            mice_speed(50),
            course(180),
            AprsSymbol::CAR,
            mice_status_text("sydney"),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.latitude().as_degrees() - (-33.86)).abs() < 0.02,
            "lat={}",
            pos.latitude().as_degrees(),
        );
        assert!(
            (pos.longitude().as_degrees() - 151.21).abs() < 0.02,
            "lon={}",
            pos.longitude().as_degrees(),
        );
        Ok(())
    }

    #[test]
    fn build_mice_speed_course_roundtrip() -> TestResult {
        let source = test_source();
        let report = MiceReport::new(
            latitude(35.0),
            longitude(-97.0),
            mice_speed(55),
            course(270),
            AprsSymbol::CAR,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert_eq!(pos.speed().map(Speed::as_knots), Some(55.0));
        assert_eq!(pos.course().map(Course::as_degrees), Some(270));
        Ok(())
    }

    #[test]
    fn mice_speed_and_course_reject_unrepresentable_values() {
        assert!(MiceSpeed::new(800).is_err());
        assert!(MiceSpeed::new(2_280).is_err());
        assert!(Course::new(361).is_err());
        assert!(Course::new(720).is_err());
    }

    #[test]
    fn build_mice_normal_speed_course_roundtrips() -> TestResult {
        let source = test_source();
        let report = MiceReport::new(
            latitude(35.0),
            longitude(-97.0),
            mice_speed(55),
            course(270),
            AprsSymbol::CAR,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert_eq!(pos.speed().map(Speed::as_knots), Some(55.0));
        assert_eq!(pos.course().map(Course::as_degrees), Some(270));
        Ok(())
    }

    #[test]
    fn build_mice_zero_speed_course() -> TestResult {
        let source = test_source();
        let report = MiceReport::new(
            latitude(40.0),
            longitude(-74.0),
            mice_speed(0),
            course(0),
            AprsSymbol::CAR,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert_eq!(pos.speed().map(Speed::as_knots), Some(0.0));
        // Course 0 = unknown → None in the decoder.
        assert_eq!(pos.course(), None);
        Ok(())
    }

    #[test]
    fn build_mice_lon_0_to_9_sets_offset_and_high_column() -> TestResult {
        // Regression guard (APRS 1.0.1 §10 p.47): a longitude
        // in 0..10° must (a) set the offset bit on destination char 4,
        // and (b) emit an info-field d-byte in the high column 118-127
        // (`v` through `DEL`).
        let source = test_source();
        // London-area longitude 0.1°W: lon_abs = 0.1, lon_deg_raw = 0,
        // expected d = 0 + 90 = 90, expected info[1] = 90 + 28 = 118 (`v`).
        let report = MiceReport::new(
            latitude(51.5),
            longitude(-0.1),
            mice_speed(0),
            course(0),
            AprsSymbol::HOUSE,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        // info[1] is the lon-degrees d-byte. Must sit in 118..=127 for
        // a 0..10° longitude.
        let d_byte = *packet.information().get(1).ok_or("info[1] missing")?;
        assert!(
            (118..=127).contains(&d_byte),
            "lon d-byte {d_byte} outside spec column 118..=127 for 0-9°",
        );

        // Destination char 4 carries the lon-offset flag. For a
        // 0..10° longitude the offset bit must be set, so the char
        // must be in `P..=Y`.
        let dest_str = packet.header.destination.callsign.as_str();
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
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.longitude().as_degrees() - (-0.1)).abs() < 0.02,
            "decoded lon={} should be within 0.02° of -0.1",
            pos.longitude().as_degrees(),
        );
        Ok(())
    }

    #[test]
    fn build_mice_lon_hundredths_boundary_carries() -> TestResult {
        // A rounded 60.00-minute value carries into the next degree and
        // leaves a legal 00-hundredths byte on the wire.
        let source = test_source();
        let report = MiceReport::new(
            latitude(35.0),
            longitude(-97.999_983_3),
            mice_speed(0),
            course(0),
            AprsSymbol::CAR,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;

        // info[3] is the longitude-hundredths byte.
        let hundredths_byte = *packet.information().get(3).ok_or("info[3] missing")?;
        assert_eq!(hundredths_byte, 28, "carried hundredths should be zero");

        // The decoded longitude should still be within ~0.001° of the
        // input (we lose at most one hundredth of a minute ≈ 18 m).
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.longitude().as_degrees() - (-97.999_983_3)).abs() < 0.001,
            "decoded lon={} should be within 0.001° of input",
            pos.longitude().as_degrees(),
        );
        Ok(())
    }

    #[test]
    fn build_mice_high_longitude() -> TestResult {
        // 35.0 N, 140.0 E (Tokyo area)
        let source = test_source();
        let report = MiceReport::new(
            latitude(35.0),
            longitude(140.0),
            mice_speed(10),
            course(90),
            AprsSymbol::CAR,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.latitude().as_degrees() - 35.0).abs() < 0.02,
            "lat={}",
            pos.latitude().as_degrees()
        );
        assert!(
            (pos.longitude().as_degrees() - 140.0).abs() < 0.02,
            "lon={}",
            pos.longitude().as_degrees(),
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
            let report = MiceReport::new(
                latitude(35.25),
                longitude(-97.75),
                mice_speed(10),
                course(90),
                AprsSymbol::CAR,
                mice_status_text(""),
            )?
            .with_message(msg);
            let wire = report.to_wire(source, default_digipeater_path());
            let kiss = KissFrame::from_bytes(&wire)?;
            let packet = Ax25Packet::from_bytes(&kiss.data)?;
            let pos = AprsPosition::from_mice_bytes(
                packet.header.destination.callsign.as_str(),
                packet.information(),
            )?;
            assert_eq!(pos.mice_message(), Some(msg), "round trip for {msg:?}");
        }
        Ok(())
    }

    #[test]
    fn build_mice_lon_100_109() -> TestResult {
        // 35.0 N, 105.5 W (New Mexico)
        let source = test_source();
        let report = MiceReport::new(
            latitude(35.0),
            longitude(-105.5),
            mice_speed(0),
            course(0),
            AprsSymbol::CAR,
            mice_status_text(""),
        )?;
        let wire = report.to_wire(source, default_digipeater_path());

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let pos = AprsPosition::from_mice_bytes(
            packet.header.destination.callsign.as_str(),
            packet.information(),
        )?;
        assert!(
            (pos.longitude().as_degrees() - (-105.5)).abs() < 0.02,
            "lon={}",
            pos.longitude().as_degrees(),
        );
        Ok(())
    }

    // ---- build_query_response_position ----

    #[test]
    fn build_query_response_roundtrip() -> TestResult {
        let source = test_source();
        let wire = header(source, default_digipeater_path()).query_response_position(
            latitude(35.258),
            longitude(-97.755),
            AprsSymbol::CAR,
            &position_text("QRY resp"),
        );
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let data = AprsData::from_bytes(packet.information())?;
        let AprsData::Position(pos) = data else {
            return Err(format!("expected Position, got {data:?}").into());
        };
        assert!((pos.latitude().as_degrees() - 35.258).abs() < 0.01);
        assert!((pos.longitude().as_degrees() - (-97.755)).abs() < 0.01);
        assert!(pos.comment().contains("QRY resp"), "comment preserved");
        Ok(())
    }

    #[test]
    fn selected_identity_and_owned_header_reach_every_packet() -> TestResult {
        let source = Ax25Address::new("W1AW", 3)?;
        let path = DigipeaterPath::new(vec![RouteEntry::new("WIDE2", 1)?])?;
        let selected = Tocall::new("APTEST")?;
        let routing = AprsFrameHeader::new(source.clone(), selected, path.clone());
        assert_eq!(routing.source(), &source);
        assert_eq!(routing.path(), &path);
        assert_eq!(routing.tocall().as_str(), "APTEST");
        let packet = routing.status_packet(&StatusText::new("test")?);
        assert_eq!(packet.header.source, source);
        assert_eq!(packet.header.digipeaters, path);
        assert_eq!(packet.header.destination.callsign, "APTEST");
        assert_eq!(packet.information(), b">test\r");
        Ok(())
    }

    #[test]
    fn mice_longitude_representability_rejects_rounded_antimeridian() -> TestResult {
        for longitude in [180.0, -180.0, 179.999_99, -179.999_99] {
            let report = MiceReport::new(
                Latitude::EQUATOR,
                Longitude::new(longitude)?,
                MiceSpeed::new(0)?,
                Course::new(0)?,
                AprsSymbol::CAR,
                MiceStatusText::new("")?,
            );
            assert_eq!(
                report,
                Err(AprsError::UnrepresentableMiceLongitude),
                "{longitude}"
            );
        }
        for longitude in [179.999_8, -179.999_8] {
            let report = MiceReport::new(
                Latitude::EQUATOR,
                Longitude::new(longitude)?,
                MiceSpeed::new(0)?,
                Course::new(0)?,
                AprsSymbol::CAR,
                MiceStatusText::new("")?,
            )?;
            let packet = report.to_packet(test_source(), DigipeaterPath::empty());
            assert_eq!(
                packet.information().get(1..4),
                Some([107, 87, 127].as_slice())
            );
            let parsed = AprsPosition::from_mice_bytes(
                packet.header.destination.callsign.as_str(),
                packet.information(),
            )?;
            assert!((parsed.longitude().as_degrees() - longitude).abs() < 0.000_1);
        }
        Ok(())
    }
}
