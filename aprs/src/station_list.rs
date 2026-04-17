//! APRS station list tracker.
//!
//! Maintains a list of APRS stations heard on the network, with their
//! latest position, status, weather data, packet count, and digipeater
//! path. Supports spatial queries via the haversine formula.
//!
//! # Time handling
//!
//! Per the crate-level convention, this module is sans-io and never calls
//! `std::time::Instant::now()` internally. Every stateful method that
//! reads the clock accepts a `now: Instant` parameter; callers (typically
//! the tokio shell) read the wall clock once per iteration and thread
//! it down.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::packet::AprsData;
use crate::position::AprsPosition;
use crate::weather::AprsWeather;

/// Earth's mean radius in kilometres (WGS-84 volumetric mean).
const EARTH_RADIUS_KM: f64 = 6_371.0;

/// Tracks APRS stations heard on the network.
#[derive(Debug)]
pub struct StationList {
    /// Stations indexed by callsign.
    stations: HashMap<String, StationEntry>,
    /// Maximum number of entries to keep.
    max_entries: usize,
    /// Maximum age before a station is considered expired.
    max_age: Duration,
}

/// A single station's latest state.
#[derive(Debug, Clone)]
pub struct StationEntry {
    /// Station callsign (key).
    pub callsign: String,
    /// When this station was last heard.
    pub last_heard: Instant,
    /// Most recent position.
    pub position: Option<AprsPosition>,
    /// Most recent status text.
    pub last_status: Option<String>,
    /// Most recent weather report.
    pub last_weather: Option<AprsWeather>,
    /// Total number of packets received from this station.
    pub packet_count: u32,
    /// Digipeater path from the most recent packet.
    pub last_path: Vec<String>,
}

impl StationList {
    /// Create a new station list with the given capacity and age limits.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // HashMap::new() is not const
    pub fn new(max_entries: usize, max_age: Duration) -> Self {
        Self {
            stations: HashMap::new(),
            max_entries,
            max_age,
        }
    }

    /// Update the station list from a parsed APRS packet.
    ///
    /// Creates a new entry if the station has not been seen before, or
    /// updates the existing entry with fresh data. The `now` parameter
    /// stamps the entry's `last_heard` field — callers in the tokio
    /// shell read the wall clock once per iteration and thread it down.
    pub fn update(&mut self, source: &str, data: &AprsData, path: &[String], now: Instant) {
        let entry = self
            .stations
            .entry(source.to_owned())
            .or_insert_with(|| StationEntry {
                callsign: source.to_owned(),
                last_heard: now,
                position: None,
                last_status: None,
                last_weather: None,
                packet_count: 0,
                last_path: Vec::new(),
            });

        entry.last_heard = now;
        entry.packet_count = entry.packet_count.saturating_add(1);
        entry.last_path = path.to_vec();

        match data {
            AprsData::Position(pos) => {
                // A weather-station position (symbol `_`) carries embedded
                // wx data too — record both.
                if let Some(ref wx) = pos.weather {
                    entry.last_weather = Some(wx.clone());
                }
                entry.position = Some(pos.clone());
            }
            AprsData::Status(status) => {
                entry.last_status = Some(status.text.clone());
            }
            AprsData::Message(_)
            | AprsData::Object(_)
            | AprsData::Item(_)
            | AprsData::Telemetry(_)
            | AprsData::Query(_)
            | AprsData::ThirdParty { .. }
            | AprsData::Grid(_)
            | AprsData::RawGps(_)
            | AprsData::StationCapabilities(_)
            | AprsData::AgreloDfJr(_)
            | AprsData::UserDefined { .. }
            | AprsData::InvalidOrTest(_) => {
                // These frame types don't update the station's own
                // position or status.
            }
            AprsData::Weather(wx) => {
                entry.last_weather = Some(wx.clone());
            }
        }

        // Evict oldest entry if over capacity.
        if self.stations.len() > self.max_entries {
            self.evict_oldest();
        }
    }

    /// Get a station entry by callsign.
    #[must_use]
    pub fn get(&self, callsign: &str) -> Option<&StationEntry> {
        self.stations.get(callsign)
    }

    /// Get all stations sorted by last heard (most recent first).
    #[must_use]
    pub fn recent(&self) -> Vec<&StationEntry> {
        let mut entries: Vec<&StationEntry> = self.stations.values().collect();
        entries.sort_by(|a, b| b.last_heard.cmp(&a.last_heard));
        entries
    }

    /// Get stations within a radius (in km) of a position.
    ///
    /// Uses the haversine formula for great-circle distance calculation.
    /// Only stations with a known position are considered.
    #[must_use]
    pub fn nearby(&self, lat: f64, lon: f64, radius_km: f64) -> Vec<&StationEntry> {
        self.stations
            .values()
            .filter(|e| {
                e.position.as_ref().is_some_and(|pos| {
                    haversine_km(lat, lon, pos.latitude, pos.longitude) <= radius_km
                })
            })
            .collect()
    }

    /// Remove expired entries (older than `max_age`).
    ///
    /// The `now` parameter is compared against each entry's `last_heard`
    /// timestamp; entries older than `max_age` are evicted.
    pub fn purge_expired(&mut self, now: Instant) {
        let max_age = self.max_age;
        self.stations
            .retain(|_, e| now.duration_since(e.last_heard) < max_age);
    }

    /// Total number of stations tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stations.len()
    }

    /// Returns `true` if the station list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    /// Remove the oldest station entry to make room.
    fn evict_oldest(&mut self) {
        if let Some(oldest_key) = self
            .stations
            .iter()
            .min_by_key(|(_, e)| e.last_heard)
            .map(|(k, _)| k.clone())
        {
            let _removed = self.stations.remove(&oldest_key);
        }
    }
}

/// Haversine great-circle distance between two lat/lon points in kilometres.
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let lat1_r = lat1.to_radians();
    let lat2_r = lat2.to_radians();

    let a = (lat1_r.cos() * lat2_r.cos())
        .mul_add((d_lon / 2.0).sin().powi(2), (d_lat / 2.0).sin().powi(2));
    let c = 2.0 * a.sqrt().asin();
    EARTH_RADIUS_KM * c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::AprsMessage;
    use crate::packet::{AprsDataExtension, PositionAmbiguity};
    use crate::status::AprsStatus;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn make_position(lat: f64, lon: f64) -> AprsData {
        AprsData::Position(AprsPosition {
            latitude: lat,
            longitude: lon,
            symbol_table: '/',
            symbol_code: '>',
            speed_knots: None,
            course_degrees: None,
            comment: String::new(),
            weather: None,
            extensions: AprsDataExtension::default(),
            mice_message: None,
            mice_altitude_m: None,
            ambiguity: PositionAmbiguity::None,
        })
    }

    fn make_status(text: &str) -> AprsData {
        AprsData::Status(AprsStatus {
            text: text.to_owned(),
        })
    }

    fn make_weather() -> AprsData {
        AprsData::Weather(AprsWeather {
            wind_direction: Some(180),
            wind_speed: Some(10),
            wind_gust: None,
            temperature: Some(72),
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: None,
            humidity: Some(55),
            pressure: None,
        })
    }

    #[test]
    fn new_station_list_is_empty() {
        let sl = StationList::new(100, Duration::from_secs(3600));
        assert!(sl.is_empty());
        assert_eq!(sl.len(), 0);
    }

    #[test]
    fn update_creates_and_increments() -> TestResult {
        let mut sl = StationList::new(100, Duration::from_secs(3600));
        let pos = make_position(35.0, -97.0);
        let t0 = Instant::now();
        sl.update("N0CALL", &pos, &["WIDE1-1".to_owned()], t0);

        assert_eq!(sl.len(), 1);
        let entry = sl.get("N0CALL").ok_or("expected N0CALL entry")?;
        assert_eq!(entry.callsign, "N0CALL");
        assert_eq!(entry.packet_count, 1);
        assert!(entry.position.is_some());
        assert_eq!(entry.last_path, vec!["WIDE1-1".to_owned()]);

        // Second update increments count.
        sl.update("N0CALL", &pos, &[], t0);
        let entry = sl.get("N0CALL").ok_or("expected N0CALL entry")?;
        assert_eq!(entry.packet_count, 2);
        Ok(())
    }

    #[test]
    fn update_status_and_weather() -> TestResult {
        let mut sl = StationList::new(100, Duration::from_secs(3600));
        let t0 = Instant::now();

        sl.update("WX1", &make_status("Sunny"), &[], t0);
        let entry = sl.get("WX1").ok_or("expected WX1 entry")?;
        assert_eq!(entry.last_status.as_deref(), Some("Sunny"));
        assert!(entry.last_weather.is_none());

        sl.update("WX1", &make_weather(), &[], t0);
        let entry = sl.get("WX1").ok_or("expected WX1 entry")?;
        assert!(entry.last_weather.is_some());
        assert_eq!(entry.packet_count, 2);
        Ok(())
    }

    #[test]
    fn message_does_not_update_position_or_status() -> TestResult {
        let mut sl = StationList::new(100, Duration::from_secs(3600));
        let pos = make_position(35.0, -97.0);
        let t0 = Instant::now();
        sl.update("N0CALL", &pos, &[], t0);

        let msg = AprsData::Message(AprsMessage {
            addressee: "W1AW".to_owned(),
            text: "Hello".to_owned(),
            message_id: None,
            reply_ack: None,
        });
        sl.update("N0CALL", &msg, &[], t0);

        let entry = sl.get("N0CALL").ok_or("expected N0CALL entry")?;
        // Position should still be the original.
        assert!(entry.position.is_some());
        assert_eq!(entry.packet_count, 2);
        Ok(())
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let sl = StationList::new(100, Duration::from_secs(3600));
        assert!(sl.get("NOBODY").is_none());
    }

    #[test]
    fn recent_returns_sorted_by_last_heard() -> TestResult {
        let mut sl = StationList::new(100, Duration::from_secs(3600));
        let pos = make_position(35.0, -97.0);
        let t0 = Instant::now();

        sl.update("FIRST", &pos, &[], t0);
        sl.update("SECOND", &pos, &[], t0 + Duration::from_millis(1));
        sl.update("THIRD", &pos, &[], t0 + Duration::from_millis(2));

        let recent = sl.recent();
        assert_eq!(recent.len(), 3);
        // Most recent should be last updated.
        let first = recent.first().ok_or("expected at least one entry")?;
        assert_eq!(first.callsign, "THIRD");
        Ok(())
    }

    #[test]
    fn nearby_filters_by_distance() -> TestResult {
        let mut sl = StationList::new(100, Duration::from_secs(3600));
        let t0 = Instant::now();

        // Two stations: one close, one far.
        sl.update("CLOSE", &make_position(35.01, -97.01), &[], t0);
        sl.update("FAR", &make_position(40.0, -80.0), &[], t0);
        // One station with no position.
        sl.update("NOPOS", &make_status("No GPS"), &[], t0);

        let nearby = sl.nearby(35.0, -97.0, 10.0);
        assert_eq!(nearby.len(), 1);
        let first = nearby.first().ok_or("expected a nearby entry")?;
        assert_eq!(first.callsign, "CLOSE");
        Ok(())
    }

    #[test]
    fn evict_oldest_when_over_capacity() {
        let mut sl = StationList::new(2, Duration::from_secs(3600));
        let pos = make_position(35.0, -97.0);
        let t0 = Instant::now();

        sl.update("FIRST", &pos, &[], t0);
        sl.update("SECOND", &pos, &[], t0 + Duration::from_millis(1));
        assert_eq!(sl.len(), 2);

        // Adding a third should evict the oldest (FIRST).
        sl.update("THIRD", &pos, &[], t0 + Duration::from_millis(2));
        assert_eq!(sl.len(), 2);
        assert!(sl.get("FIRST").is_none());
        assert!(sl.get("SECOND").is_some());
        assert!(sl.get("THIRD").is_some());
    }

    #[test]
    fn haversine_zero_distance() {
        let d = haversine_km(35.0, -97.0, 35.0, -97.0);
        assert!(d.abs() < 0.001);
    }

    #[test]
    fn haversine_known_distance() {
        // New York to London: approximately 5,570 km.
        let d = haversine_km(40.7128, -74.0060, 51.5074, -0.1278);
        assert!((d - 5_570.0).abs() < 50.0);
    }

    #[test]
    fn purge_expired_is_no_op_for_fresh_entries() {
        let mut sl = StationList::new(100, Duration::from_secs(3600));
        let t0 = Instant::now();
        sl.update("N0CALL", &make_position(35.0, -97.0), &[], t0);
        sl.purge_expired(t0);
        assert_eq!(sl.len(), 1);
    }

    #[test]
    fn purge_expired_removes_old_entries() {
        // Use a reasonable max_age. We can now drive the clock directly
        // via the injected `now` parameter instead of backdating
        // `last_heard` after the fact.
        let mut sl = StationList::new(100, Duration::from_secs(60));
        let t0 = Instant::now();
        sl.update("N0CALL", &make_position(35.0, -97.0), &[], t0);
        assert_eq!(sl.len(), 1);

        // Advance the clock past max_age.
        let future = t0 + Duration::from_secs(120);
        sl.purge_expired(future);
        assert_eq!(sl.len(), 0, "expired entry should have been purged");
    }
}
