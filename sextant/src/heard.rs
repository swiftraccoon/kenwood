// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Heard-station list — per-callsign history of received traffic.
//!
//! Timestamps are UTC wall clock (not `Instant`) so they survive
//! restarts and render as real dates. The persisted file carries the
//! timestamp, last message, and last position alongside the counters.

use std::fmt::Write as _;
use std::path::PathBuf;

use time::OffsetDateTime;

/// Maximum stations retained; oldest-heard evicted past this.
const MAX_STATIONS: usize = 100;

/// One station observed on the reflector.
#[derive(Debug, Clone)]
pub(crate) struct HeardStation {
    /// Source callsign (display form).
    pub(crate) callsign: String,
    /// When this station was last observed (UTC). `None` only for
    /// entries restored from a legacy heard-file without timestamps.
    pub(crate) last_heard: Option<OffsetDateTime>,
    /// Number of voice streams observed from this station.
    pub(crate) stream_count: u32,
    /// Most recent slow-data text message, if any.
    pub(crate) last_message: Option<String>,
    /// Most recent decoded position (latitude, longitude), if any.
    pub(crate) last_gps: Option<(f64, f64)>,
}

/// Recency-ordered heard-station list, bounded to `MAX_STATIONS`.
#[derive(Debug, Default)]
pub(crate) struct HeardList {
    stations: Vec<HeardStation>,
}

impl HeardList {
    /// Record a voice stream from `callsign` at `now`.
    pub(crate) fn record_stream(&mut self, callsign: &str, now: OffsetDateTime) {
        if let Some(s) = self.stations.iter_mut().find(|s| s.callsign == callsign) {
            s.last_heard = Some(now);
            s.stream_count = s.stream_count.saturating_add(1);
        } else {
            self.stations.push(HeardStation {
                callsign: callsign.to_owned(),
                last_heard: Some(now),
                stream_count: 1,
                last_message: None,
                last_gps: None,
            });
        }
        self.evict_oldest();
    }

    /// Attach a slow-data message to the named station.
    pub(crate) fn record_message(&mut self, callsign: &str, message: String) {
        if let Some(s) = self.stations.iter_mut().find(|s| s.callsign == callsign) {
            s.last_message = Some(message);
        }
    }

    /// Attach a decoded position to the named station.
    pub(crate) fn record_gps(&mut self, callsign: &str, lat: f64, lon: f64) {
        if let Some(s) = self.stations.iter_mut().find(|s| s.callsign == callsign) {
            s.last_gps = Some((lat, lon));
        }
    }

    /// Stations ordered most-recently-heard first; legacy entries
    /// without a timestamp sort last.
    pub(crate) fn recent(&self) -> Vec<&HeardStation> {
        let mut out: Vec<&HeardStation> = self.stations.iter().collect();
        out.sort_by(|a, b| b.last_heard.cmp(&a.last_heard));
        out
    }

    /// Drop the oldest-heard station(s) past the cap (timestamp-less
    /// legacy entries count as oldest).
    fn evict_oldest(&mut self) {
        while self.stations.len() > MAX_STATIONS {
            let Some((idx, _)) = self
                .stations
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.last_heard)
            else {
                break;
            };
            let _removed = self.stations.remove(idx);
        }
    }

    /// Persisted-file path: `<config dir>/sextant/heard.tsv`.
    fn path() -> Option<PathBuf> {
        let mut dir = dirs_next::config_dir()?;
        dir.push("sextant");
        Some(dir.join("heard.tsv"))
    }

    /// Serialize one station per line:
    /// `callsign<TAB>count<TAB>unix_secs<TAB>lat<TAB>lon<TAB>message`.
    /// Empty fields are written as empty strings; the message is
    /// sanitized so it can never contain a tab or newline.
    fn serialize(&self) -> String {
        let mut body = String::new();
        for s in &self.stations {
            body.push_str(&s.callsign);
            body.push('\t');
            body.push_str(&s.stream_count.to_string());
            body.push('\t');
            if let Some(ts) = s.last_heard {
                body.push_str(&ts.unix_timestamp().to_string());
            }
            body.push('\t');
            if let Some((lat, lon)) = s.last_gps {
                let _w = write!(body, "{lat:.6}\t{lon:.6}");
            } else {
                body.push('\t');
            }
            body.push('\t');
            if let Some(msg) = &s.last_message {
                for ch in msg.chars() {
                    body.push(if ch == '\t' || ch == '\n' { ' ' } else { ch });
                }
            }
            body.push('\n');
        }
        body
    }

    /// Parse the persisted format. Also accepts the legacy two-field
    /// `callsign<TAB>count` lines (no timestamp / message / position).
    fn deserialize(raw: &str) -> Self {
        let mut list = Self::default();
        for line in raw.lines() {
            let mut parts = line.split('\t');
            let (Some(callsign), Some(count)) = (parts.next(), parts.next()) else {
                continue;
            };
            if callsign.is_empty() {
                continue;
            }
            let last_heard = parts
                .next()
                .and_then(|f| f.parse::<i64>().ok())
                .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok());
            let lat = parts.next().and_then(|f| f.parse::<f64>().ok());
            let lon = parts.next().and_then(|f| f.parse::<f64>().ok());
            let last_message = parts
                .next()
                .filter(|m| !m.is_empty())
                .map(ToOwned::to_owned);
            list.stations.push(HeardStation {
                callsign: callsign.to_owned(),
                last_heard,
                stream_count: count.parse().unwrap_or(0),
                last_message,
                last_gps: lat.zip(lon),
            });
        }
        list
    }

    /// Load persisted stations. Falls back to empty on any error.
    pub(crate) fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::deserialize(&raw)
    }

    /// Persist the current station list. Logs and swallows IO errors —
    /// heard-list persistence must never block shutdown.
    pub(crate) fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        if let Err(e) = std::fs::write(&path, self.serialize()) {
            tracing::warn!(error = %e, "could not write heard-list");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn ts(secs: i64) -> Result<OffsetDateTime, Box<dyn std::error::Error>> {
        Ok(OffsetDateTime::from_unix_timestamp(secs)?)
    }

    #[test]
    fn record_stream_creates_then_increments() -> TestResult {
        let mut list = HeardList::default();
        let t0 = ts(1_800_000_000)?;
        list.record_stream("W1AW", t0);
        list.record_stream("W1AW", t0);
        let recent = list.recent();
        let first = recent.first().ok_or("no station recorded")?;
        assert_eq!(first.callsign, "W1AW");
        assert_eq!(first.stream_count, 2);
        assert_eq!(first.last_heard, Some(t0));
        Ok(())
    }

    #[test]
    fn recent_orders_by_last_heard_desc() -> TestResult {
        let mut list = HeardList::default();
        list.record_stream("OLD", ts(1_800_000_000)?);
        list.record_stream("NEW", ts(1_800_000_001)?);
        let recent = list.recent();
        let first = recent.first().ok_or("empty")?;
        assert_eq!(first.callsign, "NEW", "most recent station sorts first");
        Ok(())
    }

    #[test]
    fn record_message_attaches_to_station() -> TestResult {
        let mut list = HeardList::default();
        list.record_stream("W1AW", ts(1_800_000_000)?);
        list.record_message("W1AW", "CQ".into());
        let recent = list.recent();
        let first = recent.first().ok_or("empty")?;
        assert_eq!(first.last_message.as_deref(), Some("CQ"));
        Ok(())
    }

    #[test]
    fn persistence_roundtrips_timestamp_message_and_position() -> TestResult {
        let mut list = HeardList::default();
        let t0 = ts(1_800_000_000)?;
        list.record_stream("W1AW", t0);
        list.record_stream("W1AW", t0);
        list.record_message("W1AW", "73 de CT".into());
        list.record_gps("W1AW", 41.714_775, -72.727_260);
        list.record_stream("K4XYZ", ts(1_800_000_100)?);

        let restored = HeardList::deserialize(&list.serialize());
        let recent = restored.recent();
        assert_eq!(recent.len(), 2, "both stations restored");
        let w1aw = recent
            .iter()
            .find(|s| s.callsign == "W1AW")
            .ok_or("W1AW restored")?;
        assert_eq!(w1aw.stream_count, 2, "count survives");
        assert_eq!(w1aw.last_heard, Some(t0), "timestamp survives");
        assert_eq!(
            w1aw.last_message.as_deref(),
            Some("73 de CT"),
            "message survives"
        );
        let (lat, lon) = w1aw.last_gps.ok_or("position survives")?;
        assert!((lat - 41.714_775).abs() < 1e-5, "lat survives, got {lat}");
        assert!((lon - -72.727_260).abs() < 1e-5, "lon survives, got {lon}");
        Ok(())
    }

    #[test]
    fn legacy_two_field_lines_still_parse() -> TestResult {
        let restored = HeardList::deserialize("W1AW\t3\nK4XYZ\t1\n");
        let recent = restored.recent();
        assert_eq!(recent.len(), 2, "legacy entries restored");
        let w1aw = recent
            .iter()
            .find(|s| s.callsign == "W1AW")
            .ok_or("W1AW restored")?;
        assert_eq!(w1aw.stream_count, 3);
        assert_eq!(
            w1aw.last_heard, None,
            "legacy entries carry no timestamp instead of a fake one"
        );
        Ok(())
    }

    #[test]
    fn message_with_tab_is_sanitized_not_corrupting() -> TestResult {
        let mut list = HeardList::default();
        list.record_stream("W1AW", ts(1_800_000_000)?);
        list.record_message("W1AW", "a\tb\nc".into());
        let restored = HeardList::deserialize(&list.serialize());
        let recent = restored.recent();
        let first = recent.first().ok_or("restored")?;
        assert_eq!(
            first.last_message.as_deref(),
            Some("a b c"),
            "tabs/newlines flattened, line structure intact"
        );
        Ok(())
    }
}
