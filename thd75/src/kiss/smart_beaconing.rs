//! `SmartBeaconing` algorithm for adaptive APRS beacon timing.
//!
//! Implements the `HamHUD` `SmartBeaconing` algorithm v2.1 by Tony Arnerich
//! (KD7TA) and Jason Townsend (KD7TA), which adjusts beacon interval based
//! on speed and course changes:
//! - Stopped or slow: beacon every `slow_rate` seconds
//! - Fast: beacon every `fast_rate` seconds (linearly interpolated)
//! - Course change: immediate beacon if heading changed more than the
//!   **speed-dependent** turn threshold, computed as:
//!
//!   ```text
//!   turn_threshold = turn_min + (turn_slope * 10) / speed_kmh
//!   ```
//!
//! This makes slow-moving stations less likely to emit turn-triggered
//! beacons from small steering inputs, while fast-moving stations beacon
//! on relatively small heading changes.
//!
//! Per Operating Tips §14 and User Manual Chapter 14, the TH-D75 exposes
//! eight parameters via Menu 540-547:
//!
//! | Menu | Name | Default | Our field |
//! |-----:|------|--------:|-----------|
//! | 540 | L Spd        | 5 km/h   | `low_speed_kmh` |
//! | 541 | H Spd        | 70 km/h  | `high_speed_kmh` |
//! | 542 | L Rate       | 30 min   | `slow_rate_secs` |
//! | 543 | H Rate       | 180 s    | `fast_rate_secs` |
//! | 544 | Turn Slope   | 26       | `turn_slope` |
//! | 545 | Turn Thresh  | 28°      | `turn_min_deg` |
//! | 546 | Turn Time    | 30 s     | `turn_time_secs` |

use std::time::{Duration, Instant};

/// Configuration for the `SmartBeaconing` algorithm.
///
/// Matches the TH-D75 Menu 540-547 settings and the `HamHUD` `SmartBeaconing`
/// v2.1 parameter set.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartBeaconingConfig {
    /// Speed threshold below which `slow_rate` is used (km/h). Default: 5.
    /// Corresponds to TH-D75 Menu 540 (L Spd).
    pub low_speed_kmh: f64,
    /// Speed at/above which `fast_rate` is used (km/h). Default: 70.
    /// Corresponds to TH-D75 Menu 541 (H Spd).
    pub high_speed_kmh: f64,
    /// Beacon interval when stopped/slow (seconds). Default: 1800 (30 min).
    /// Corresponds to TH-D75 Menu 542 (L Rate).
    pub slow_rate_secs: u32,
    /// Beacon interval at high speed (seconds). Default: 180 (3 min).
    /// Corresponds to TH-D75 Menu 543 (H Rate).
    pub fast_rate_secs: u32,
    /// Turn slope scalar used in the speed-dependent turn threshold
    /// formula `turn_min + (turn_slope * 10) / speed_kmh`. Default: 26.
    /// Corresponds to TH-D75 Menu 544 (Turn Slope).
    pub turn_slope: u16,
    /// Minimum heading change for a turn beacon, in degrees. Applied as
    /// the `turn_min` term in the threshold formula. Default: 28.
    /// Corresponds to TH-D75 Menu 545 (Turn Thresh).
    pub turn_min_deg: f64,
    /// Minimum time between turn-triggered beacons (seconds). Default: 15.
    /// Corresponds to TH-D75 Menu 546 (Turn Time).
    pub turn_time_secs: u32,
}

impl From<crate::types::McpSmartBeaconingConfig> for SmartBeaconingConfig {
    /// Convert a radio-memory `SmartBeaconing` config (mph/seconds) to
    /// the runtime form (`km/h` / seconds / `f64`).
    fn from(mcp: crate::types::McpSmartBeaconingConfig) -> Self {
        const MPH_TO_KMH: f64 = 1.609_344;
        Self {
            low_speed_kmh: f64::from(mcp.low_speed) * MPH_TO_KMH,
            high_speed_kmh: f64::from(mcp.high_speed) * MPH_TO_KMH,
            slow_rate_secs: u32::from(mcp.slow_rate),
            fast_rate_secs: u32::from(mcp.fast_rate),
            turn_slope: u16::from(mcp.turn_slope),
            turn_min_deg: f64::from(mcp.turn_angle),
            turn_time_secs: u32::from(mcp.turn_time),
        }
    }
}

impl Default for SmartBeaconingConfig {
    fn default() -> Self {
        Self {
            low_speed_kmh: 5.0,
            high_speed_kmh: 70.0,
            slow_rate_secs: 1800,
            fast_rate_secs: 180,
            turn_slope: 26,
            turn_min_deg: 28.0,
            turn_time_secs: 15,
        }
    }
}

/// Reason a `SmartBeacon` was triggered at a given moment.
///
/// Returned by [`SmartBeaconing::beacon_reason`]. Useful for logging or
/// UI display — `SmartBeaconing` has three distinct trigger conditions,
/// and users often want to know which one fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconReason {
    /// First beacon of the session — nothing sent yet.
    First,
    /// Time-based interval elapsed since the previous beacon.
    TimeExpired,
    /// Heading change exceeded the (speed-dependent) turn threshold.
    Turn,
}

/// `SmartBeaconing` runtime state.
///
/// The algorithm starts in [`BeaconState::Uninitialized`] and transitions
/// to [`BeaconState::Running`] the first time a beacon is recorded via
/// [`SmartBeaconing::beacon_sent_with`]. The state holds the last
/// beacon's course and speed so subsequent turn-threshold checks have
/// the reference data they need.
#[derive(Debug, Clone, PartialEq)]
pub enum BeaconState {
    /// No beacon has been sent yet — first call to `should_beacon` /
    /// `beacon_reason` will return `Some(BeaconReason::First)`.
    Uninitialized,
    /// At least one beacon has been sent. Carries the timestamp and
    /// the (course, speed) recorded at that beacon.
    Running {
        /// When the last beacon was transmitted.
        last_beacon_time: Instant,
        /// Course in degrees at the last beacon, or `None` if the
        /// caller used [`SmartBeaconing::beacon_sent`] without
        /// supplying one.
        last_course: Option<f64>,
        /// Speed in km/h at the last beacon, or `None` if unknown.
        last_speed: Option<f64>,
    },
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
    /// Runtime state machine.
    state: BeaconState,
}

impl SmartBeaconing {
    /// Create a new `SmartBeaconing` instance with the given configuration.
    #[must_use]
    pub const fn new(config: SmartBeaconingConfig) -> Self {
        Self {
            config,
            state: BeaconState::Uninitialized,
        }
    }

    /// Return a snapshot of the current state machine.
    #[must_use]
    pub const fn state(&self) -> &BeaconState {
        &self.state
    }

    /// Check if a beacon should be sent now, given current speed and course.
    #[must_use]
    pub fn should_beacon(&mut self, speed_kmh: f64, course_deg: f64) -> bool {
        self.beacon_reason(speed_kmh, course_deg).is_some()
    }

    /// Classify why (if at all) a beacon is due at the current speed and
    /// course. Returns `None` if no beacon should be sent yet, otherwise
    /// a [`BeaconReason`] identifying which condition tripped.
    #[must_use]
    pub fn beacon_reason(&mut self, speed_kmh: f64, course_deg: f64) -> Option<BeaconReason> {
        let now = Instant::now();

        // First beacon: always send.
        let BeaconState::Running {
            last_beacon_time,
            last_course,
            ..
        } = self.state
        else {
            return Some(BeaconReason::First);
        };

        let elapsed = now.duration_since(last_beacon_time);
        let interval = Duration::from_secs(u64::from(self.compute_interval(speed_kmh)));

        if elapsed >= interval {
            return Some(BeaconReason::TimeExpired);
        }

        if speed_kmh > self.config.low_speed_kmh
            && let Some(last_course) = last_course
        {
            let turn = heading_delta(last_course, course_deg);
            let threshold = self.current_turn_threshold(speed_kmh);
            if turn >= threshold
                && elapsed >= Duration::from_secs(u64::from(self.config.turn_time_secs))
            {
                return Some(BeaconReason::Turn);
            }
        }

        None
    }

    /// Compute the current turn threshold (in degrees) for the given speed
    /// using the `HamHUD` formula:
    ///
    /// ```text
    /// turn_threshold = turn_min + (turn_slope * 10) / speed_kmh
    /// ```
    #[must_use]
    pub fn current_turn_threshold(&self, speed_kmh: f64) -> f64 {
        if speed_kmh <= self.config.low_speed_kmh {
            return f64::INFINITY;
        }
        self.config.turn_min_deg + (f64::from(self.config.turn_slope) * 10.0) / speed_kmh
    }

    /// Mark that a beacon was just sent. Updates the internal state
    /// with the current time, preserving any previously-recorded course
    /// and speed.
    pub fn beacon_sent(&mut self) {
        let (prev_course, prev_speed) = match self.state {
            BeaconState::Uninitialized => (None, None),
            BeaconState::Running {
                last_course,
                last_speed,
                ..
            } => (last_course, last_speed),
        };
        self.state = BeaconState::Running {
            last_beacon_time: Instant::now(),
            last_course: prev_course,
            last_speed: prev_speed,
        };
    }

    /// Mark that a beacon was just sent with the given speed and course.
    pub fn beacon_sent_with(&mut self, speed_kmh: f64, course_deg: f64) {
        self.state = BeaconState::Running {
            last_beacon_time: Instant::now(),
            last_course: Some(course_deg),
            last_speed: Some(speed_kmh),
        };
    }

    /// Get the current recommended interval in seconds.
    ///
    /// Based on the last known speed, or `slow_rate` if no speed data.
    #[must_use]
    pub fn current_interval_secs(&self) -> u32 {
        match &self.state {
            BeaconState::Running {
                last_speed: Some(s),
                ..
            } => self.compute_interval(*s),
            _ => self.config.slow_rate_secs,
        }
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
        assert_eq!(cfg.turn_slope, 26);
        assert!((cfg.turn_min_deg - 28.0).abs() < f64::EPSILON);
        assert_eq!(cfg.turn_time_secs, 15);
    }

    #[test]
    fn turn_threshold_matches_hamhud_formula() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // Stopped / slow: threshold is infinity, no turn beacon possible.
        assert!(sb.current_turn_threshold(0.0).is_infinite());
        assert!(sb.current_turn_threshold(5.0).is_infinite());

        // At 60 km/h: 28 + (26 * 10) / 60 ≈ 32.333
        let t60 = sb.current_turn_threshold(60.0);
        assert!((t60 - (28.0 + 260.0 / 60.0)).abs() < 1e-9);

        // At 10 km/h: 28 + 26 = 54 degrees (need big turn to beacon).
        let t10 = sb.current_turn_threshold(10.0);
        assert!((t10 - 54.0).abs() < 1e-9);

        // At 120 km/h (high speed): 28 + (260 / 120) ≈ 30.167
        let threshold_120 = sb.current_turn_threshold(120.0);
        assert!((threshold_120 - (28.0 + 260.0 / 120.0)).abs() < 1e-9);
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
        sb.beacon_sent_with(70.0, 0.0);
        assert_eq!(sb.current_interval_secs(), 180);
    }

    #[test]
    fn beacon_sent_updates_state() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(matches!(sb.state(), BeaconState::Uninitialized));
        sb.beacon_sent();
        assert!(matches!(sb.state(), BeaconState::Running { .. }));
    }

    #[test]
    fn beacon_sent_with_stores_course_and_speed() {
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        sb.beacon_sent_with(50.0, 270.0);
        match sb.state() {
            BeaconState::Running {
                last_course,
                last_speed,
                ..
            } => {
                assert!((last_speed.unwrap() - 50.0).abs() < f64::EPSILON);
                assert!((last_course.unwrap() - 270.0).abs() < f64::EPSILON);
            }
            BeaconState::Uninitialized => panic!("expected Running state"),
        }
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
