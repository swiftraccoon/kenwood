// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Heard-station list — per-callsign history of received traffic.

use std::path::PathBuf;
use std::time::Instant;

/// Maximum stations retained; oldest-heard evicted past this.
const MAX_STATIONS: usize = 100;

/// One station observed on the reflector.
#[derive(Debug, Clone)]
pub(crate) struct HeardStation {
    /// Source callsign (display form).
    pub(crate) callsign: String,
    /// When this station was last observed.
    pub(crate) last_heard: Instant,
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
    pub(crate) fn record_stream(&mut self, callsign: &str, now: Instant) {
        if let Some(s) = self.stations.iter_mut().find(|s| s.callsign == callsign) {
            s.last_heard = now;
            s.stream_count = s.stream_count.saturating_add(1);
        } else {
            self.stations.push(HeardStation {
                callsign: callsign.to_owned(),
                last_heard: now,
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

    /// Stations ordered most-recently-heard first.
    pub(crate) fn recent(&self) -> Vec<&HeardStation> {
        let mut out: Vec<&HeardStation> = self.stations.iter().collect();
        out.sort_by(|a, b| b.last_heard.cmp(&a.last_heard));
        out
    }

    /// Drop the oldest-heard station(s) past the cap.
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

    /// Serialize to `callsign<TAB>stream_count` lines. Timing and the
    /// last message / position are session-local and not persisted.
    fn serialize(&self) -> String {
        let mut body = String::new();
        for s in &self.stations {
            body.push_str(&s.callsign);
            body.push('\t');
            body.push_str(&s.stream_count.to_string());
            body.push('\n');
        }
        body
    }

    /// Parse `callsign<TAB>stream_count` lines; `last_heard` resets to
    /// `now` since wall-clock instants don't survive a restart.
    fn deserialize(raw: &str, now: Instant) -> Self {
        let mut list = Self::default();
        for line in raw.lines() {
            let mut parts = line.split('\t');
            let (Some(callsign), Some(count)) = (parts.next(), parts.next()) else {
                continue;
            };
            if callsign.is_empty() {
                continue;
            }
            list.stations.push(HeardStation {
                callsign: callsign.to_owned(),
                last_heard: now,
                stream_count: count.parse().unwrap_or(0),
                last_message: None,
                last_gps: None,
            });
        }
        list
    }

    /// Load persisted stations. Falls back to empty on any error.
    pub(crate) fn load(now: Instant) -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::deserialize(&raw, now)
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

    #[test]
    fn record_stream_creates_then_increments() -> TestResult {
        let mut list = HeardList::default();
        let t0 = Instant::now();
        list.record_stream("W1AW", t0);
        list.record_stream("W1AW", t0);
        let recent = list.recent();
        let first = recent.first().ok_or("no station recorded")?;
        assert_eq!(first.callsign, "W1AW");
        assert_eq!(first.stream_count, 2);
        Ok(())
    }

    #[test]
    fn recent_orders_by_last_heard_desc() -> TestResult {
        let mut list = HeardList::default();
        let t0 = Instant::now();
        list.record_stream("OLD", t0);
        list.record_stream("NEW", t0 + std::time::Duration::from_secs(1));
        let recent = list.recent();
        let first = recent.first().ok_or("empty")?;
        assert_eq!(first.callsign, "NEW", "most recent station sorts first");
        Ok(())
    }

    #[test]
    fn record_message_attaches_to_station() -> TestResult {
        let mut list = HeardList::default();
        let t0 = Instant::now();
        list.record_stream("W1AW", t0);
        list.record_message("W1AW", "CQ".into());
        let recent = list.recent();
        let first = recent.first().ok_or("empty")?;
        assert_eq!(first.last_message.as_deref(), Some("CQ"));
        Ok(())
    }

    #[test]
    fn persistence_roundtrips_callsign_and_count() {
        let mut list = HeardList::default();
        let t0 = Instant::now();
        list.record_stream("W1AW", t0);
        list.record_stream("W1AW", t0);
        list.record_stream("K4XYZ", t0);

        let serialized = list.serialize();
        let restored = HeardList::deserialize(&serialized, t0);
        let recent = restored.recent();
        assert_eq!(recent.len(), 2, "both stations restored");
        let w1aw = recent.iter().find(|s| s.callsign == "W1AW");
        assert!(
            matches!(w1aw, Some(s) if s.stream_count == 2),
            "W1AW stream count must survive the round trip"
        );
    }
}
