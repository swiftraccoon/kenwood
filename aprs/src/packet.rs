//! APRS packet wrapper, data enum, data extensions, and PHG field.
//!
//! This module contains the top-level [`AprsData`] enum (one variant per
//! APRS data type identifier), the [`parse_aprs_data`] dispatcher, and a
//! handful of shared primitive types: [`AprsDataExtension`], [`Phg`],
//! [`PositionAmbiguity`], [`ParseContext`], [`AprsTimestamp`],
//! [`TelemetryDefinition`], and [`TelemetryParameters`].

use core::fmt;

use crate::error::AprsError;
use crate::item::{
    AprsItem, AprsObject, AprsQuery, parse_aprs_item, parse_aprs_object, parse_aprs_query,
};
use crate::message::{AprsMessage, parse_aprs_message};
use crate::position::{AprsPosition, parse_aprs_position};
use crate::status::{AprsStatus, parse_aprs_status};
use crate::telemetry::{AprsTelemetry, parse_aprs_telemetry};
use crate::weather::{AprsWeather, parse_aprs_weather_positionless};

// ---------------------------------------------------------------------------
// ParseContext
// ---------------------------------------------------------------------------

/// Diagnostic context for a parse failure.
///
/// Carries the byte offset within the input where the parser stopped,
/// alongside an error variant. Most parser entry points return the
/// bare error type for backwards compatibility; use
/// [`ParseContext::with_error`] to wrap one when richer diagnostics are
/// useful (e.g. when reporting failures from a fuzz harness or when
/// logging untrusted wire data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseContext<E> {
    /// Underlying error.
    pub error: E,
    /// Byte offset within the input where the parser noticed the
    /// problem (0 if unknown).
    pub offset: usize,
    /// Optional human-readable name for the field that failed.
    pub field: Option<&'static str>,
}

impl<E> ParseContext<E> {
    /// Wrap an error with the given byte offset and optional field name.
    pub const fn with_error(error: E, offset: usize, field: Option<&'static str>) -> Self {
        Self {
            error,
            offset,
            field,
        }
    }
}

impl<E: fmt::Display> fmt::Display for ParseContext<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = self.field {
            write!(
                f,
                "{} (at byte {} in field {field})",
                self.error, self.offset
            )
        } else {
            write!(f, "{} (at byte {})", self.error, self.offset)
        }
    }
}

// ---------------------------------------------------------------------------
// PositionAmbiguity
// ---------------------------------------------------------------------------

/// APRS position ambiguity level (APRS 1.0.1 §8.1.6).
///
/// Stations can deliberately reduce their reported precision by
/// replacing trailing latitude/longitude digits with spaces. Each level
/// masks one more trailing digit:
///
/// | Level | Example               | Effective precision |
/// |-------|-----------------------|---------------------|
/// | 0     | `4903.50N`            | 0.01 minute         |
/// | 1     | `4903.5 N`            | 0.1 minute          |
/// | 2     | `4903.  N`            | 1 minute            |
/// | 3     | `490 .  N`            | 10 minutes          |
/// | 4     | `49  .  N`            | 1 degree            |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionAmbiguity {
    /// No ambiguity — full DDMM.HH precision.
    None,
    /// Last digit of hundredths-of-a-minute masked (0.1' precision).
    OneDigit,
    /// Whole hundredths-of-a-minute masked (1' precision).
    TwoDigits,
    /// Tens of minutes masked (10' precision).
    ThreeDigits,
    /// Whole minutes masked (1° precision).
    FourDigits,
}

// ---------------------------------------------------------------------------
// AprsTimestamp
// ---------------------------------------------------------------------------

/// An APRS timestamp as used by object and position-with-timestamp
/// reports (APRS 1.0.1 §6.1).
///
/// Four formats are defined on the wire:
///
/// | Suffix | Meaning | Digits |
/// |--------|---------|--------|
/// | `z`    | Day / hour / minute, zulu | DDHHMM |
/// | `/`    | Day / hour / minute, local| DDHHMM |
/// | `h`    | Hour / minute / second, zulu | HHMMSS |
/// | (none) | Month / day / hour / minute, zulu (11 chars) | MDHM |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsTimestamp {
    /// Day / hour / minute in Zulu (UTC) time. Format `DDHHMMz`.
    DhmZulu {
        /// Day of month, 1-31.
        day: u8,
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
    },
    /// Day / hour / minute in local time. Format `DDHHMM/`.
    DhmLocal {
        /// Day of month, 1-31.
        day: u8,
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
    },
    /// Hour / minute / second in Zulu (UTC) time. Format `HHMMSSh`.
    Hms {
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
        /// Second, 0-59.
        second: u8,
    },
    /// Month / day / hour / minute in Zulu (UTC) time (no suffix).
    /// Format `MMDDHHMM`.
    Mdhm {
        /// Month, 1-12.
        month: u8,
        /// Day of month, 1-31.
        day: u8,
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
    },
}

impl AprsTimestamp {
    /// Format this timestamp as the exact 7-byte APRS wire representation
    /// (or 8 bytes for `Mdhm`).
    #[must_use]
    pub fn to_wire_string(self) -> String {
        match self {
            Self::DhmZulu { day, hour, minute } => {
                format!("{day:02}{hour:02}{minute:02}z")
            }
            Self::DhmLocal { day, hour, minute } => {
                format!("{day:02}{hour:02}{minute:02}/")
            }
            Self::Hms {
                hour,
                minute,
                second,
            } => {
                format!("{hour:02}{minute:02}{second:02}h")
            }
            Self::Mdhm {
                month,
                day,
                hour,
                minute,
            } => {
                format!("{month:02}{day:02}{hour:02}{minute:02}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phg and AprsDataExtension
// ---------------------------------------------------------------------------

/// Power-Height-Gain-Directivity data (APRS101 Chapter 7).
///
/// PHG provides station RF characteristics for range circle calculations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phg {
    /// Effective radiated power in watts.
    pub power_watts: u32,
    /// Antenna height above average terrain in feet.
    pub height_feet: u32,
    /// Antenna gain in dB.
    pub gain_db: u8,
    /// Antenna directivity in degrees (0 = omni).
    pub directivity_deg: u16,
}

/// Parsed APRS data extensions from the position comment field.
///
/// Position reports can carry structured data in the comment string
/// after the coordinates. This struct captures the extensions defined
/// in APRS101 Chapters 6-7.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AprsDataExtension {
    /// Course in degrees (0-360) and speed in knots, from CSE/SPD.
    pub course_speed: Option<(u16, u16)>,
    /// Power, Height, Gain, Directivity (PHG).
    pub phg: Option<Phg>,
    /// Altitude in feet (from `/A=NNNNNN` in comment).
    pub altitude_ft: Option<i32>,
    /// DAO precision extension (`!DAO!` for extra lat/lon digits).
    pub dao: Option<(f64, f64)>,
}

/// Parse data extensions from an APRS position comment string.
///
/// Extracts CSE/SPD, PHG, altitude (`/A=NNNNNN`), and DAO (`!DAO!`)
/// extensions per APRS101 Chapters 6-7.
///
/// # Parameters
///
/// - `comment`: The comment string after the APRS position fields.
///
/// # Returns
///
/// An [`AprsDataExtension`] with each field populated if found.
#[must_use]
pub fn parse_aprs_extensions(comment: &str) -> AprsDataExtension {
    let course_speed = parse_cse_spd(comment);
    let phg = parse_phg(comment);
    let altitude_ft = parse_altitude(comment);
    let dao = parse_dao(comment);

    AprsDataExtension {
        course_speed,
        phg,
        altitude_ft,
        dao,
    }
}

/// Parse CSE/SPD from the first 7 characters of the comment.
///
/// Format: `DDD/SSS` where DDD is 3-digit course (000-360) and SSS is
/// 3-digit speed in knots. Per APRS101 Chapter 7, this must be at the
/// start of the comment and use the exact `NNN/NNN` format.
fn parse_cse_spd(comment: &str) -> Option<(u16, u16)> {
    let bytes = comment.as_bytes();
    let header = bytes.get(..7)?;
    if header.get(3) != Some(&b'/') {
        return None;
    }
    let dir_bytes = header.get(..3)?;
    let spd_bytes = header.get(4..7)?;
    if !dir_bytes.iter().all(u8::is_ascii_digit) || !spd_bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let course: u16 = comment.get(0..3)?.parse().ok()?;
    let speed: u16 = comment.get(4..7)?.parse().ok()?;
    if course > 360 {
        return None;
    }
    Some((course, speed))
}

/// PHG power codes: index^2 watts. Per APRS101 Table on p.28.
const PHG_POWER: [u32; 10] = [0, 1, 4, 9, 16, 25, 36, 49, 64, 81];
/// PHG height codes: 10 * 2^N feet.
const PHG_HEIGHT: [u32; 10] = [10, 20, 40, 80, 160, 320, 640, 1280, 2560, 5120];
/// PHG directivity codes: 0=omni, then 20, 40, ..., 320 degrees.
const PHG_DIR: [u16; 10] = [0, 20, 40, 60, 80, 100, 120, 140, 160, 180];

/// Parse a PHG extension from the comment string.
///
/// Format: `PHGNhgd` anywhere in the comment, where each of N, h, g, d
/// is a single ASCII digit (0-9).
fn parse_phg(comment: &str) -> Option<Phg> {
    let idx = comment.find("PHG")?;
    let rest = comment.get(idx + 3..)?;
    let first_four = rest.get(..4)?.as_bytes();
    if !first_four.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let p = (*first_four.first()? - b'0') as usize;
    let h = (*first_four.get(1)? - b'0') as usize;
    let g = *first_four.get(2)? - b'0';
    let d = (*first_four.get(3)? - b'0') as usize;

    Some(Phg {
        power_watts: PHG_POWER.get(p).copied().unwrap_or(0),
        height_feet: PHG_HEIGHT.get(h).copied().unwrap_or(10),
        gain_db: g,
        directivity_deg: PHG_DIR.get(d).copied().unwrap_or(0),
    })
}

/// Parse altitude extension from the comment string.
///
/// Format: `/A=NNNNNN` anywhere in the comment (6-digit altitude in feet,
/// can be negative with a leading minus sign in the 6-digit field).
fn parse_altitude(comment: &str) -> Option<i32> {
    let idx = comment.find("/A=")?;
    let rest = comment.get(idx + 3..)?;
    let val_str = rest.get(..6)?;
    val_str.parse::<i32>().ok()
}

/// Parse a DAO extension from the comment string.
///
/// Format: `!DAO!` where D and O are extra precision digits for latitude
/// and longitude respectively. The middle character indicates the encoding:
/// - Uppercase letter (W): human-readable. D and O are ASCII digits (0-9)
///   representing hundredths of a minute increment (divide by 60 for degrees).
/// - Lowercase letter (w): base-91 encoded. D and O are base-91 characters
///   giving finer precision.
///
/// Returns `(lat_correction, lon_correction)` in decimal degrees.
fn parse_dao(comment: &str) -> Option<(f64, f64)> {
    // Find `!` followed by 3 chars and another `!`.
    let bytes = comment.as_bytes();
    for i in 0..bytes.len().saturating_sub(4) {
        let window = bytes.get(i..i + 5)?;
        if window.first() != Some(&b'!') || window.get(4) != Some(&b'!') {
            continue;
        }
        let d = *window.get(1)?;
        let a = *window.get(2)?;
        let o = *window.get(3)?;

        if a.is_ascii_uppercase() {
            // Human-readable: D and O are ASCII digits.
            if d.is_ascii_digit() && o.is_ascii_digit() {
                let lat_extra = f64::from(d - b'0') / 600.0;
                let lon_extra = f64::from(o - b'0') / 600.0;
                return Some((lat_extra, lon_extra));
            }
        } else if a.is_ascii_lowercase() {
            // Base-91: D and O are base-91 chars (33-123).
            if (33..=123).contains(&d) && (33..=123).contains(&o) {
                let lat_extra = f64::from(d - 33) / (91.0 * 60.0);
                let lon_extra = f64::from(o - 33) / (91.0 * 60.0);
                return Some((lat_extra, lon_extra));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// MessageKind
// ---------------------------------------------------------------------------

/// APRS message kind (per APRS 1.0.1 §14 and bulletin sections).
///
/// Distinguishes direct station-to-station messages from the various
/// bulletin forms based on the addressee prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Direct station-to-station message.
    Direct,
    /// Generic bulletin (addressee `BLN0`-`BLN9`).
    Bulletin {
        /// Bulletin number (0-9).
        number: u8,
    },
    /// Group bulletin (addressee `BLN<group>` where group is an alpha
    /// identifier, e.g. `BLNWX` for weather group).
    GroupBulletin {
        /// Group identifier (1-5 alphanumeric characters).
        group: String,
    },
    /// National Weather Service bulletin (addressee `NWS-*`, `SKY-*`,
    /// `CWA-*`, `BOM-*`).
    NwsBulletin,
    /// An APRS ack/rej control frame (text begins with `ack` or `rej`
    /// followed by 1-5 alnum).
    AckRej,
}

// ---------------------------------------------------------------------------
// TelemetryDefinition / TelemetryParameters
// ---------------------------------------------------------------------------

/// Telemetry parameter definitions sent as APRS messages.
///
/// Per APRS 1.0.1 §13.2, a station that emits telemetry frames can send
/// four additional parameter-definition messages to tell receivers how
/// to interpret the analog and digital channels. These messages use the
/// standard APRS message format (`:ADDRESSEE:PARM.…`) with a well-known
/// keyword prefix.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryDefinition {
    /// `PARM.P1,P2,P3,P4,P5,B1,B2,B3,B4,B5,B6,B7,B8` — human-readable
    /// names for 5 analog + 8 digital channels.
    Parameters(TelemetryParameters),
    /// `UNIT.U1,U2,U3,U4,U5,B1,B2,B3,B4,B5,B6,B7,B8` — unit labels.
    Units(TelemetryParameters),
    /// `EQNS.a1,b1,c1,a2,b2,c2,...` — calibration coefficients for the
    /// 5 analog channels (`y = a*x² + b*x + c`, 15 values total).
    Equations([Option<(f64, f64, f64)>; 5]),
    /// `BITS.b1b2b3b4b5b6b7b8,project_title` — active-bit mask plus
    /// project title.
    Bits {
        /// 8-character binary string specifying which digital bits are
        /// "active" (`'1'`) vs "inactive" (`'0'`).
        bits: String,
        /// Free-form project title (up to 23 characters).
        title: String,
    },
}

/// 5 analog + 8 digital channel labels used by both `PARM.` and `UNIT.`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TelemetryParameters {
    /// Analog channel labels (5 entries, `None` when omitted).
    pub analog: [Option<String>; 5],
    /// Digital channel labels (8 entries, `None` when omitted).
    pub digital: [Option<String>; 8],
}

impl TelemetryDefinition {
    /// Try to parse a telemetry parameter-definition message from the
    /// text portion of an [`AprsMessage`] (everything after the second
    /// `:` in the wire frame).
    ///
    /// Returns `None` when the text doesn't start with a known keyword.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("PARM.") {
            return Some(Self::Parameters(parse_telemetry_labels(rest)));
        }
        if let Some(rest) = trimmed.strip_prefix("UNIT.") {
            return Some(Self::Units(parse_telemetry_labels(rest)));
        }
        if let Some(rest) = trimmed.strip_prefix("EQNS.") {
            return Some(Self::Equations(parse_telemetry_equations(rest)));
        }
        if let Some(rest) = trimmed.strip_prefix("BITS.") {
            let (bits, title) = rest.split_once(',').unwrap_or((rest, ""));
            return Some(Self::Bits {
                bits: bits.to_owned(),
                title: title.to_owned(),
            });
        }
        None
    }
}

/// Parse a comma-separated label list for `PARM.` / `UNIT.`.
fn parse_telemetry_labels(s: &str) -> TelemetryParameters {
    let mut params = TelemetryParameters::default();
    for (i, field) in s.split(',').enumerate() {
        let field = field.trim();
        if i < 5 {
            if !field.is_empty()
                && let Some(slot) = params.analog.get_mut(i)
            {
                *slot = Some(field.to_owned());
            }
        } else if i < 13 {
            if !field.is_empty()
                && let Some(slot) = params.digital.get_mut(i - 5)
            {
                *slot = Some(field.to_owned());
            }
        } else {
            break;
        }
    }
    params
}

/// Parse a `EQNS.` coefficient list into 5 `(a, b, c)` tuples.
fn parse_telemetry_equations(s: &str) -> [Option<(f64, f64, f64)>; 5] {
    let values: Vec<f64> = s
        .split(',')
        .map(str::trim)
        .map(|v| v.parse::<f64>().unwrap_or(0.0))
        .collect();
    let mut out: [Option<(f64, f64, f64)>; 5] = [None, None, None, None, None];
    for (i, slot) in out.iter_mut().enumerate() {
        let base = i * 3;
        if let (Some(&a), Some(&b), Some(&c)) =
            (values.get(base), values.get(base + 1), values.get(base + 2))
        {
            *slot = Some((a, b, c));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// AprsData and AprsPacket
// ---------------------------------------------------------------------------

/// A parsed APRS data frame, covering all major APRS data types.
///
/// Per APRS101.PDF, the data type is determined by the first byte of the
/// AX.25 information field. This enum covers the types most relevant to
/// the TH-D75's APRS implementation.
#[derive(Debug, Clone, PartialEq)]
pub enum AprsData {
    /// Position report (uncompressed, compressed, or Mic-E).
    Position(AprsPosition),
    /// APRS message addressed to a specific station.
    Message(AprsMessage),
    /// Status report (free-form text, optionally with Maidenhead grid).
    Status(AprsStatus),
    /// Object report (named, with position and timestamp).
    Object(AprsObject),
    /// Item report (named, with position, no timestamp).
    Item(AprsItem),
    /// Weather report (temperature, wind, rain, pressure, humidity).
    Weather(AprsWeather),
    /// Telemetry report (analog values and digital status).
    Telemetry(AprsTelemetry),
    /// Query (position, status, message, or direction finding).
    Query(AprsQuery),
    /// Third-party traffic — a packet originating elsewhere and
    /// forwarded by an intermediate station (APRS 1.0.1 §17). The
    /// `header` carries the original `source>dest,path` and the
    /// `payload` the original info field.
    ThirdParty {
        /// Raw `source>dest,path` header text from the third-party
        /// wrapper.
        header: String,
        /// Original APRS info field as bytes (no further parsing).
        payload: Vec<u8>,
    },
    /// Maidenhead grid locator (data type `[`). The string form is the
    /// 4-6 character grid square, e.g. `"EM13qc"` or `"FM18lv"`.
    Grid(String),
    /// Raw GPS sentence / Ultimeter 2000 data (data type `$`).
    ///
    /// APRS 1.0.1 §5.2: anything starting with `$GP`, `$GN`, `$GL`,
    /// `$GA` (GPS/GNSS NMEA) or other `$`-prefixed instrument data.
    /// We store the full NMEA sentence minus the leading `$`.
    RawGps(String),
    /// Station capabilities report (data type `<`).
    ///
    /// APRS 1.0.1 §15.2: comma-separated `TOKEN=value` tuples
    /// describing what the station supports (`IGATE`, `MSG_CNT`,
    /// `LOC_CNT`, etc.). We store them as a map.
    StationCapabilities(Vec<(String, String)>),
    /// Agrelo `DFjr` (direction-finding) data (data type `%`).
    ///
    /// The library doesn't interpret the binary format; we preserve
    /// the raw payload bytes for callers that do.
    AgreloDfJr(Vec<u8>),
    /// User-defined APRS data (data type `{`).
    ///
    /// APRS 1.0.1 §18: format is `{<experiment_id><type><data>` where
    /// the experiment ID is one character. We split it out for
    /// convenience; callers that understand the experiment can parse
    /// the rest.
    UserDefined {
        /// One-character experiment identifier (immediately follows `{`).
        experiment: char,
        /// Everything after the experiment ID.
        data: Vec<u8>,
    },
    /// Invalid/test frame (data type `,`).
    ///
    /// Used for test beacons and frames that should be ignored by
    /// normal receivers. We preserve the payload for diagnostics.
    InvalidOrTest(Vec<u8>),
}

/// A parsed APRS packet. Currently just a thin wrapper over [`AprsData`];
/// future extensions may add envelope-level fields (source callsign,
/// digipeater path) if the APRS layer ever owns the AX.25 context.
#[derive(Debug, Clone, PartialEq)]
pub struct AprsPacket {
    /// Decoded APRS data payload.
    pub data: AprsData,
}

// ---------------------------------------------------------------------------
// parse_aprs_data dispatcher
// ---------------------------------------------------------------------------

/// Parse any APRS data frame from an AX.25 information field.
///
/// Dispatches based on the data type identifier (first byte) to the
/// appropriate parser. For Mic-E positions, use
/// [`crate::mic_e::parse_mice_position`] directly since it also requires
/// the destination address.
///
/// **Prefer [`crate::mic_e::parse_aprs_data_full`] when the AX.25
/// destination address is available** — it handles all data types
/// including Mic-E.
///
/// # Supported data types
///
/// | Byte | Type | Parser |
/// |------|------|--------|
/// | `!`, `=` | Position (no timestamp) | [`parse_aprs_position`] |
/// | `/`, `@` | Position (with timestamp) | [`parse_aprs_position`] |
/// | `:` | Message | Inline |
/// | `>` | Status | Inline |
/// | `;` | Object | Inline |
/// | `)` | Item | Inline |
/// | `_` | Positionless weather | Inline |
/// | `` ` ``, `'` | Mic-E | Returns error (use [`crate::mic_e::parse_mice_position`]) |
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or data is invalid.
pub fn parse_aprs_data(info: &[u8]) -> Result<AprsData, AprsError> {
    let first = *info.first().ok_or(AprsError::InvalidFormat)?;

    match first {
        // Position reports (uncompressed and compressed)
        b'!' | b'=' | b'/' | b'@' => parse_aprs_position(info).map(AprsData::Position),
        // Message
        b':' => parse_aprs_message(info).map(AprsData::Message),
        // Status
        b'>' => parse_aprs_status(info).map(AprsData::Status),
        // Object
        b';' => parse_aprs_object(info).map(AprsData::Object),
        // Item
        b')' => parse_aprs_item(info).map(AprsData::Item),
        // Positionless weather
        b'_' => parse_aprs_weather_positionless(info).map(AprsData::Weather),
        // Telemetry
        b'T' => parse_aprs_telemetry(info).map(AprsData::Telemetry),
        // Query
        b'?' => parse_aprs_query(info).map(AprsData::Query),
        // Third-party traffic (APRS 1.0.1 §17): `}source>dest,path:payload`
        b'}' => parse_aprs_third_party(info),
        // Maidenhead grid locator (APRS 1.0.1 §5.6): `[EM13qc`
        b'[' => parse_aprs_grid(info),
        // Raw GPS / NMEA / Ultimeter (APRS 1.0.1 §5.2): `$GPRMC,...`
        b'$' => parse_aprs_raw_gps(info),
        // Station capabilities (APRS 1.0.1 §15.2): `<IGATE,MSG_CNT=10,LOC_CNT=0`
        b'<' => parse_aprs_capabilities(info),
        // Agrelo DFjr direction-finding data (APRS 1.0.1 §5.5): `%...`
        b'%' => Ok(AprsData::AgreloDfJr(info.get(1..).unwrap_or(&[]).to_vec())),
        // User-defined data (APRS 1.0.1 §18): `{<expid><type><data>`
        b'{' => parse_aprs_user_defined(info),
        // Invalid/test data (APRS 1.0.1 §5.7): `,...`
        b',' => Ok(AprsData::InvalidOrTest(
            info.get(1..).unwrap_or(&[]).to_vec(),
        )),
        // Mic-E (` ' 0x1C 0x1D) needs destination address — use parse_mice_position().
        b'`' | b'\'' | 0x1C | 0x1D => Err(AprsError::MicERequiresDestination),
        // All other types are unrecognized.
        _ => Err(AprsError::InvalidFormat),
    }
}

/// Parse an APRS third-party traffic frame (data type `}`).
///
/// Format: `}source>dest,path:payload`. The outer envelope identifies
/// the station that forwarded the packet, and the inner fields carry
/// the original packet exactly as it appeared on its origin transport
/// (typically APRS-IS).
fn parse_aprs_third_party(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'}') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).ok_or(AprsError::InvalidFormat)?;
    let Some(colon) = body.iter().position(|&b| b == b':') else {
        return Err(AprsError::InvalidFormat);
    };
    let header_bytes = body.get(..colon).ok_or(AprsError::InvalidFormat)?;
    let payload = body
        .get(colon + 1..)
        .ok_or(AprsError::InvalidFormat)?
        .to_vec();
    let header = String::from_utf8_lossy(header_bytes).into_owned();
    Ok(AprsData::ThirdParty { header, payload })
}

/// Parse an APRS Maidenhead grid locator frame (data type `[`).
///
/// Format: `[<4-6 chars>`. The locator is left-padded / right-trimmed.
fn parse_aprs_grid(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'[') {
        return Err(AprsError::InvalidFormat);
    }
    let tail = info.get(1..).unwrap_or(&[]);
    let body = String::from_utf8_lossy(tail)
        .trim_end_matches(['\r', '\n', ' '])
        .to_owned();
    if !(4..=6).contains(&body.len()) {
        return Err(AprsError::InvalidFormat);
    }
    let bytes = body.as_bytes();
    // First two: letters A-R. Next two: digits 0-9. Last two (optional):
    // letters a-x.
    let b0 = *bytes.first().ok_or(AprsError::InvalidFormat)?;
    let b1 = *bytes.get(1).ok_or(AprsError::InvalidFormat)?;
    let b2 = *bytes.get(2).ok_or(AprsError::InvalidFormat)?;
    let b3 = *bytes.get(3).ok_or(AprsError::InvalidFormat)?;
    if !b0.is_ascii_uppercase()
        || !b1.is_ascii_uppercase()
        || !b2.is_ascii_digit()
        || !b3.is_ascii_digit()
        || b0 > b'R'
        || b1 > b'R'
    {
        return Err(AprsError::InvalidFormat);
    }
    if bytes.len() == 6 {
        let b4 = *bytes.get(4).ok_or(AprsError::InvalidFormat)?;
        let b5 = *bytes.get(5).ok_or(AprsError::InvalidFormat)?;
        if !b4.is_ascii_lowercase() || !b5.is_ascii_lowercase() || b4 > b'x' || b5 > b'x' {
            return Err(AprsError::InvalidFormat);
        }
    }
    Ok(AprsData::Grid(body))
}

/// Parse an APRS raw GPS / NMEA frame (data type `$`).
///
/// Per APRS 1.0.1 §5.2, the frame is a full NMEA sentence including the
/// leading `$`. We preserve the body without the leading `$` (so the
/// caller still sees `GPRMC,...` etc.).
fn parse_aprs_raw_gps(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'$') {
        return Err(AprsError::InvalidFormat);
    }
    let tail = info.get(1..).unwrap_or(&[]);
    let body = std::str::from_utf8(tail)
        .map_err(|_| AprsError::InvalidFormat)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    Ok(AprsData::RawGps(body))
}

/// Parse an APRS station capabilities frame (data type `<`).
///
/// Per APRS 1.0.1 §15.2, the body is a comma-separated list of tokens,
/// each of the form `KEY` (flag) or `KEY=value`. Whitespace around the
/// delimiters is not permitted in the spec but we trim it anyway for
/// tolerance.
fn parse_aprs_capabilities(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'<') {
        return Err(AprsError::InvalidFormat);
    }
    let tail = info.get(1..).unwrap_or(&[]);
    let body = std::str::from_utf8(tail)
        .map_err(|_| AprsError::InvalidFormat)?
        .trim_end_matches(['\r', '\n']);
    let mut tokens: Vec<(String, String)> = Vec::new();
    for entry in body.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((k, v)) = entry.split_once('=') {
            tokens.push((k.trim().to_owned(), v.trim().to_owned()));
        } else {
            tokens.push((entry.to_owned(), String::new()));
        }
    }
    Ok(AprsData::StationCapabilities(tokens))
}

/// Parse an APRS user-defined frame (data type `{`).
///
/// Per APRS 1.0.1 §18, the frame is `{<experiment_id>[<type>]<data>`.
/// The experiment ID is the first character after `{`.
fn parse_aprs_user_defined(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'{') {
        return Err(AprsError::InvalidFormat);
    }
    let experiment = *info.get(1).ok_or(AprsError::InvalidFormat)? as char;
    let data = info.get(2..).unwrap_or(&[]).to_vec();
    Ok(AprsData::UserDefined { experiment, data })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- parse_aprs_data dispatch tests ----

    #[test]
    fn dispatch_position() {
        let info = b"!4903.50N/07201.75W-Test";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Position(_))),
            "expected Position variant",
        );
    }

    #[test]
    fn dispatch_message() {
        let info = b":N0CALL   :Hello{1";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Message(_))),
            "expected Message variant",
        );
    }

    #[test]
    fn dispatch_status() {
        let info = b">Status text";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Status(_))),
            "expected Status variant",
        );
    }

    #[test]
    fn dispatch_object() {
        let info = b";OBJNAME  *092345z4903.50N/07201.75W-";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Object(_))),
            "expected Object variant",
        );
    }

    #[test]
    fn dispatch_item() {
        let info = b")ITEM!4903.50N/07201.75W-";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Item(_))),
            "expected Item variant",
        );
    }

    #[test]
    fn dispatch_weather() {
        let info = b"_01011234c180s005t072";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Weather(_))),
            "expected Weather variant",
        );
    }

    #[test]
    fn dispatch_third_party() -> TestResult {
        let info = b"}W1AW>APK005,TCPIP:!4903.50N/07201.75W-from IS";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::ThirdParty { header, payload }
                    if header == "W1AW>APK005,TCPIP"
                        && payload == b"!4903.50N/07201.75W-from IS"
            ),
            "expected ThirdParty, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_locator() -> TestResult {
        let info = b"[EM13qc";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::Grid(g) if g == "EM13qc"),
            "expected Grid, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_4char() -> TestResult {
        let info = b"[FM18";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::Grid(g) if g == "FM18"),
            "expected Grid, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_invalid_rejected() {
        assert!(parse_aprs_data(b"[XX12").is_err(), "X > R rejected");
        assert!(parse_aprs_data(b"[AB").is_err(), "too short rejected");
    }

    #[test]
    fn dispatch_raw_gps() -> TestResult {
        let info = b"$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::RawGps(s) if s.starts_with("GPRMC,") && s.contains("4807.038")
            ),
            "expected RawGps, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_capabilities_parses_tokens() -> TestResult {
        let info = b"<IGATE,MSG_CNT=10,LOC_CNT=42";
        let result = parse_aprs_data(info)?;
        let AprsData::StationCapabilities(tokens) = result else {
            return Err("expected StationCapabilities".into());
        };
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens.first(), Some(&("IGATE".to_owned(), String::new())));
        assert_eq!(
            tokens.get(1),
            Some(&("MSG_CNT".to_owned(), "10".to_owned()))
        );
        assert_eq!(
            tokens.get(2),
            Some(&("LOC_CNT".to_owned(), "42".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn dispatch_agrelo_df() -> TestResult {
        let info = b"%\x01\x02\x03\x04";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::AgreloDfJr(bytes) if bytes == &vec![1u8, 2, 3, 4]),
            "expected AgreloDfJr, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_user_defined() -> TestResult {
        let info = b"{Adata payload";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::UserDefined { experiment, data }
                    if *experiment == 'A' && data == b"data payload"
            ),
            "expected UserDefined, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_invalid_or_test() -> TestResult {
        let info = b",test frame";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::InvalidOrTest(bytes) if bytes == b"test frame"),
            "expected InvalidOrTest, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_mice_returns_error() {
        // Mic-E needs destination address, can't parse from info alone
        let info = &[0x60u8, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        assert!(
            matches!(
                parse_aprs_data(info),
                Err(AprsError::MicERequiresDestination)
            ),
            "expected MicERequiresDestination",
        );
    }

    // ---- ParseContext tests ----

    #[test]
    fn parse_context_display_with_field() {
        let ctx = ParseContext::with_error(AprsError::InvalidFormat, 17, Some("addressee"));
        let s = format!("{ctx}");
        assert!(s.contains("byte 17"), "expected byte 17 in {s:?}");
        assert!(s.contains("addressee"), "expected addressee in {s:?}");
    }

    #[test]
    fn parse_context_display_without_field() {
        let ctx = ParseContext::with_error(AprsError::InvalidCoordinates, 4, None);
        let s = format!("{ctx}");
        assert!(s.contains("byte 4"), "expected byte 4 in {s:?}");
    }

    // ---- Timestamp tests ----

    #[test]
    fn aprs_timestamp_dhm_zulu_format() {
        let ts = AprsTimestamp::DhmZulu {
            day: 9,
            hour: 23,
            minute: 45,
        };
        assert_eq!(ts.to_wire_string(), "092345z");
    }

    #[test]
    fn aprs_timestamp_hms_format() {
        let ts = AprsTimestamp::Hms {
            hour: 12,
            minute: 0,
            second: 1,
        };
        assert_eq!(ts.to_wire_string(), "120001h");
    }

    // ---- Extensions parser tests ----

    #[test]
    fn parse_extensions_cse_spd() {
        let ext = parse_aprs_extensions("088/036");
        assert_eq!(ext.course_speed, Some((88, 36)));
        assert!(ext.phg.is_none());
        assert!(ext.altitude_ft.is_none());
        assert!(ext.dao.is_none());
    }

    #[test]
    fn parse_extensions_cse_spd_with_comment() {
        let ext = parse_aprs_extensions("270/015via Mic-E");
        assert_eq!(ext.course_speed, Some((270, 15)));
    }

    #[test]
    fn parse_extensions_cse_spd_invalid_course() {
        // Course 999 > 360 is invalid.
        let ext = parse_aprs_extensions("999/050");
        assert!(ext.course_speed.is_none());
    }

    #[test]
    fn parse_extensions_cse_spd_not_at_start() {
        // CSE/SPD must be at position 0.
        let ext = parse_aprs_extensions("xx088/036");
        assert!(ext.course_speed.is_none());
    }

    #[test]
    fn parse_extensions_phg() -> TestResult {
        let ext = parse_aprs_extensions("PHG5132");
        let phg = ext.phg.ok_or("phg missing")?;
        assert_eq!(phg.power_watts, 25);
        assert_eq!(phg.height_feet, 20);
        assert_eq!(phg.gain_db, 3);
        assert_eq!(phg.directivity_deg, 40);
        Ok(())
    }

    #[test]
    fn parse_extensions_phg_omni() -> TestResult {
        let ext = parse_aprs_extensions("PHG2360");
        let phg = ext.phg.ok_or("phg missing")?;
        assert_eq!(phg.power_watts, 4);
        assert_eq!(phg.height_feet, 80);
        assert_eq!(phg.gain_db, 6);
        assert_eq!(phg.directivity_deg, 0);
        Ok(())
    }

    #[test]
    fn parse_extensions_phg_in_comment() -> TestResult {
        let ext = parse_aprs_extensions("some text PHG5132 more text");
        let phg = ext.phg.ok_or("phg missing")?;
        assert_eq!(phg.power_watts, 25);
        Ok(())
    }

    #[test]
    fn parse_extensions_altitude() {
        let ext = parse_aprs_extensions("some comment /A=001234 more");
        assert_eq!(ext.altitude_ft, Some(1234));
    }

    #[test]
    fn parse_extensions_altitude_negative() {
        let ext = parse_aprs_extensions("/A=-00100");
        assert_eq!(ext.altitude_ft, Some(-100));
    }

    #[test]
    fn parse_extensions_altitude_zeros() {
        let ext = parse_aprs_extensions("/A=000000");
        assert_eq!(ext.altitude_ft, Some(0));
    }

    #[test]
    fn parse_extensions_dao_human_readable() -> TestResult {
        // !W5! — W is uppercase, so digits 5 and 5.
        let ext = parse_aprs_extensions("text !5W5! more");
        let (lat, lon) = ext.dao.ok_or("dao missing")?;
        let expected = 5.0 / 600.0;
        assert!((lat - expected).abs() < 1e-9, "lat={lat}");
        assert!((lon - expected).abs() < 1e-9, "lon={lon}");
        Ok(())
    }

    #[test]
    fn parse_extensions_dao_base91() -> TestResult {
        // !w"! — w is lowercase, " is char 34, so base-91 value = 34-33 = 1
        let ext = parse_aprs_extensions("!\"w\"!");
        let (lat, lon) = ext.dao.ok_or("dao missing")?;
        let expected = 1.0 / (91.0 * 60.0);
        assert!((lat - expected).abs() < 1e-9, "lat={lat}");
        assert!((lon - expected).abs() < 1e-9, "lon={lon}");
        Ok(())
    }

    #[test]
    fn parse_extensions_combined() {
        let ext = parse_aprs_extensions("088/036PHG5132/A=001234");
        assert_eq!(ext.course_speed, Some((88, 36)));
        assert!(ext.phg.is_some());
        assert_eq!(ext.altitude_ft, Some(1234));
    }

    #[test]
    fn parse_extensions_empty() {
        let ext = parse_aprs_extensions("");
        assert!(ext.course_speed.is_none());
        assert!(ext.phg.is_none());
        assert!(ext.altitude_ft.is_none());
        assert!(ext.dao.is_none());
    }

    // ---- TelemetryDefinition tests ----

    #[test]
    fn telemetry_definition_parm() -> TestResult {
        let def =
            TelemetryDefinition::from_text("PARM.Volts,Temp,Humid,Wind,Rain,Door,Light,Heat,,,,,")
                .ok_or("missing")?;
        let TelemetryDefinition::Parameters(p) = def else {
            return Err("expected Parameters".into());
        };
        assert_eq!(p.analog.first().and_then(Option::as_deref), Some("Volts"));
        assert_eq!(p.analog.get(4).and_then(Option::as_deref), Some("Rain"));
        assert_eq!(p.digital.first().and_then(Option::as_deref), Some("Door"));
        assert_eq!(p.digital.get(2).and_then(Option::as_deref), Some("Heat"));
        Ok(())
    }

    #[test]
    fn telemetry_definition_unit() -> TestResult {
        let def = TelemetryDefinition::from_text("UNIT.Vdc,C,%,mph,in,open,lit,on,,,,,")
            .ok_or("missing")?;
        let TelemetryDefinition::Units(p) = def else {
            return Err("expected Units".into());
        };
        assert_eq!(p.analog.get(1).and_then(Option::as_deref), Some("C"));
        Ok(())
    }

    #[test]
    fn telemetry_definition_eqns() -> TestResult {
        let def = TelemetryDefinition::from_text("EQNS.0,0.1,0,0,0.5,0,0,1,0,0,2,0,0,3,0")
            .ok_or("missing")?;
        let TelemetryDefinition::Equations(eqs) = def else {
            return Err("expected Equations".into());
        };
        assert_eq!(eqs.first(), Some(&Some((0.0, 0.1, 0.0))));
        assert_eq!(eqs.get(1), Some(&Some((0.0, 0.5, 0.0))));
        assert_eq!(eqs.get(4), Some(&Some((0.0, 3.0, 0.0))));
        Ok(())
    }

    #[test]
    fn telemetry_definition_bits() -> TestResult {
        let def = TelemetryDefinition::from_text("BITS.11111111,WX station telemetry")
            .ok_or("missing")?;
        let TelemetryDefinition::Bits { bits, title } = def else {
            return Err("expected Bits".into());
        };
        assert_eq!(bits, "11111111");
        assert_eq!(title, "WX station telemetry");
        Ok(())
    }

    #[test]
    fn telemetry_definition_unknown_returns_none() {
        assert!(TelemetryDefinition::from_text("hello world").is_none());
    }
}
