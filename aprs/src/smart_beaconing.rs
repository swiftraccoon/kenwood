//! `SmartBeaconing` algorithm for adaptive APRS beacon timing.
//!
//! Implements the `HamHUD` `SmartBeaconing` algorithm v2.1 originated
//! by Tony Arnerich (KD7TA) and refined by Steve Bragg (KA9MVA), which
//! adjusts beacon interval based on speed and heading changes:
//!
//! - Stopped or slow: beacon every `slow_rate` seconds.
//! - Between the thresholds: interval = `fast_rate * high_speed / speed`.
//! - Fast: beacon every `fast_rate` seconds.
//! - Course change: immediate beacon if heading changed more than the
//!   **speed-dependent** turn threshold, computed as:
//!
//!   ```text
//!   turn_threshold = min(120°, turn_min + (turn_slope * 10) / speed_kmh)
//!   ```
//!
//! This makes slow-moving stations less likely to emit turn-triggered
//! beacons from small steering inputs, while fast-moving stations
//! beacon on relatively small heading changes.
//!
//! Per Operating Tips §14 and User Manual Chapter 14, the TH-D75 exposes
//! seven `SmartBeaconing` parameters via Menu 530-535. Low/high speed share
//! Menu 530:
//!
//! | Menu | Name        | V1.03 default             | Our field         |
//! |-----:|-------------|---------------------------|-------------------|
//! | 530  | Low Speed   | 5 in Menu 970 speed unit  | `low_speed()`     |
//! | 530  | High Speed  | 70 in Menu 970 speed unit | `high_speed()`    |
//! | 531  | Slow Rate   | 30 min                    | `slow_rate_secs()` |
//! | 532  | Fast Rate   | 120 s                     | `fast_rate_secs()` |
//! | 533  | Turn Angle  | 28°                       | `turn_minimum()`  |
//! | 534  | Turn Slope  | 26                        | `turn_slope()`    |
//! | 535  | Turn Time   | 60 s                      | `turn_time_secs()` |
//!
//! This runtime type uses km/h. Its default represents a TH-D75A V1.03
//! default configuration (Menu 970 = mi/h) converted without changing its
//! physical thresholds or corner-pegging behavior.
//!
//! # Time handling
//!
//! Per the crate-level convention, this module is sans-io and never calls
//! `std::time::Instant::now()` internally. Every stateful method accepts
//! a `now: Instant` parameter; callers (typically the tokio shell) read
//! the wall clock once per iteration and thread it down.

use std::time::{Duration, Instant};

use crate::error::AprsError;
use crate::units::{Heading, Speed};

/// Configuration for the `SmartBeaconing` algorithm.
///
/// Matches the TH-D75 Menu 530-535 settings and the `HamHUD`
/// `SmartBeaconing` v2.1 parameter set after normalization to km/h.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartBeaconingConfig {
    /// Speed threshold below which `slow_rate` is used, in km/h.
    /// TH-D75A V1.03 default: 5 mph = 8.04672 km/h (Menu 530).
    low_speed: Speed,
    /// Speed at/above which `fast_rate` is used, in km/h.
    /// TH-D75A V1.03 default: 70 mph = 112.65408 km/h (Menu 530).
    high_speed: Speed,
    /// Beacon interval when stopped/slow (seconds). Default: 1800 (30 min).
    /// Corresponds to TH-D75 Menu 531 (Slow Rate).
    slow_rate_secs: u32,
    /// Beacon interval at high speed (seconds). V1.03 default: 120.
    /// Corresponds to TH-D75 Menu 532 (Fast Rate).
    fast_rate_secs: u32,
    /// Turn-slope coefficient normalized for a km/h denominator in
    /// `turn_min + (turn_slope * 10) / speed_kmh`. TH-D75A V1.03 default:
    /// 26 × 1.609344 = 41.842944. Corresponds to Menu 534.
    turn_slope: f64,
    /// Minimum heading change for a turn beacon, in degrees. Applied as
    /// the `turn_min` term in the threshold formula. Default: 28.
    /// Corresponds to TH-D75 Menu 533 (Turn Angle).
    turn_minimum: Heading,
    /// Minimum time between turn-triggered beacons (seconds). Default: 60.
    /// Corresponds to TH-D75 Menu 535 (Turn Time).
    turn_time_secs: u32,
}

impl SmartBeaconingConfig {
    /// Create a validated `SmartBeaconing` configuration.
    ///
    /// `turn_slope` is normalized for a km/h denominator. The low threshold
    /// must be positive and no greater than the high threshold, beacon rates
    /// must be nonzero, and the minimum turn angle must not exceed the
    /// algorithm's 120-degree ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSmartBeaconingConfig`] when the values
    /// cannot define a finite, nonzero beacon schedule.
    pub fn new(
        low_speed: Speed,
        high_speed: Speed,
        slow_rate_secs: u32,
        fast_rate_secs: u32,
        turn_slope: f64,
        turn_minimum: Heading,
        turn_time_secs: u32,
    ) -> Result<Self, AprsError> {
        if low_speed.as_kmh() <= 0.0 {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "low speed must be greater than zero",
            ));
        }
        if high_speed < low_speed {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "high speed must be greater than or equal to low speed",
            ));
        }
        if slow_rate_secs == 0 {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "slow beacon rate must be at least one second",
            ));
        }
        if fast_rate_secs == 0 {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "fast beacon rate must be at least one second",
            ));
        }
        if !turn_slope.is_finite() || turn_slope < 0.0 {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "turn slope must be finite and non-negative",
            ));
        }
        let maximum_turn_adjustment = (turn_slope * 10.0) / low_speed.as_kmh();
        if !maximum_turn_adjustment.is_finite() {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "turn-slope calculation exceeds the supported range",
            ));
        }
        if turn_minimum.as_degrees() > 120.0 {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "minimum turn angle must not exceed 120 degrees",
            ));
        }

        let maximum_interpolated_interval =
            f64::from(fast_rate_secs) * (high_speed.as_kmh() / low_speed.as_kmh());
        if !maximum_interpolated_interval.is_finite()
            || maximum_interpolated_interval > f64::from(u32::MAX)
        {
            return Err(AprsError::InvalidSmartBeaconingConfig(
                "interpolated beacon interval exceeds the supported range",
            ));
        }

        Ok(Self {
            low_speed,
            high_speed,
            slow_rate_secs,
            fast_rate_secs,
            turn_slope,
            turn_minimum,
            turn_time_secs,
        })
    }

    /// Return the low-speed threshold.
    #[must_use]
    pub const fn low_speed(&self) -> Speed {
        self.low_speed
    }

    /// Return the high-speed threshold.
    #[must_use]
    pub const fn high_speed(&self) -> Speed {
        self.high_speed
    }

    /// Return the slow beacon interval in seconds.
    #[must_use]
    pub const fn slow_rate_secs(&self) -> u32 {
        self.slow_rate_secs
    }

    /// Return the fast beacon interval in seconds.
    #[must_use]
    pub const fn fast_rate_secs(&self) -> u32 {
        self.fast_rate_secs
    }

    /// Return the turn-slope coefficient normalized for km/h.
    #[must_use]
    pub const fn turn_slope(&self) -> f64 {
        self.turn_slope
    }

    /// Return the minimum turn angle.
    #[must_use]
    pub const fn turn_minimum(&self) -> Heading {
        self.turn_minimum
    }

    /// Return the minimum interval between turn-triggered beacons.
    #[must_use]
    pub const fn turn_time_secs(&self) -> u32 {
        self.turn_time_secs
    }
}

impl Default for SmartBeaconingConfig {
    fn default() -> Self {
        let low_speed = Speed::from_kmh(8.046_72)
            .unwrap_or_else(|_| unreachable!("the default low speed is valid"));
        let high_speed = Speed::from_kmh(112.654_08)
            .unwrap_or_else(|_| unreachable!("the default high speed is valid"));
        let turn_minimum =
            Heading::new(28.0).unwrap_or_else(|_| unreachable!("the default turn angle is valid"));
        Self::new(
            low_speed,
            high_speed,
            1800,
            120,
            41.842_944,
            turn_minimum,
            60,
        )
        .unwrap_or_else(|_| unreachable!("the default SmartBeaconing configuration is valid"))
    }
}

/// Reason a `SmartBeacon` was triggered at a given moment.
///
/// Returned by [`SmartBeaconing::beacon_reason`]. Useful for logging or
/// UI display: `SmartBeaconing` has three distinct trigger conditions,
/// and users often want to know which one fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconReason {
    /// First beacon of the session, with nothing sent yet.
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
/// beacon's heading and speed so subsequent turn-threshold checks have
/// the reference data they need.
#[derive(Debug, Clone, PartialEq)]
pub enum BeaconState {
    /// No beacon has been sent yet; the first call to `should_beacon` /
    /// `beacon_reason` will return `Some(BeaconReason::First)`.
    Uninitialized,
    /// At least one beacon has been sent. Carries the timestamp and
    /// the (heading, speed) recorded at that beacon.
    Running {
        /// When the last beacon was transmitted.
        last_beacon_time: Instant,
        /// Validated heading at the last beacon, or `None` if the caller used
        /// [`SmartBeaconing::beacon_sent`] or explicitly recorded that no
        /// heading measurement was available.
        last_heading: Option<Heading>,
        /// Speed in km/h at the last beacon, or `None` if unknown.
        last_speed: Option<Speed>,
    },
}

/// `SmartBeaconing` algorithm for adaptive APRS position beacon timing.
///
/// Adjusts beacon interval based on speed and heading changes:
/// - Stopped or slow: beacon every `slow_rate` seconds
/// - Fast: beacon every `fast_rate` seconds
/// - Course change: immediate beacon if heading changed > `turn_threshold`
///
/// Per Operating Tips §14: `SmartBeaconing` settings are Menu 530-535.
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

    /// Return the validated algorithm parameters.
    #[must_use]
    pub const fn config(&self) -> &SmartBeaconingConfig {
        &self.config
    }

    /// Check if a beacon should be sent now, given current speed and heading.
    ///
    /// `None` for `heading` means that no heading measurement is available,
    /// so only the time-based rules can trigger.
    ///
    /// `now` is the current wall-clock time, injected by the caller so
    /// this module remains sans-io.
    #[must_use]
    pub fn should_beacon(&self, speed: Speed, heading: Option<Heading>, now: Instant) -> bool {
        self.beacon_reason(speed, heading, now).is_some()
    }

    /// Classify why (if at all) a beacon is due at the current speed and
    /// heading. Returns `None` if no beacon should be sent yet, otherwise
    /// a [`BeaconReason`] identifying which condition tripped.
    ///
    /// [`Speed`] and [`Heading`] reject non-finite or out-of-range sensor
    /// values before they enter the state machine. `None` means no heading
    /// measurement is available for turn detection.
    ///
    /// `now` is the current wall-clock time, injected by the caller so
    /// this module remains sans-io.
    #[must_use]
    pub fn beacon_reason(
        &self,
        speed: Speed,
        heading: Option<Heading>,
        now: Instant,
    ) -> Option<BeaconReason> {
        // First beacon: always send.
        let BeaconState::Running {
            last_beacon_time,
            last_heading,
            ..
        } = self.state
        else {
            return Some(BeaconReason::First);
        };

        let elapsed = now.checked_duration_since(last_beacon_time)?;
        let interval = Duration::from_secs(u64::from(self.compute_interval(speed)));

        if elapsed >= interval {
            return Some(BeaconReason::TimeExpired);
        }

        if speed > self.config.low_speed
            && let Some(last_heading) = last_heading
            && let Some(heading) = heading
        {
            let turn = heading_delta(last_heading, heading);
            if let Some(threshold) = self.current_turn_threshold(speed)
                && turn >= threshold
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
    /// turn_threshold = min(120°, turn_min + (turn_slope * 10) / speed_kmh)
    /// ```
    ///
    /// Returns `None` at or below the low-speed threshold, where turn
    /// beacons are disabled.
    #[must_use]
    pub fn current_turn_threshold(&self, speed: Speed) -> Option<f64> {
        if speed <= self.config.low_speed {
            return None;
        }
        Some(
            (self.config.turn_minimum.as_degrees()
                + (self.config.turn_slope * 10.0) / speed.as_kmh())
            .min(120.0),
        )
    }

    /// Mark that a beacon was just sent. Updates the internal state
    /// with the supplied time, preserving any previously-recorded heading
    /// and speed.
    ///
    /// `now` is the wall-clock time at which the beacon was sent.
    pub const fn beacon_sent(&mut self, now: Instant) {
        let (previous_heading, previous_speed) = match self.state {
            BeaconState::Uninitialized => (None, None),
            BeaconState::Running {
                last_heading,
                last_speed,
                ..
            } => (last_heading, last_speed),
        };
        self.state = BeaconState::Running {
            last_beacon_time: now,
            last_heading: previous_heading,
            last_speed: previous_speed,
        };
    }

    /// Mark that a beacon was just sent with the given speed and heading.
    ///
    /// `None` for `heading` records that no heading measurement was
    /// available for this beacon.
    ///
    /// `now` is the wall-clock time at which the beacon was sent.
    pub const fn beacon_sent_with(&mut self, speed: Speed, heading: Option<Heading>, now: Instant) {
        self.state = BeaconState::Running {
            last_beacon_time: now,
            last_heading: heading,
            last_speed: Some(speed),
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
    /// V1.03 uses `fast_rate * high_speed / speed` between the low/high
    /// thresholds. Below low speed it uses `slow_rate`; at or above high
    /// speed it uses `fast_rate`.
    ///
    fn compute_interval(&self, speed: Speed) -> u32 {
        if speed.as_kmh() <= 0.0 || speed < self.config.low_speed {
            return self.config.slow_rate_secs;
        }
        if speed >= self.config.high_speed {
            return self.config.fast_rate_secs;
        }

        let interval = f64::from(self.config.fast_rate_secs)
            * (self.config.high_speed.as_kmh() / speed.as_kmh());

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "validated configuration bounds this finite positive value to u32, and the \
                      radio stores whole-second intervals"
        )]
        let interval = interval.round() as u32;
        interval
    }
}

/// Compute the absolute heading change between two headings (0-360),
/// accounting for the wraparound at 360/0.
fn heading_delta(a: Heading, b: Heading) -> f64 {
    let mut delta = (b.as_degrees() - a.as_degrees()).abs();
    if delta > 180.0 {
        delta = 360.0 - delta;
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn speed(kmh: f64) -> Speed {
        Speed::from_kmh(kmh).unwrap_or_else(|_| unreachable!("test speed is valid"))
    }

    fn heading(degrees: f64) -> Heading {
        Heading::new(degrees).unwrap_or_else(|_| unreachable!("test heading is valid"))
    }

    fn measured_heading(degrees: f64) -> Option<Heading> {
        Heading::new(degrees).ok()
    }

    fn config(
        low_speed_kmh: f64,
        high_speed_kmh: f64,
        slow_rate_secs: u32,
        fast_rate_secs: u32,
        turn_slope: f64,
        turn_minimum_degrees: f64,
        turn_time_secs: u32,
    ) -> SmartBeaconingConfig {
        SmartBeaconingConfig::new(
            speed(low_speed_kmh),
            speed(high_speed_kmh),
            slow_rate_secs,
            fast_rate_secs,
            turn_slope,
            heading(turn_minimum_degrees),
            turn_time_secs,
        )
        .unwrap_or_else(|_| unreachable!("test configuration is valid"))
    }

    fn immediate_turn_config() -> SmartBeaconingConfig {
        let defaults = SmartBeaconingConfig::default();
        SmartBeaconingConfig::new(
            defaults.low_speed(),
            defaults.high_speed(),
            defaults.slow_rate_secs(),
            defaults.fast_rate_secs(),
            defaults.turn_slope(),
            defaults.turn_minimum(),
            0,
        )
        .unwrap_or_else(|_| unreachable!("zero turn time remains a valid schedule"))
    }

    #[test]
    fn default_config_values() {
        let cfg = SmartBeaconingConfig::default();
        assert!((cfg.low_speed().as_kmh() - 8.046_72).abs() < f64::EPSILON);
        assert!((cfg.high_speed().as_kmh() - 112.654_08).abs() < f64::EPSILON);
        assert_eq!(cfg.slow_rate_secs(), 1800);
        assert_eq!(cfg.fast_rate_secs(), 120);
        assert!((cfg.turn_slope() - 41.842_944).abs() < f64::EPSILON);
        assert!((cfg.turn_minimum().as_degrees() - 28.0).abs() < f64::EPSILON);
        assert_eq!(cfg.turn_time_secs(), 60);
    }

    #[test]
    fn turn_threshold_matches_hamhud_formula() {
        let sb = SmartBeaconing::new(config(5.0, 70.0, 1800, 120, 24.0, 30.0, 60));
        assert_eq!(sb.current_turn_threshold(speed(0.0)), None);
        assert_eq!(sb.current_turn_threshold(speed(5.0)), None);

        // V1.03 manual example: 30 + (24 * 10) / 60 = 34 degrees.
        let t60 = sb
            .current_turn_threshold(speed(60.0))
            .unwrap_or_else(|| unreachable!("60 km/h is above the low threshold"));
        assert!((t60 - 34.0).abs() < 1e-9);

        // At 10 km/h: 30 + 24 = 54 degrees.
        let t10 = sb
            .current_turn_threshold(speed(10.0))
            .unwrap_or_else(|| unreachable!("10 km/h is above the low threshold"));
        assert!((t10 - 54.0).abs() < 1e-9);
    }

    #[test]
    fn turn_threshold_is_capped_at_120_degrees() {
        let sb = SmartBeaconing::new(config(2.0, 90.0, 1800, 120, 255.0, 90.0, 60));
        assert_eq!(sb.current_turn_threshold(speed(3.0)), Some(120.0));
    }

    #[test]
    fn first_beacon_always_true() {
        let t0 = Instant::now();
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.should_beacon(speed(0.0), measured_heading(0.0), t0));
    }

    #[test]
    fn interval_at_low_speed() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.compute_interval(speed(0.0)), 1800);
        assert_eq!(sb.compute_interval(speed(8.0)), 1800);
        // At exactly Low Speed, V1.03 applies the inverse-speed formula.
        assert_eq!(sb.compute_interval(speed(8.046_72)), 1680);
    }

    #[test]
    fn interval_at_high_speed() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.compute_interval(speed(112.654_08)), 120);
        assert_eq!(sb.compute_interval(speed(150.0)), 120);
    }

    #[test]
    fn interval_matches_v103_inverse_speed_examples() {
        let sb = SmartBeaconing::new(config(5.0, 70.0, 1800, 120, 24.0, 30.0, 60));
        assert_eq!(sb.compute_interval(speed(50.0)), 168);
        assert_eq!(sb.compute_interval(speed(30.0)), 280);
        assert_eq!(sb.compute_interval(speed(20.0)), 420);
        assert_eq!(sb.compute_interval(speed(10.0)), 840);
        assert_eq!(sb.compute_interval(speed(5.0)), 1680);
    }

    #[test]
    fn current_interval_without_speed_data() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.current_interval_secs(), 1800);
    }

    #[test]
    fn current_interval_with_speed_data() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        sb.beacon_sent_with(speed(120.0), measured_heading(0.0), t0);
        assert_eq!(sb.current_interval_secs(), 120);
    }

    #[test]
    fn beacon_sent_updates_state() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(matches!(sb.state(), BeaconState::Uninitialized));
        sb.beacon_sent(t0);
        assert!(matches!(sb.state(), BeaconState::Running { .. }));
    }

    #[test]
    fn beacon_sent_with_stores_heading_and_speed() -> TestResult {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        sb.beacon_sent_with(speed(50.0), measured_heading(270.0), t0);
        let BeaconState::Running {
            last_heading,
            last_speed,
            ..
        } = sb.state()
        else {
            return Err("expected Running state".into());
        };
        let recorded_speed = last_speed.ok_or("expected last_speed to be Some")?;
        let recorded_heading = last_heading.ok_or("expected last_heading to be Some")?;
        assert!((recorded_speed.as_kmh() - 50.0).abs() < f64::EPSILON);
        assert!((recorded_heading.as_degrees() - 270.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn no_beacon_immediately_after_send() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // First beacon is always true.
        assert!(sb.should_beacon(speed(0.0), measured_heading(0.0), t0));
        sb.beacon_sent_with(speed(0.0), measured_heading(0.0), t0);

        // Immediately after, should not beacon (interval not elapsed).
        assert!(!sb.should_beacon(speed(0.0), measured_heading(0.0), t0));
    }

    #[test]
    fn heading_delta_simple() {
        assert!((heading_delta(heading(0.0), heading(90.0)) - 90.0).abs() < f64::EPSILON);
        assert!((heading_delta(heading(90.0), heading(0.0)) - 90.0).abs() < f64::EPSILON);
    }

    #[test]
    fn heading_delta_wraparound() {
        // 350 to 10 = 20 degrees, not 340.
        assert!((heading_delta(heading(350.0), heading(10.0)) - 20.0).abs() < f64::EPSILON);
        assert!((heading_delta(heading(10.0), heading(350.0)) - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn heading_delta_opposite() {
        assert!((heading_delta(heading(0.0), heading(180.0)) - 180.0).abs() < f64::EPSILON);
        assert!((heading_delta(heading(90.0), heading(270.0)) - 180.0).abs() < f64::EPSILON);
    }

    #[test]
    fn turn_beacon_not_triggered_at_low_speed() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(immediate_turn_config());

        // Send initial beacon heading north.
        assert!(sb.should_beacon(speed(3.0), measured_heading(0.0), t0));
        sb.beacon_sent_with(speed(3.0), measured_heading(0.0), t0);

        // Large heading change but at low speed: should NOT trigger.
        assert!(!sb.should_beacon(speed(3.0), measured_heading(90.0), t0));
    }

    #[test]
    fn turn_beacon_triggered_at_high_speed() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(immediate_turn_config());

        // Send initial beacon heading north above High Speed.
        assert!(sb.should_beacon(speed(120.0), measured_heading(0.0), t0));
        sb.beacon_sent_with(speed(120.0), measured_heading(0.0), t0);

        // Course change above turn_threshold (28 deg) at high speed
        // should trigger an immediate beacon.
        assert!(sb.should_beacon(speed(120.0), measured_heading(45.0), t0));
    }

    #[test]
    fn turn_beacon_below_threshold_no_trigger() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(immediate_turn_config());

        // Send initial beacon heading north above High Speed.
        assert!(sb.should_beacon(speed(120.0), measured_heading(0.0), t0));
        sb.beacon_sent_with(speed(120.0), measured_heading(0.0), t0);

        // Course change below turn_threshold (28 deg) should NOT trigger.
        assert!(!sb.should_beacon(speed(120.0), measured_heading(20.0), t0));
    }

    #[test]
    fn time_expired_triggers_beacon_after_interval() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.should_beacon(speed(0.0), measured_heading(0.0), t0));
        sb.beacon_sent_with(speed(0.0), measured_heading(0.0), t0);

        // At low speed, slow_rate is 1800 secs. Advance past interval.
        let later = t0 + Duration::from_secs(1801);
        assert_eq!(
            sb.beacon_reason(speed(0.0), measured_heading(0.0), later),
            Some(BeaconReason::TimeExpired),
        );
    }

    #[test]
    fn invalid_sensor_measurements_cannot_enter_the_algorithm() {
        for invalid_speed in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            assert!(Speed::from_kmh(invalid_speed).is_err());
        }
        for invalid_heading in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 360.1] {
            assert!(Heading::new(invalid_heading).is_err());
        }
    }

    #[test]
    fn missing_heading_fires_no_turn_beacon() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(immediate_turn_config());
        sb.beacon_sent_with(speed(120.0), measured_heading(0.0), t0);
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(sb.beacon_reason(speed(120.0), None, t1), None);

        let mut without_reference = SmartBeaconing::new(immediate_turn_config());
        without_reference.beacon_sent_with(speed(120.0), None, t0);
        assert_eq!(
            without_reference.beacon_reason(speed(120.0), measured_heading(120.0), t1),
            None,
        );
    }

    #[test]
    fn regressing_clock_does_not_panic_or_trigger() {
        let earlier = Instant::now();
        let t0 = earlier + Duration::from_secs(1);
        let mut sb = SmartBeaconing::new(immediate_turn_config());
        sb.beacon_sent_with(speed(120.0), measured_heading(0.0), t0);
        assert_eq!(
            sb.beacon_reason(speed(120.0), measured_heading(120.0), earlier,),
            None,
        );
    }

    #[test]
    fn invalid_configurations_are_rejected() -> TestResult {
        let valid_low = speed(5.0);
        let valid_high = speed(70.0);
        let valid_turn = heading(28.0);

        assert!(
            SmartBeaconingConfig::new(speed(0.0), valid_high, 1800, 120, 26.0, valid_turn, 60,)
                .is_err()
        );
        assert!(
            SmartBeaconingConfig::new(valid_high, valid_low, 1800, 120, 26.0, valid_turn, 60,)
                .is_err()
        );
        for (slow_rate, fast_rate) in [(0, 120), (1800, 0)] {
            assert!(
                SmartBeaconingConfig::new(
                    valid_low, valid_high, slow_rate, fast_rate, 26.0, valid_turn, 60,
                )
                .is_err()
            );
        }
        for turn_slope in [f64::NAN, f64::INFINITY, f64::MAX, -1.0] {
            assert!(
                SmartBeaconingConfig::new(
                    valid_low, valid_high, 1800, 120, turn_slope, valid_turn, 60,
                )
                .is_err()
            );
        }
        assert!(
            SmartBeaconingConfig::new(
                valid_low,
                valid_high,
                1800,
                120,
                26.0,
                Heading::new(121.0)?,
                60,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn turn_time_gates_turn_beacon() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // Default turn_time_secs is 60; less than 60 secs and the turn
        // beacon is suppressed even when the angle threshold is met.
        assert!(sb.should_beacon(speed(120.0), measured_heading(0.0), t0));
        sb.beacon_sent_with(speed(120.0), measured_heading(0.0), t0);

        // 5 seconds after the beacon, a 45-degree turn should NOT fire:
        // the turn_time_secs gate is still closed.
        let t5 = t0 + Duration::from_secs(5);
        assert_eq!(
            sb.beacon_reason(speed(120.0), measured_heading(45.0), t5),
            None,
        );

        // 61 seconds after, the gate is open and the turn beacon fires.
        let t61 = t0 + Duration::from_secs(61);
        assert_eq!(
            sb.beacon_reason(speed(120.0), measured_heading(45.0), t61),
            Some(BeaconReason::Turn),
        );
    }
}
