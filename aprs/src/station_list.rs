//! Bounded heard-station tracking with immutable entries and caller-supplied time.
//!
//! See the [station storage example](crate#beacon-scheduling-and-station-storage).

use std::cmp::Reverse;
use std::collections::{HashMap, hash_map};
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use ax25_codec::{Ax25Address, DigipeaterPath};

use crate::packet::AprsData;
use crate::position::AprsPosition;
use crate::units::{Latitude, Longitude};
use crate::weather::{AprsPositionlessWeatherReport, AprsWeather};

/// Spherical distance model's mean Earth radius in kilometres.
const EARTH_RADIUS_KM: f64 = 6_371.0;

/// Invalid station-query radius, rejected before traversal or allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StationRadiusError {
    /// Radius is NaN or infinite.
    NonFinite {
        /// Rejected kilometres.
        kilometers: f64,
    },
    /// Radius is below zero.
    Negative {
        /// Rejected kilometres.
        kilometers: f64,
    },
}

impl std::fmt::Display for StationRadiusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite { kilometers } => write!(
                formatter,
                "station radius must be finite; received {kilometers} kilometres"
            ),
            Self::Negative { kilometers } => write!(
                formatter,
                "station radius must be nonnegative; received {kilometers} kilometres"
            ),
        }
    }
}
impl std::error::Error for StationRadiusError {}

/// A finite, nonnegative station-query radius in kilometres.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct StationRadius(f64);
impl StationRadius {
    /// Validate a radius, accepting zero and every finite nonnegative value.
    ///
    /// # Errors
    ///
    /// Returns [`StationRadiusError::NonFinite`] for NaN/infinity or
    /// [`StationRadiusError::Negative`] for a negative value.
    pub fn new(kilometers: f64) -> Result<Self, StationRadiusError> {
        if !kilometers.is_finite() {
            return Err(StationRadiusError::NonFinite { kilometers });
        }
        if kilometers < 0.0 {
            return Err(StationRadiusError::Negative { kilometers });
        }
        Ok(Self(kilometers))
    }
    /// Return the checked radius in kilometres.
    #[must_use]
    pub const fn kilometers(self) -> f64 {
        self.0
    }
}
impl TryFrom<f64> for StationRadius {
    type Error = StationRadiusError;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<StationRadius> for f64 {
    fn from(value: StationRadius) -> Self {
        value.0
    }
}

impl std::fmt::Display for StationRadius {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}
impl std::fmt::LowerExp for StationRadius {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerExp::fmt(&self.0, formatter)
    }
}
impl std::fmt::UpperExp for StationRadius {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperExp::fmt(&self.0, formatter)
    }
}

/// A bounded map of stations, keyed by complete callsign and SSID.
#[derive(Debug)]
pub struct StationList {
    stations: HashMap<Ax25Address, StationEntry>,
    max_entries: NonZeroUsize,
    max_age: Duration,
}

/// One station's latest processed data and nonregressing heard timestamp.
///
/// Instances come from [`StationList`]. Immutable access keeps their address
/// consistent with the map key. A cloned entry is an independent snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationEntry {
    address: Ax25Address,
    last_heard: Instant,
    position: Option<AprsPosition>,
    last_status: Option<String>,
    last_weather: Option<AprsWeather>,
    last_positionless_weather: Option<AprsPositionlessWeatherReport>,
    packet_count: u32,
    last_path: DigipeaterPath,
}

impl StationEntry {
    /// Borrow the complete station identity, including its SSID.
    #[must_use]
    pub const fn address(&self) -> &Ax25Address {
        &self.address
    }
    /// Return the greatest supplied heard timestamp for this station.
    #[must_use]
    pub const fn last_heard(&self) -> Instant {
        self.last_heard
    }
    /// Borrow the latest station position; object/item positions remain separate.
    #[must_use]
    pub const fn position(&self) -> Option<&AprsPosition> {
        self.position.as_ref()
    }
    /// Borrow the latest station status text.
    #[must_use]
    pub fn last_status(&self) -> Option<&str> {
        self.last_status.as_deref()
    }
    /// Borrow the latest embedded or standalone weather observation.
    #[must_use]
    pub const fn last_weather(&self) -> Option<&AprsWeather> {
        self.last_weather.as_ref()
    }
    /// Borrow the latest standalone report, retaining its timestamp and comment.
    #[must_use]
    pub const fn last_positionless_weather(&self) -> Option<&AprsPositionlessWeatherReport> {
        self.last_positionless_weather.as_ref()
    }
    /// Return the packet counter, saturating at `u32::MAX`.
    #[must_use]
    pub const fn packet_count(&self) -> u32 {
        self.packet_count
    }
    /// Borrow the last complete validated digipeater path.
    #[must_use]
    pub const fn last_path(&self) -> &DigipeaterPath {
        &self.last_path
    }
    /// Return elapsed age, saturating to zero for a regressing caller clock.
    #[must_use]
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_heard)
    }
}

/// Borrowed station entries in unspecified map order, without allocation.
#[derive(Debug, Clone)]
pub struct StationIter<'a>(hash_map::Values<'a, Ax25Address, StationEntry>);
impl<'a> Iterator for StationIter<'a> {
    type Item = &'a StationEntry;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl ExactSizeIterator for StationIter<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }
}
impl std::iter::FusedIterator for StationIter<'_> {}
impl<'a> IntoIterator for &'a StationList {
    type Item = &'a StationEntry;
    type IntoIter = StationIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl StationList {
    /// Create a list with positive capacity and a caller-defined maximum age.
    ///
    /// No clock is read. A zero maximum age expires every entry whose heard
    /// timestamp is at or before a subsequent purge call; future entries remain.
    #[must_use]
    pub fn new(max_entries: NonZeroUsize, max_age: Duration) -> Self {
        Self {
            stations: HashMap::new(),
            max_entries,
            max_age,
        }
    }
    /// Record a parsed packet and its validated source/path at caller time.
    ///
    /// The payload/path follow processing order. A regressing clock cannot
    /// shorten the heard timestamp or expiry interval. Counters saturate.
    /// A new station beyond capacity evicts the oldest timestamp; ties have
    /// unspecified order. Retained station records never exceed capacity.
    pub fn update(
        &mut self,
        source: &Ax25Address,
        data: &AprsData,
        path: &DigipeaterPath,
        now: Instant,
    ) {
        let entry = self
            .stations
            .entry(source.clone())
            .or_insert_with(|| StationEntry {
                address: source.clone(),
                last_heard: now,
                position: None,
                last_status: None,
                last_weather: None,
                last_positionless_weather: None,
                packet_count: 0,
                last_path: DigipeaterPath::empty(),
            });
        entry.last_heard = entry.last_heard.max(now);
        entry.packet_count = entry.packet_count.saturating_add(1);
        if &entry.last_path != path {
            entry.last_path.clone_from(path);
        }
        match data {
            AprsData::Position(position) => {
                if let Some(weather) = position.weather() {
                    entry.last_weather = Some(weather.clone());
                }
                entry.position = Some(position.clone());
            }
            AprsData::Status(status) => entry.last_status = Some(status.text().to_owned()),
            AprsData::PositionlessWeather(report) => {
                entry.last_weather = Some(report.weather().clone());
                entry.last_positionless_weather = Some(report.clone());
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
            | AprsData::InvalidOrTest(_)
            | AprsData::RawWeather { .. } => {}
        }
        if self.stations.len() > self.max_entries.get() {
            self.evict_oldest();
        }
    }
    /// Borrow one complete station identity.
    #[must_use]
    pub fn get(&self, address: &Ax25Address) -> Option<&StationEntry> {
        self.stations.get(address)
    }
    /// Borrow entries in unspecified map order, without allocation.
    #[must_use]
    pub fn iter(&self) -> StationIter<'_> {
        StationIter(self.stations.values())
    }
    /// Allocate a borrowed-entry view sorted newest first; timestamp ties are unspecified.
    #[must_use]
    pub fn recent(&self) -> Vec<&StationEntry> {
        let mut entries: Vec<_> = self.iter().collect();
        entries.sort_by_key(|entry| Reverse(entry.last_heard));
        entries
    }
    /// Borrow stations within an inclusive spherical radius, without allocating.
    ///
    /// Only stations with a position are considered. Coordinates and radius
    /// are validated before traversal; distances use a 6371 km spherical Earth.
    pub fn nearby(
        &self,
        latitude: Latitude,
        longitude: Longitude,
        radius: StationRadius,
    ) -> impl Iterator<Item = &StationEntry> {
        self.iter().filter(move |entry| {
            entry.position.as_ref().is_some_and(|position| {
                haversine_km(
                    latitude.as_degrees(),
                    longitude.as_degrees(),
                    position.latitude().as_degrees(),
                    position.longitude().as_degrees(),
                ) <= radius.kilometers()
            })
        })
    }
    /// Remove entries at or past maximum age, retaining future timestamps on clock reversal.
    pub fn purge_expired(&mut self, now: Instant) {
        let max_age = self.max_age;
        self.stations.retain(|_, entry| {
            now.checked_duration_since(entry.last_heard)
                .is_none_or(|age| age < max_age)
        });
    }
    /// Return the number of retained entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stations.len()
    }
    /// Return whether the list has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }
    fn evict_oldest(&mut self) {
        if let Some(address) = self
            .stations
            .iter()
            .min_by_key(|(_, entry)| entry.last_heard)
            .map(|(key, _)| key.clone())
        {
            let _removed = self.stations.remove(&address);
        }
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (lat1.to_radians().cos() * lat2.to_radians().cos())
        .mul_add((d_lon / 2.0).sin().powi(2), (d_lat / 2.0).sin().powi(2));
    2.0 * EARTH_RADIUS_KM * a.clamp(0.0, 1.0).sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::AprsSymbol;
    type TestResult = Result<(), Box<dyn std::error::Error>>;
    fn list(capacity: usize, age: Duration) -> Result<StationList, Box<dyn std::error::Error>> {
        Ok(StationList::new(
            NonZeroUsize::new(capacity).ok_or("positive fixture capacity")?,
            age,
        ))
    }
    fn position(latitude: f64, longitude: f64) -> Result<AprsData, crate::AprsError> {
        Ok(AprsData::Position(AprsPosition::new(
            Latitude::new(latitude)?,
            Longitude::new(longitude)?,
            AprsSymbol::CAR,
        )))
    }
    #[test]
    fn typed_identity_path_and_immutable_iteration_preserve_state() -> TestResult {
        let mut stations = list(100, Duration::from_secs(60))?;
        assert!(stations.is_empty());
        let first = Ax25Address::new("N0CALL", 1)?;
        let second = Ax25Address::new("N0CALL", 2)?;
        let now = Instant::now();
        let data = position(35.0, -97.0)?;
        let path = DigipeaterPath::new(vec![ax25_codec::RouteEntry::new("WIDE1", 1)?])?;
        stations.update(&first, &data, &path, now);
        stations.update(&second, &data, &DigipeaterPath::empty(), now);
        stations.update(&first, &data, &path, now);
        let entry = stations.get(&first).ok_or("first SSID")?;
        assert_eq!(entry.address(), &first);
        assert_eq!(entry.packet_count(), 2);
        assert_eq!(entry.last_path(), &path);
        assert!(entry.position().is_some());
        assert_eq!(
            entry.age(now + Duration::from_secs(10)),
            Duration::from_secs(10)
        );
        let empty = DigipeaterPath::empty();
        stations.update(&first, &data, &empty, now);
        assert_eq!(
            stations.get(&first).ok_or("retained first")?.last_path(),
            &empty
        );
        assert_eq!(stations.iter().len(), 2);
        assert_eq!((&stations).into_iter().count(), 2);
        assert_eq!(
            stations
                .iter()
                .map(StationEntry::address)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2
        );
        Ok(())
    }
    #[test]
    fn status_weather_and_messages_update_only_their_station_fields() -> TestResult {
        let mut stations = list(10, Duration::from_secs(60))?;
        let source = Ax25Address::new("W1AW", 0)?;
        let now = Instant::now();
        let empty = DigipeaterPath::empty();
        stations.update(&source, &position(35.0, -97.0)?, &empty, now);
        stations.update(&source, &AprsData::from_bytes(b">Sunny")?, &empty, now);
        stations.update(
            &source,
            &AprsData::from_bytes(b"_10092345c180s010g...t072h55")?,
            &empty,
            now,
        );
        stations.update(
            &source,
            &AprsData::from_bytes(b":W1AW     :Hello")?,
            &empty,
            now,
        );
        let entry = stations.get(&source).ok_or("station")?;
        assert_eq!(entry.last_status(), Some("Sunny"));
        assert!(entry.last_weather().is_some());
        assert!(entry.last_positionless_weather().is_some());
        assert!(entry.position().is_some());
        assert_eq!(entry.packet_count(), 4);
        Ok(())
    }
    #[test]
    fn capacity_recent_order_and_expiry_use_nonregressing_time() -> TestResult {
        let mut stations = list(2, Duration::from_secs(60))?;
        let base = Instant::now();
        let empty = DigipeaterPath::empty();
        let first = Ax25Address::new("FIRST", 0)?;
        let second = Ax25Address::new("SECOND", 0)?;
        let third = Ax25Address::new("THIRD", 0)?;
        let data = position(35.0, -97.0)?;
        stations.update(&first, &data, &empty, base);
        stations.update(&second, &data, &empty, base + Duration::from_secs(10));
        stations.update(&third, &data, &empty, base + Duration::from_secs(20));
        assert!(stations.get(&first).is_none());
        assert_eq!(stations.len(), 2);
        assert_eq!(
            stations.recent().first().ok_or("recent station")?.address(),
            &third
        );
        stations.update(&second, &data, &empty, base);
        assert_eq!(
            stations.get(&second).ok_or("second station")?.last_heard(),
            base + Duration::from_secs(10)
        );
        stations.purge_expired(base);
        assert_eq!(stations.len(), 2);
        stations.purge_expired(base + Duration::from_secs(69));
        assert!(stations.get(&second).is_some());
        stations.purge_expired(base + Duration::from_secs(70));
        assert!(stations.get(&second).is_none());
        assert_eq!(stations.len(), 1);
        stations.purge_expired(base + Duration::from_secs(80));
        assert!(stations.is_empty());
        let mut immediate = list(1, Duration::ZERO)?;
        immediate.update(&first, &data, &empty, base + Duration::from_secs(1));
        immediate.purge_expired(base);
        assert_eq!(immediate.len(), 1);
        immediate.purge_expired(base + Duration::from_secs(1));
        assert!(immediate.is_empty());
        Ok(())
    }
    #[test]
    fn nearby_requires_finite_domains_and_keeps_antipodes_finite() -> TestResult {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1] {
            assert!(StationRadius::new(value).is_err());
        }
        let mut stations = list(3, Duration::from_secs(60))?;
        let now = Instant::now();
        let empty = DigipeaterPath::empty();
        let close = Ax25Address::new("CLOSE", 0)?;
        let far = Ax25Address::new("FAR", 0)?;
        stations.update(&close, &position(35.01, -97.01)?, &empty, now);
        stations.update(&far, &position(40.0, -80.0)?, &empty, now);
        let nearby: Vec<_> = stations
            .nearby(
                Latitude::new(35.0)?,
                Longitude::new(-97.0)?,
                StationRadius::new(10.0)?,
            )
            .collect();
        assert_eq!(nearby.len(), 1);
        assert_eq!(nearby.first().ok_or("nearby station")?.address(), &close);
        assert!(haversine_km(35.0, -97.0, 35.0, -97.0).abs() < 0.001);
        assert!((haversine_km(40.7128, -74.0060, 51.5074, -0.1278) - 5_570.0).abs() < 50.0);
        for (lat, lon) in [(0.0, 0.0), (35.264_389, 45.0), (89.999_999, 179.999_999)] {
            let distance = haversine_km(lat, lon, -lat, lon - 180.0);
            assert!(distance.is_finite());
            assert!(
                std::f64::consts::PI
                    .mul_add(-EARTH_RADIUS_KM, distance)
                    .abs()
                    < 0.001
            );
        }
        Ok(())
    }
}
