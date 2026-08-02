//! Swift-facing APRS operational-session models and journal.

use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use ::aprs::{
    AprsData, AprsItem, AprsObject, AprsPosition, MAX_APRS_MESSAGE_TEXT_LEN,
    build_aprs_message_packet, build_aprs_position_report_packet, parse_aprs_data_full,
};
use ax25_codec::{Ax25Address, Ax25Packet, RouteEntry, build_ax25, parse_ax25};
use kenwood_thd75::types::TncBaud;

use crate::automation::AutomationError;

/// Maximum number of activity rows retained by the in-memory journal.
const ACTIVITY_CAPACITY: usize = 1_000;

/// TNC data rate used for an APRS KISS session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AprsTncBaud {
    /// 1200-baud AFSK, normally used on VHF APRS channels.
    Bps1200,
    /// 9600-baud packet mode.
    Bps9600,
}

impl From<AprsTncBaud> for TncBaud {
    fn from(value: AprsTncBaud) -> Self {
        match value {
            AprsTncBaud::Bps1200 => Self::Bps1200,
            AprsTncBaud::Bps9600 => Self::Bps9600,
        }
    }
}

/// Configuration for one host-owned APRS KISS session.
///
/// KISS and CAT are mutually exclusive on the TH-D75. Starting this session
/// suspends screen capture and persistent setting access until it is stopped.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AprsSessionConfig {
    /// Source station in `CALL` or `CALL-SSID` form. Empty enables RX-only use.
    pub station_callsign: String,
    /// Comma-separated AX.25 digipeater path, or empty for a direct path.
    pub path: String,
    /// TNC data rate.
    pub baud: AprsTncBaud,
    /// APRS symbol table or overlay character, exactly one printable ASCII byte.
    pub symbol_table: String,
    /// APRS symbol code, exactly one printable ASCII byte.
    pub symbol_code: String,
    /// KISS TX delay in units of 10 milliseconds. TH-D75 range: 0 through 120.
    pub tx_delay_10ms: u8,
    /// KISS persistence value. Probability is `(value + 1) / 256`.
    pub persistence: u8,
    /// KISS slot time in units of 10 milliseconds. TH-D75 range: 0 through 250.
    pub slot_time_10ms: u8,
    /// KISS TX tail in units of 10 milliseconds.
    pub tx_tail_10ms: u8,
    /// Whether the KISS TNC is configured for full duplex.
    pub full_duplex: bool,
}

impl Default for AprsSessionConfig {
    fn default() -> Self {
        Self {
            station_callsign: String::new(),
            path: "WIDE1-1,WIDE2-1".to_owned(),
            baud: AprsTncBaud::Bps1200,
            symbol_table: "/".to_owned(),
            symbol_code: ">".to_owned(),
            tx_delay_10ms: 50,
            persistence: 128,
            slot_time_10ms: 10,
            tx_tail_10ms: 3,
            full_duplex: false,
        }
    }
}

/// Lifecycle phase of the host-owned APRS session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AprsSessionPhase {
    /// Qualified automation CAT control is active; no packet bytes are being monitored.
    Inactive,
    /// The controller is leaving CAT and entering KISS.
    Starting,
    /// KISS is active and incoming packet bytes are being drained continuously.
    Active,
    /// KISS Return was requested and automation control is being requalified.
    Restoring,
    /// The last APRS lifecycle transition failed.
    Failed,
}

/// Current APRS operational-session status and cumulative counters.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AprsSessionStatus {
    /// Current lifecycle phase.
    pub phase: AprsSessionPhase,
    /// Identifier shared by every activity row from the current/last session.
    pub session_id: u64,
    /// Unix epoch milliseconds at which the current session began.
    pub started_at_unix_ms: Option<u64>,
    /// Active or most recently attempted configuration.
    pub configuration: Option<AprsSessionConfig>,
    /// Number of valid AX.25 packets received during this session.
    pub received_packets: u64,
    /// Number of explicit host-requested packets transmitted during this session.
    pub transmitted_packets: u64,
    /// Number of KISS data frames that could not be decoded as AX.25.
    pub decode_failures: u64,
    /// Number of old journal rows evicted from bounded storage.
    pub dropped_activities: u64,
    /// Human-readable failure from the most recent lifecycle transition.
    pub last_error: Option<String>,
}

impl Default for AprsSessionStatus {
    fn default() -> Self {
        Self {
            phase: AprsSessionPhase::Inactive,
            session_id: 0,
            started_at_unix_ms: None,
            configuration: None,
            received_packets: 0,
            transmitted_packets: 0,
            decode_failures: 0,
            dropped_activities: 0,
            last_error: None,
        }
    }
}

/// Direction of an APRS activity row relative to the connected radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AprsActivityDirection {
    /// A packet received from the radio's KISS TNC.
    Rx,
    /// A packet explicitly sent by Azimuth to the radio's KISS TNC.
    Tx,
    /// A session, control, or error event that is not an RF packet.
    System,
}

/// Decoded category of one APRS activity row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AprsActivityKind {
    /// KISS/CAT session lifecycle information.
    Session,
    /// APRS position or Mic-E position.
    Position,
    /// APRS addressed message, bulletin, acknowledgement, or rejection.
    Message,
    /// APRS free-form status report.
    Status,
    /// APRS object report.
    Object,
    /// APRS item report.
    Item,
    /// APRS weather report.
    Weather,
    /// APRS telemetry report.
    Telemetry,
    /// APRS query.
    Query,
    /// Third-party APRS traffic.
    ThirdParty,
    /// Maidenhead grid report.
    Grid,
    /// Raw NMEA/GPS payload.
    RawGps,
    /// Station capability report.
    Capabilities,
    /// Direction-finding payload.
    DirectionFinding,
    /// User-defined APRS payload.
    UserDefined,
    /// Invalid/test APRS payload.
    Test,
    /// Legacy raw weather payload.
    RawWeather,
    /// Valid AX.25 whose information field is not recognized as APRS.
    Ax25,
    /// Non-data KISS control frame.
    KissControl,
    /// KISS data that failed AX.25 decoding.
    DecodeError,
    /// Session, transport, or restore failure that is not a packet decode error.
    Error,
}

/// One timestamped row in the live APRS activity journal.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AprsActivityRecord {
    /// Monotonically increasing journal identifier.
    pub sequence: u64,
    /// Session that produced this row.
    pub session_id: u64,
    /// Observation or completed-transmit time in Unix epoch milliseconds.
    pub timestamp_unix_ms: u64,
    /// Direction relative to the connected radio.
    pub direction: AprsActivityDirection,
    /// Decoded activity category.
    pub kind: AprsActivityKind,
    /// AX.25 source address when available.
    pub source: Option<String>,
    /// AX.25 destination address when available.
    pub destination: Option<String>,
    /// Ordered AX.25 digipeater path.
    pub path: Vec<String>,
    /// Concise human-readable decoded summary.
    pub summary: String,
    /// Lossless-enough TNC2-style envelope plus lossy text info for inspection.
    pub raw_packet: String,
    /// Exact AX.25 bytes from the KISS data field. Empty for system events.
    pub raw_ax25: Vec<u8>,
    /// Position latitude when the APRS payload decoded to a position.
    pub latitude: Option<f64>,
    /// Position longitude when the APRS payload decoded to a position.
    pub longitude: Option<f64>,
    /// Reported speed in knots, when present.
    pub speed_knots: Option<u16>,
    /// Reported course in degrees, when present.
    pub course_degrees: Option<u16>,
}

/// Latest per-station observation derived solely from received packets.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AprsStationRecord {
    /// AX.25 station address including SSID when non-zero.
    pub callsign: String,
    /// Most recent receive time in Unix epoch milliseconds.
    pub last_heard_unix_ms: u64,
    /// Packets observed from this source during retained process lifetime.
    pub packet_count: u64,
    /// Most recently decoded latitude.
    pub latitude: Option<f64>,
    /// Most recently decoded longitude.
    pub longitude: Option<f64>,
    /// Most recently decoded speed in knots.
    pub speed_knots: Option<u16>,
    /// Most recently decoded course in degrees.
    pub course_degrees: Option<u16>,
    /// Digipeater path from the latest packet.
    pub path: Vec<String>,
    /// Summary from the latest received packet.
    pub latest_summary: String,
}

/// Immutable polling snapshot for APRS activity and station views.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AprsOperationalSnapshot {
    /// Current lifecycle and counters.
    pub status: AprsSessionStatus,
    /// Rows newer than the caller's requested sequence.
    pub activities: Vec<AprsActivityRecord>,
    /// Latest state for each heard source, newest first.
    pub stations: Vec<AprsStationRecord>,
    /// Highest activity sequence allocated so far.
    pub latest_sequence: u64,
    /// Whether requested history had already been evicted from bounded storage.
    pub history_truncated: bool,
}

/// Validated Rust-only form of [`AprsSessionConfig`].
#[derive(Debug, Clone)]
pub(crate) struct AprsRuntimeConfig {
    pub(crate) source: Option<Ax25Address>,
    pub(crate) path: Vec<RouteEntry>,
    pub(crate) symbol_table: char,
    pub(crate) symbol_code: char,
}

impl AprsSessionConfig {
    pub(crate) fn validate(&self) -> Result<AprsRuntimeConfig, AutomationError> {
        if self.tx_delay_10ms > 120 {
            return Err(invalid_config(
                "TX delay must be between 0 and 120 (0–1200 ms)",
            ));
        }
        if self.slot_time_10ms > 250 {
            return Err(invalid_config(
                "slot time must be between 0 and 250 (0–2500 ms)",
            ));
        }

        let station = self.station_callsign.trim().to_ascii_uppercase();
        let source = if station.is_empty() {
            None
        } else {
            let (callsign, ssid) = parse_station_address(&station)?;
            Some(Ax25Address::new(callsign, ssid).map_err(|error| {
                invalid_config(&format!("invalid source station {station}: {error}"))
            })?)
        };
        let path = kenwood_thd75::aprs::parse_digipeater_path(&self.path)
            .map_err(|error| invalid_config(&format!("invalid digipeater path: {error}")))?;
        let symbol_table = one_symbol(&self.symbol_table, "symbol table")?;
        let symbol_code = one_symbol(&self.symbol_code, "symbol code")?;

        Ok(AprsRuntimeConfig {
            source,
            path,
            symbol_table,
            symbol_code,
        })
    }
}

fn parse_station_address(station: &str) -> Result<(&str, u8), AutomationError> {
    let (callsign, ssid) = station.split_once('-').map_or((station, 0), |(call, raw)| {
        let parsed = raw.parse::<u8>().unwrap_or(u8::MAX);
        (call, parsed)
    });
    if callsign.is_empty() || callsign.len() > 6 || ssid > 15 {
        return Err(invalid_config(
            "source station must be a 1–6 character AX.25 callsign with optional SSID 0–15",
        ));
    }
    Ok((callsign, ssid))
}

fn one_symbol(value: &str, label: &str) -> Result<char, AutomationError> {
    let mut chars = value.chars();
    let Some(symbol) = chars.next() else {
        return Err(invalid_config(&format!(
            "{label} must contain one printable ASCII character"
        )));
    };
    if chars.next().is_some() || !symbol.is_ascii() || !('!'..='~').contains(&symbol) {
        return Err(invalid_config(&format!(
            "{label} must contain one printable ASCII character"
        )));
    }
    Ok(symbol)
}

fn invalid_config(detail: &str) -> AutomationError {
    AutomationError::InvalidAprsConfiguration {
        detail: detail.to_owned(),
    }
}

/// Actor-owned journal storage shared with synchronous snapshot reads.
#[derive(Debug)]
pub(crate) struct AprsActivityStore {
    status: AprsSessionStatus,
    activities: VecDeque<AprsActivityRecord>,
    stations: BTreeMap<String, AprsStationRecord>,
    next_sequence: u64,
    next_session_id: u64,
}

impl Default for AprsActivityStore {
    fn default() -> Self {
        Self {
            status: AprsSessionStatus::default(),
            activities: VecDeque::with_capacity(ACTIVITY_CAPACITY),
            stations: BTreeMap::new(),
            next_sequence: 1,
            next_session_id: 1,
        }
    }
}

impl AprsActivityStore {
    pub(crate) fn begin_start(&mut self, config: AprsSessionConfig) {
        let session_id = take_identifier(&mut self.next_session_id);
        self.stations.clear();
        self.status = AprsSessionStatus {
            phase: AprsSessionPhase::Starting,
            session_id,
            started_at_unix_ms: Some(unix_milliseconds()),
            configuration: Some(config),
            received_packets: 0,
            transmitted_packets: 0,
            decode_failures: 0,
            dropped_activities: self.status.dropped_activities,
            last_error: None,
        };
        self.push_system("Entering host KISS mode; CAT screen and settings control are paused.");
    }

    pub(crate) fn mark_active(&mut self) {
        self.status.phase = AprsSessionPhase::Active;
        self.status.last_error = None;
        self.push_system("KISS packet monitoring is active.");
    }

    pub(crate) fn mark_restoring(&mut self) {
        self.status.phase = AprsSessionPhase::Restoring;
        self.push_system("Stopping KISS and restoring qualified automation CAT control.");
    }

    pub(crate) fn mark_inactive(&mut self) {
        self.status.phase = AprsSessionPhase::Inactive;
        self.status.last_error = None;
        self.push_system("KISS stopped; qualified automation CAT control is restored.");
    }

    pub(crate) fn mark_start_failed_after_restoration(&mut self, detail: &str) {
        self.status.phase = AprsSessionPhase::Inactive;
        self.status.last_error = Some(detail.to_owned());
        drop(self.push_record(AprsActivityRecord {
            sequence: 0,
            session_id: self.status.session_id,
            timestamp_unix_ms: unix_milliseconds(),
            direction: AprsActivityDirection::System,
            kind: AprsActivityKind::Error,
            source: None,
            destination: None,
            path: Vec::new(),
            summary: format!(
                "APRS did not start; qualified automation CAT control was restored: {detail}"
            ),
            raw_packet: String::new(),
            raw_ax25: Vec::new(),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        }));
    }

    pub(crate) fn mark_failed(&mut self, detail: String) {
        self.status.phase = AprsSessionPhase::Failed;
        self.status.last_error = Some(detail.clone());
        drop(self.push_record(AprsActivityRecord {
            sequence: 0,
            session_id: self.status.session_id,
            timestamp_unix_ms: unix_milliseconds(),
            direction: AprsActivityDirection::System,
            kind: AprsActivityKind::Error,
            source: None,
            destination: None,
            path: Vec::new(),
            summary: detail,
            raw_packet: String::new(),
            raw_ax25: Vec::new(),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        }));
    }

    pub(crate) fn push_operation_error(&mut self, detail: String) {
        self.status.last_error = Some(detail.clone());
        drop(self.push_record(AprsActivityRecord {
            sequence: 0,
            session_id: self.status.session_id,
            timestamp_unix_ms: unix_milliseconds(),
            direction: AprsActivityDirection::System,
            kind: AprsActivityKind::Error,
            source: None,
            destination: None,
            path: Vec::new(),
            summary: detail,
            raw_packet: String::new(),
            raw_ax25: Vec::new(),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        }));
    }

    pub(crate) fn push_session_note(&mut self, summary: &str) {
        self.push_system(summary);
    }

    pub(crate) fn status(&self) -> AprsSessionStatus {
        self.status.clone()
    }

    pub(crate) fn push_received_ax25(&mut self, raw_ax25: Vec<u8>) -> AprsActivityRecord {
        match parse_ax25(&raw_ax25) {
            Ok(packet) => {
                self.status.received_packets = self.status.received_packets.saturating_add(1);
                let record = activity_from_packet(
                    self.status.session_id,
                    AprsActivityDirection::Rx,
                    &packet,
                    raw_ax25,
                );
                self.push_record(record)
            }
            Err(error) => {
                self.status.decode_failures = self.status.decode_failures.saturating_add(1);
                self.push_record(AprsActivityRecord {
                    sequence: 0,
                    session_id: self.status.session_id,
                    timestamp_unix_ms: unix_milliseconds(),
                    direction: AprsActivityDirection::Rx,
                    kind: AprsActivityKind::DecodeError,
                    source: None,
                    destination: None,
                    path: Vec::new(),
                    summary: format!("AX.25 decode failed: {error}"),
                    raw_packet: hex_bytes(&raw_ax25),
                    raw_ax25,
                    latitude: None,
                    longitude: None,
                    speed_knots: None,
                    course_degrees: None,
                })
            }
        }
    }

    pub(crate) fn push_kiss_control(&mut self, command: &str, data: &[u8]) {
        let raw_packet = hex_bytes(data);
        drop(self.push_record(AprsActivityRecord {
            sequence: 0,
            session_id: self.status.session_id,
            timestamp_unix_ms: unix_milliseconds(),
            direction: AprsActivityDirection::System,
            kind: AprsActivityKind::KissControl,
            source: None,
            destination: None,
            path: Vec::new(),
            summary: format!("KISS control frame: {command}"),
            raw_packet,
            raw_ax25: Vec::new(),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        }));
    }

    pub(crate) fn build_message(
        &self,
        addressee: &str,
        text: &str,
        message_id: Option<&str>,
    ) -> Result<Ax25Packet, AutomationError> {
        let runtime = self.active_runtime()?;
        let source = runtime
            .source
            .ok_or_else(|| AutomationError::AprsOperation {
                detail: "set a valid source callsign before transmitting".to_owned(),
            })?;
        if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
            return Err(AutomationError::AprsOperation {
                detail: format!(
                    "message text is {} bytes; APRS permits at most {MAX_APRS_MESSAGE_TEXT_LEN}",
                    text.len()
                ),
            });
        }
        if addressee.is_empty() || addressee.len() > 9 || !addressee.is_ascii() {
            return Err(AutomationError::AprsOperation {
                detail: "message addressee must contain 1–9 ASCII characters".to_owned(),
            });
        }
        if let Some(identifier) = message_id
            && (identifier.is_empty()
                || identifier.len() > 5
                || !identifier.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        {
            return Err(AutomationError::AprsOperation {
                detail: "message ID must contain 1–5 ASCII letters or digits".to_owned(),
            });
        }
        Ok(build_aprs_message_packet(
            &source,
            addressee,
            text,
            message_id,
            &runtime.path,
        ))
    }

    pub(crate) fn build_position(
        &self,
        latitude: f64,
        longitude: f64,
        comment: &str,
    ) -> Result<Ax25Packet, AutomationError> {
        if !latitude.is_finite()
            || !longitude.is_finite()
            || !(-90.0..=90.0).contains(&latitude)
            || !(-180.0..=180.0).contains(&longitude)
        {
            return Err(AutomationError::AprsOperation {
                detail: "position must contain finite latitude −90…90 and longitude −180…180"
                    .to_owned(),
            });
        }
        let runtime = self.active_runtime()?;
        let source = runtime
            .source
            .ok_or_else(|| AutomationError::AprsOperation {
                detail: "set a valid source callsign before transmitting".to_owned(),
            })?;
        Ok(build_aprs_position_report_packet(
            &source,
            latitude,
            longitude,
            runtime.symbol_table,
            runtime.symbol_code,
            comment,
            &runtime.path,
        ))
    }

    fn active_runtime(&self) -> Result<AprsRuntimeConfig, AutomationError> {
        if self.status.phase != AprsSessionPhase::Active {
            return Err(AutomationError::AprsModeInactive);
        }
        self.status
            .configuration
            .as_ref()
            .ok_or(AutomationError::AprsModeInactive)?
            .validate()
    }

    pub(crate) fn push_transmitted(&mut self, packet: &Ax25Packet) -> AprsActivityRecord {
        self.status.transmitted_packets = self.status.transmitted_packets.saturating_add(1);
        let raw_ax25 = build_ax25(packet);
        let record = activity_from_packet(
            self.status.session_id,
            AprsActivityDirection::Tx,
            packet,
            raw_ax25,
        );
        self.push_record(record)
    }

    pub(crate) fn snapshot(&self, after_sequence: Option<u64>) -> AprsOperationalSnapshot {
        let after = after_sequence.unwrap_or(0);
        let oldest = self
            .activities
            .front()
            .map_or(self.next_sequence, |row| row.sequence);
        let history_truncated = after > 0 && after.saturating_add(1) < oldest;
        let activities = self
            .activities
            .iter()
            .filter(|row| row.sequence > after)
            .cloned()
            .collect();
        let mut stations: Vec<AprsStationRecord> = self.stations.values().cloned().collect();
        stations.sort_by(|left, right| {
            right
                .last_heard_unix_ms
                .cmp(&left.last_heard_unix_ms)
                .then_with(|| left.callsign.cmp(&right.callsign))
        });
        AprsOperationalSnapshot {
            status: self.status.clone(),
            activities,
            stations,
            latest_sequence: self.next_sequence.saturating_sub(1),
            history_truncated,
        }
    }

    fn push_system(&mut self, summary: &str) {
        drop(self.push_record(AprsActivityRecord {
            sequence: 0,
            session_id: self.status.session_id,
            timestamp_unix_ms: unix_milliseconds(),
            direction: AprsActivityDirection::System,
            kind: AprsActivityKind::Session,
            source: None,
            destination: None,
            path: Vec::new(),
            summary: summary.to_owned(),
            raw_packet: String::new(),
            raw_ax25: Vec::new(),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        }));
    }

    fn push_record(&mut self, mut record: AprsActivityRecord) -> AprsActivityRecord {
        record.sequence = take_identifier(&mut self.next_sequence);
        if record.direction == AprsActivityDirection::Rx
            && let Some(source) = record.source.clone()
        {
            let station = self
                .stations
                .entry(source.clone())
                .or_insert(AprsStationRecord {
                    callsign: source,
                    last_heard_unix_ms: record.timestamp_unix_ms,
                    packet_count: 0,
                    latitude: None,
                    longitude: None,
                    speed_knots: None,
                    course_degrees: None,
                    path: Vec::new(),
                    latest_summary: String::new(),
                });
            station.last_heard_unix_ms = record.timestamp_unix_ms;
            station.packet_count = station.packet_count.saturating_add(1);
            if record.latitude.is_some() {
                station.latitude = record.latitude;
                station.longitude = record.longitude;
                station.speed_knots = record.speed_knots;
                station.course_degrees = record.course_degrees;
            }
            station.path.clone_from(&record.path);
            station.latest_summary.clone_from(&record.summary);
        }
        while self.activities.len() >= ACTIVITY_CAPACITY {
            drop(self.activities.pop_front());
            self.status.dropped_activities = self.status.dropped_activities.saturating_add(1);
        }
        self.activities.push_back(record.clone());
        record
    }
}

fn activity_from_packet(
    session_id: u64,
    direction: AprsActivityDirection,
    packet: &Ax25Packet,
    raw_ax25: Vec<u8>,
) -> AprsActivityRecord {
    let source = packet.source.to_string();
    let destination = packet.destination.to_string();
    let path: Vec<String> = packet.digipeaters.iter().map(ToString::to_string).collect();
    let raw_packet = tnc2_packet(&source, &destination, &path, &packet.info);
    let decoded = decode_summary(&source, packet);
    AprsActivityRecord {
        sequence: 0,
        session_id,
        timestamp_unix_ms: unix_milliseconds(),
        direction,
        kind: decoded.kind,
        source: Some(source),
        destination: Some(destination),
        path,
        summary: decoded.summary,
        raw_packet,
        raw_ax25,
        latitude: decoded.latitude,
        longitude: decoded.longitude,
        speed_knots: decoded.speed_knots,
        course_degrees: decoded.course_degrees,
    }
}

#[derive(Debug)]
struct DecodedSummary {
    kind: AprsActivityKind,
    summary: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
    speed_knots: Option<u16>,
    course_degrees: Option<u16>,
}

fn decode_summary(source: &str, packet: &Ax25Packet) -> DecodedSummary {
    let Ok(data) = parse_aprs_data_full(&packet.info, packet.destination.callsign.as_str()) else {
        return DecodedSummary {
            kind: AprsActivityKind::Ax25,
            summary: format!("{source}: {}", String::from_utf8_lossy(&packet.info)),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        };
    };

    match data {
        AprsData::Position(position) => position_summary(source, &position),
        AprsData::Message(message) => DecodedSummary {
            kind: AprsActivityKind::Message,
            summary: format!("{source} → {}: {}", message.addressee, message.text),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
        },
        AprsData::Status(status) => simple_summary(
            AprsActivityKind::Status,
            format!("{source} status: {}", status.text),
        ),
        AprsData::Object(object) => object_summary(source, &object),
        AprsData::Item(item) => item_summary(source, &item),
        AprsData::Weather(weather) => {
            let detail = weather.temperature.map_or_else(
                || "weather report".to_owned(),
                |temperature| format!("weather {temperature} °F"),
            );
            simple_summary(AprsActivityKind::Weather, format!("{source} {detail}"))
        }
        AprsData::Telemetry(telemetry) => simple_summary(
            AprsActivityKind::Telemetry,
            format!("{source} telemetry sequence {}", telemetry.sequence),
        ),
        AprsData::Query(query) => {
            simple_summary(AprsActivityKind::Query, format!("{source} query {query:?}"))
        }
        AprsData::ThirdParty { header, .. } => simple_summary(
            AprsActivityKind::ThirdParty,
            format!("{source} third-party traffic from {header}"),
        ),
        AprsData::Grid(grid) => {
            simple_summary(AprsActivityKind::Grid, format!("{source} grid {grid}"))
        }
        AprsData::RawGps(sentence) => simple_summary(
            AprsActivityKind::RawGps,
            format!("{source} GPS: {sentence}"),
        ),
        AprsData::StationCapabilities(capabilities) => simple_summary(
            AprsActivityKind::Capabilities,
            format!("{source} reports {} capabilities", capabilities.len()),
        ),
        AprsData::AgreloDfJr(_) => simple_summary(
            AprsActivityKind::DirectionFinding,
            format!("{source} direction-finding report"),
        ),
        AprsData::UserDefined { experiment, .. } => simple_summary(
            AprsActivityKind::UserDefined,
            format!("{source} user-defined experiment {experiment}"),
        ),
        AprsData::InvalidOrTest(_) => simple_summary(
            AprsActivityKind::Test,
            format!("{source} invalid/test payload"),
        ),
        AprsData::RawWeather { .. } => simple_summary(
            AprsActivityKind::RawWeather,
            format!("{source} legacy raw weather payload"),
        ),
    }
}

fn position_summary(source: &str, position: &AprsPosition) -> DecodedSummary {
    let suffix = if position.comment.is_empty() {
        String::new()
    } else {
        format!(" · {}", position.comment)
    };
    DecodedSummary {
        kind: AprsActivityKind::Position,
        summary: format!(
            "{source} position {:.5}, {:.5}{suffix}",
            position.latitude, position.longitude
        ),
        latitude: Some(position.latitude),
        longitude: Some(position.longitude),
        speed_knots: position.speed_knots,
        course_degrees: position.course_degrees,
    }
}

fn object_summary(source: &str, object: &AprsObject) -> DecodedSummary {
    positioned_entity_summary(
        AprsActivityKind::Object,
        source,
        "object",
        &object.name,
        object.live,
        &object.position,
    )
}

fn item_summary(source: &str, item: &AprsItem) -> DecodedSummary {
    positioned_entity_summary(
        AprsActivityKind::Item,
        source,
        "item",
        &item.name,
        item.live,
        &item.position,
    )
}

fn positioned_entity_summary(
    kind: AprsActivityKind,
    source: &str,
    label: &str,
    name: &str,
    live: bool,
    position: &AprsPosition,
) -> DecodedSummary {
    DecodedSummary {
        kind,
        summary: format!(
            "{source} {label} {name} {} at {:.5}, {:.5}",
            if live { "live" } else { "killed" },
            position.latitude,
            position.longitude
        ),
        latitude: Some(position.latitude),
        longitude: Some(position.longitude),
        speed_knots: position.speed_knots,
        course_degrees: position.course_degrees,
    }
}

fn simple_summary(kind: AprsActivityKind, summary: String) -> DecodedSummary {
    DecodedSummary {
        kind,
        summary,
        latitude: None,
        longitude: None,
        speed_knots: None,
        course_degrees: None,
    }
}

fn tnc2_packet(source: &str, destination: &str, path: &[String], info: &[u8]) -> String {
    let path_suffix = if path.is_empty() {
        String::new()
    } else {
        format!(",{}", path.join(","))
    };
    format!(
        "{source}>{destination}{path_suffix}:{}",
        String::from_utf8_lossy(info)
    )
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn unix_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn take_identifier(next: &mut u64) -> u64 {
    let value = *next;
    *next = next.wrapping_add(1).max(1);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn validates_receive_only_and_transmit_configs() -> TestResult {
        let receive_only = AprsSessionConfig::default().validate()?;
        assert!(
            receive_only.source.is_none(),
            "blank station must be RX-only"
        );

        let configured = AprsSessionConfig {
            station_callsign: "k1abc-7".to_owned(),
            ..AprsSessionConfig::default()
        }
        .validate()?;
        assert_eq!(
            configured.source.map(|source| source.to_string()),
            Some("K1ABC-7".to_owned())
        );
        assert_eq!(configured.path.len(), 2);
        Ok(())
    }

    #[test]
    fn rejects_invalid_kiss_ranges_and_symbols() {
        let invalid_delay = AprsSessionConfig {
            tx_delay_10ms: 121,
            ..AprsSessionConfig::default()
        };
        assert!(
            invalid_delay.validate().is_err(),
            "oversized delay rejected"
        );

        let invalid_symbol = AprsSessionConfig {
            symbol_code: "🚗".to_owned(),
            ..AprsSessionConfig::default()
        };
        assert!(
            invalid_symbol.validate().is_err(),
            "non-ASCII symbol rejected"
        );
    }

    #[test]
    fn received_position_is_exactly_journaled_and_updates_station() -> TestResult {
        let source = Ax25Address::new("N0CALL", 7)?;
        let packet =
            build_aprs_position_report_packet(&source, 35.25, -97.75, '/', '>', "mobile", &[]);
        let raw = build_ax25(&packet);
        let mut store = AprsActivityStore::default();
        store.begin_start(AprsSessionConfig::default());
        store.mark_active();
        let record = store.push_received_ax25(raw.clone());

        assert_eq!(record.direction, AprsActivityDirection::Rx);
        assert_eq!(record.kind, AprsActivityKind::Position);
        assert_eq!(record.raw_ax25, raw);
        assert_eq!(record.source.as_deref(), Some("N0CALL-7"));
        assert_eq!(record.latitude, Some(35.25));
        assert_eq!(store.snapshot(None).stations.len(), 1);
        Ok(())
    }

    #[test]
    fn message_send_builds_then_journals_exact_ax25() -> TestResult {
        let mut store = AprsActivityStore::default();
        store.begin_start(AprsSessionConfig {
            station_callsign: "K1ABC-7".to_owned(),
            ..AprsSessionConfig::default()
        });
        store.mark_active();
        let packet = store.build_message("W1AW", "hello", Some("42"))?;
        let expected = build_ax25(&packet);
        let record = store.push_transmitted(&packet);

        assert_eq!(record.direction, AprsActivityDirection::Tx);
        assert_eq!(record.kind, AprsActivityKind::Message);
        assert_eq!(record.raw_ax25, expected);
        assert!(record.raw_packet.contains(":W1AW     :hello{42"));
        Ok(())
    }

    #[test]
    fn incremental_snapshot_reports_evicted_gap() {
        let mut store = AprsActivityStore::default();
        store.begin_start(AprsSessionConfig::default());
        for index in 0..=ACTIVITY_CAPACITY {
            store.push_system(&format!("row {index}"));
        }
        let snapshot = store.snapshot(Some(1));
        assert!(snapshot.history_truncated, "old sequence must report a gap");
        assert_eq!(snapshot.activities.len(), ACTIVITY_CAPACITY);
        assert!(snapshot.status.dropped_activities > 0);
    }
}
