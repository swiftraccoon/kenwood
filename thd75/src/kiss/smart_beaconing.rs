//! `SmartBeaconing` algorithm for adaptive APRS beacon timing.
//!
//! Implements the `HamHUD` `SmartBeaconing` algorithm, which adjusts beacon
//! interval based on speed and course changes:
//! - Stopped or slow: beacon every `slow_rate` seconds
//! - Fast: beacon every `fast_rate` seconds (linearly interpolated)
//! - Course change: immediate beacon if heading changed > `turn_threshold`
//!
//! Per Operating Tips §14: `SmartBeaconing` settings are Menu 540-547.

use std::time::{Duration, Instant};

/// Configuration for the `SmartBeaconing` algorithm.
///
/// Matches the TH-D75 Menu 540-547 settings.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartBeaconingConfig {
    /// Speed threshold below which `slow_rate` is used (km/h). Default: 5.
    pub low_speed_kmh: f64,
    /// Speed at/above which `fast_rate` is used (km/h). Default: 70.
    pub high_speed_kmh: f64,
    /// Beacon interval when stopped/slow (seconds). Default: 1800 (30 min).
    pub slow_rate_secs: u32,
    /// Beacon interval at high speed (seconds). Default: 180 (3 min).
    pub fast_rate_secs: u32,
    /// Minimum heading change to trigger a turn beacon (degrees). Default: 28.
    pub turn_threshold_deg: f64,
    /// Minimum time between turn-triggered beacons (seconds). Default: 15.
    pub turn_time_secs: u32,
}

impl Default for SmartBeaconingConfig {
    fn default() -> Self {
        Self {
            low_speed_kmh: 5.0,
            high_speed_kmh: 70.0,
            slow_rate_secs: 1800,
            fast_rate_secs: 180,
            turn_threshold_deg: 28.0,
            turn_time_secs: 15,
        }
    }
}

/// `SmartBeaconing` algorithm for adaptive APRS position beacon timing.
///
/// Adjusts beacon interval based on speed and course changes:
/// - Stopped or slow: beacon every `slow_rate` seconds
/// - Fast: beacon every `fast_rate` seconds
/// - Course change: immediate beacon if heading changed > `turn_threshold`
///
/// Per Operating Tips §14: `SmartBeaconing` settings are Menu 540-547.
#[derive(Debug)]
pub struct SmartBeaconing {
    /// Algorithm parameters.
    config: SmartBeaconingConfig,
    /// When the last beacon was sent.
    last_beacon_time: Option<Instant>,
    /// Course (heading) at the time of the last beacon.
    last_course: Option<f64>,
    /// Speed at the time of the last beacon.
    last_speed: Option<f64>,
}

impl SmartBeaconing {
    /// Create a new `SmartBeaconing` instance with the given configuration.
    #[must_use]
    pub const fn new(config: SmartBeaconingConfig) -> Self {
        Self {
            config,
            last_beacon_time: None,
            last_course: None,
            last_speed: None,
        }
    }

    /// Check if a beacon should be sent now, given current speed and course.
    ///
    /// Returns `true` if a beacon is due. The caller should call
    /// [`beacon_sent`](Self::beacon_sent) after transmitting.
    #[must_use]
    pub fn should_beacon(&mut self, speed_kmh: f64, course_deg: f64) -> bool {
        let now = Instant::now();

        // First beacon: always send.
        let Some(last_time) = self.last_beacon_time else {
            return true;
        };

        let elapsed = now.duration_since(last_time);
        let interval = Duration::from_secs(u64::from(self.compute_interval(speed_kmh)));

        // Time-based beacon: interval expired.
        if elapsed >= interval {
            return true;
        }

        // Turn-based beacon: heading change exceeds threshold AND
        // minimum turn time has elapsed AND we're above low speed.
        if speed_kmh > self.config.low_speed_kmh
            && let Some(last_course) = self.last_course
        {
            let turn = heading_delta(last_course, course_deg);
            if turn >= self.config.turn_threshold_deg
                && elapsed >= Duration::from_secs(u64::from(self.config.turn_time_secs))
            {
                return true;
            }
        }

        false
    }

    /// Mark that a beacon was just sent. Updates the internal state
    /// with the current time, course, and speed.
    pub fn beacon_sent(&mut self) {
        self.last_beacon_time = Some(Instant::now());
    }

    /// Mark that a beacon was just sent with the given speed and course.
    ///
    /// This variant stores the course and speed for turn detection.
    pub fn beacon_sent_with(&mut self, speed_kmh: f64, course_deg: f64) {
        self.last_beacon_time = Some(Instant::now());
        self.last_course = Some(course_deg);
        self.last_speed = Some(speed_kmh);
    }

    /// Get the current recommended interval in seconds.
    ///
    /// Based on the last known speed, or `slow_rate` if no speed data.
    #[must_use]
    pub fn current_interval_secs(&self) -> u32 {
        self.last_speed
            .map_or(self.config.slow_rate_secs, |s| self.compute_interval(s))
    }

    /// Compute the beacon interval for a given speed.
    ///
    /// Linear interpolation between `slow_rate` at `low_speed` and
    /// `fast_rate` at `high_speed`.
    fn compute_interval(&self, speed_kmh: f64) -> u32 {
        if speed_kmh <= self.config.low_speed_kmh {
            return self.config.slow_rate_secs;
        }
        if speed_kmh >= self.config.high_speed_kmh {
            return self.config.fast_rate_secs;
        }

        // Linear interpolation.
        let speed_range = self.config.high_speed_kmh - self.config.low_speed_kmh;
        let rate_range =
            f64::from(self.config.slow_rate_secs) - f64::from(self.config.fast_rate_secs);
        let fraction = (speed_kmh - self.config.low_speed_kmh) / speed_range;

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let interval = fraction.mul_add(-rate_range, f64::from(self.config.slow_rate_secs)) as u32;
        interval
    }
}

/// Compute the absolute heading change between two courses (0-360),
/// accounting for the wraparound at 360/0.
fn heading_delta(a: f64, b: f64) -> f64 {
    let mut delta = (b - a).abs();
    if delta > 180.0 {
        delta = 360.0 - delta;
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let cfg = SmartBeaconingConfig::default();
        assert!((cfg.low_speed_kmh - 5.0).abs() < f64::EPSILON);
        assert!((cfg.high_speed_kmh - 70.0).abs() < f64::EPSILON);
        assert_eq!(cfg.slow_rate_secs, 1800);
        assert_eq!(cfg.fast_rate_secs, 180);
        assert!((cfg.turn_threshold_deg - 28.0).abs() < f64::EPSILON);
        assert_eq!(cfg.turn_time_secs, 15);
    }

    #[test]
    fn first_beacon_always_true() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.should_beacon(0.0, 0.0));
    }

    #[test]
    fn interval_at_low_speed() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.compute_interval(0.0), 1800);
        assert_eq!(sb.compute_interval(5.0), 1800);
    }

    #[test]
    fn interval_at_high_speed() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.compute_interval(70.0), 180);
        assert_eq!(sb.compute_interval(100.0), 180);
    }

    #[test]
    fn interval_interpolation_midpoint() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // Midpoint speed = (5 + 70) / 2 = 37.5 km/h
        // Midpoint rate = (1800 + 180) / 2 = 990 secs
        let interval = sb.compute_interval(37.5);
        assert!((f64::from(interval) - 990.0).abs() < 2.0);
    }

    #[test]
    fn current_interval_without_speed_data() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.current_interval_secs(), 1800);
    }

    #[test]
    fn current_interval_with_speed_data() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        sb.last_speed = Some(70.0);
        assert_eq!(sb.current_interval_secs(), 180);
    }

    #[test]
    fn beacon_sent_updates_state() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.last_beacon_time.is_none());
        sb.beacon_sent();
        assert!(sb.last_beacon_time.is_some());
    }

    #[test]
    fn beacon_sent_with_stores_course_and_speed() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        sb.beacon_sent_with(50.0, 270.0);
        assert!((sb.last_speed.unwrap() - 50.0).abs() < f64::EPSILON);
        assert!((sb.last_course.unwrap() - 270.0).abs() < f64::EPSILON);
    }

    #[test]
    fn no_beacon_immediately_after_send() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // First beacon is always true.
        assert!(sb.should_beacon(0.0, 0.0));
        sb.beacon_sent_with(0.0, 0.0);

        // Immediately after, should not beacon (interval not elapsed).
        assert!(!sb.should_beacon(0.0, 0.0));
    }

    #[test]
    fn heading_delta_simple() {
        assert!((heading_delta(0.0, 90.0) - 90.0).abs() < f64::EPSILON);
        assert!((heading_delta(90.0, 0.0) - 90.0).abs() < f64::EPSILON);
    }

    #[test]
    fn heading_delta_wraparound() {
        // 350 to 10 = 20 degrees, not 340.
        assert!((heading_delta(350.0, 10.0) - 20.0).abs() < f64::EPSILON);
        assert!((heading_delta(10.0, 350.0) - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn heading_delta_opposite() {
        assert!((heading_delta(0.0, 180.0) - 180.0).abs() < f64::EPSILON);
        assert!((heading_delta(90.0, 270.0) - 180.0).abs() < f64::EPSILON);
    }

    #[test]
    fn turn_beacon_not_triggered_at_low_speed() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig {
            turn_time_secs: 0, // No minimum turn time for test simplicity.
            ..SmartBeaconingConfig::default()
        });

        // Send initial beacon heading north.
        assert!(sb.should_beacon(3.0, 0.0));
        sb.beacon_sent_with(3.0, 0.0);

        // Large heading change but at low speed — should NOT trigger.
        assert!(!sb.should_beacon(3.0, 90.0));
    }

    #[test]
    fn turn_beacon_triggered_at_high_speed() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig {
            turn_time_secs: 0, // No minimum turn time for test simplicity.
            ..SmartBeaconingConfig::default()
        });

        // Send initial beacon heading north at high speed.
        assert!(sb.should_beacon(75.0, 0.0));
        sb.beacon_sent_with(75.0, 0.0);

        // Course change above turn_threshold (28 deg) at high speed
        // should trigger an immediate beacon.
        assert!(sb.should_beacon(75.0, 45.0));
    }

    #[test]
    fn turn_beacon_below_threshold_no_trigger() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig {
            turn_time_secs: 0,
            ..SmartBeaconingConfig::default()
        });

        // Send initial beacon heading north at high speed.
        assert!(sb.should_beacon(75.0, 0.0));
        sb.beacon_sent_with(75.0, 0.0);

        // Course change below turn_threshold (28 deg) should NOT trigger.
        assert!(!sb.should_beacon(75.0, 20.0));
    }
}
