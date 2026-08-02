//! `SmartBeaconing` algorithm for adaptive APRS beacon timing.
//!
//! Implements the `HamHUD` `SmartBeaconing` algorithm v2.1 originated
//! by Tony Arnerich (KD7TA) and refined by Steve Bragg (KA9MVA), which
//! adjusts beacon interval based on speed and course changes:
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
//! | 530  | Low Speed   | 5 in Menu 970 speed unit  | `low_speed_kmh`   |
//! | 530  | High Speed  | 70 in Menu 970 speed unit | `high_speed_kmh`  |
//! | 531  | Slow Rate   | 30 min                    | `slow_rate_secs`  |
//! | 532  | Fast Rate   | 120 s                     | `fast_rate_secs`  |
//! | 533  | Turn Angle  | 28°                       | `turn_min_deg`    |
//! | 534  | Turn Slope  | 26                        | `turn_slope`      |
//! | 535  | Turn Time   | 60 s                      | `turn_time_secs`  |
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

/// Configuration for the `SmartBeaconing` algorithm.
///
/// Matches the TH-D75 Menu 530-535 settings and the `HamHUD`
/// `SmartBeaconing` v2.1 parameter set after normalization to km/h.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartBeaconingConfig {
    /// Speed threshold below which `slow_rate` is used, in km/h.
    /// TH-D75A V1.03 default: 5 mph = 8.04672 km/h (Menu 530).
    pub low_speed_kmh: f64,
    /// Speed at/above which `fast_rate` is used, in km/h.
    /// TH-D75A V1.03 default: 70 mph = 112.65408 km/h (Menu 530).
    pub high_speed_kmh: f64,
    /// Beacon interval when stopped/slow (seconds). Default: 1800 (30 min).
    /// Corresponds to TH-D75 Menu 531 (Slow Rate).
    pub slow_rate_secs: u32,
    /// Beacon interval at high speed (seconds). V1.03 default: 120.
    /// Corresponds to TH-D75 Menu 532 (Fast Rate).
    pub fast_rate_secs: u32,
    /// Turn-slope coefficient normalized for a km/h denominator in
    /// `turn_min + (turn_slope * 10) / speed_kmh`. TH-D75A V1.03 default:
    /// 26 × 1.609344 = 41.842944. Corresponds to Menu 534.
    pub turn_slope: f64,
    /// Minimum heading change for a turn beacon, in degrees. Applied as
    /// the `turn_min` term in the threshold formula. Default: 28.
    /// Corresponds to TH-D75 Menu 533 (Turn Angle).
    pub turn_min_deg: f64,
    /// Minimum time between turn-triggered beacons (seconds). Default: 60.
    /// Corresponds to TH-D75 Menu 535 (Turn Time).
    pub turn_time_secs: u32,
}

impl Default for SmartBeaconingConfig {
    fn default() -> Self {
        Self {
            low_speed_kmh: 8.046_72,
            high_speed_kmh: 112.654_08,
            slow_rate_secs: 1800,
            fast_rate_secs: 120,
            turn_slope: 41.842_944,
            turn_min_deg: 28.0,
            turn_time_secs: 60,
        }
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
/// beacon's course and speed so subsequent turn-threshold checks have
/// the reference data they need.
#[derive(Debug, Clone, PartialEq)]
pub enum BeaconState {
    /// No beacon has been sent yet; the first call to `should_beacon` /
    /// `beacon_reason` will return `Some(BeaconReason::First)`.
    Uninitialized,
    /// At least one beacon has been sent. Carries the timestamp and
    /// the (course, speed) recorded at that beacon.
    Running {
        /// When the last beacon was transmitted.
        last_beacon_time: Instant,
        /// Course in degrees at the last beacon, normalized to
        /// `[0, 360)`, or `None` if the caller used
        /// [`SmartBeaconing::beacon_sent`] without supplying one or
        /// supplied a non-finite (no-heading-information) value to
        /// [`SmartBeaconing::beacon_sent_with`].
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

    /// Check if a beacon should be sent now, given current speed and course.
    ///
    /// Inputs are sanitized as in [`Self::beacon_reason`]: non-finite
    /// speed is treated as `0.0` (stopped ⇒ slow rate), non-finite
    /// course means "no heading information" (never fires a turn
    /// beacon), and a finite out-of-range course wraps into `[0, 360)`.
    ///
    /// `now` is the current wall-clock time, injected by the caller so
    /// this module remains sans-io.
    #[must_use]
    pub fn should_beacon(&mut self, speed_kmh: f64, course_deg: f64, now: Instant) -> bool {
        self.beacon_reason(speed_kmh, course_deg, now).is_some()
    }

    /// Classify why (if at all) a beacon is due at the current speed and
    /// course. Returns `None` if no beacon should be sent yet, otherwise
    /// a [`BeaconReason`] identifying which condition tripped.
    ///
    /// Mirroring the builder policy for non-finite lat/lon, inputs are
    /// sanitized on entry: a non-finite speed (`NaN`, `±∞`) is treated
    /// as `0.0` (stopped, so the slow rate applies and no zero-second
    /// interval can arise), and a non-finite course means "no heading
    /// information", so no turn beacon can fire. A finite out-of-range
    /// course (e.g. `480.0`) is wrapped into `[0, 360)` before the
    /// heading comparison.
    ///
    /// `now` is the current wall-clock time, injected by the caller so
    /// this module remains sans-io.
    #[must_use]
    pub fn beacon_reason(
        &mut self,
        speed_kmh: f64,
        course_deg: f64,
        now: Instant,
    ) -> Option<BeaconReason> {
        let speed_kmh = sanitize_speed(speed_kmh);
        let course_deg = sanitize_course(course_deg);
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
            && let Some(course_deg) = course_deg
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
    /// turn_threshold = min(120°, turn_min + (turn_slope * 10) / speed_kmh)
    /// ```
    ///
    /// A non-finite speed is treated as `0.0` (stopped), yielding an
    /// infinite threshold: no turn beacon is possible without valid
    /// speed data.
    #[must_use]
    pub fn current_turn_threshold(&self, speed_kmh: f64) -> f64 {
        let speed_kmh = sanitize_speed(speed_kmh);
        if speed_kmh <= self.config.low_speed_kmh {
            return f64::INFINITY;
        }
        (self.config.turn_min_deg + (self.config.turn_slope * 10.0) / speed_kmh).min(120.0)
    }

    /// Mark that a beacon was just sent. Updates the internal state
    /// with the supplied time, preserving any previously-recorded course
    /// and speed.
    ///
    /// `now` is the wall-clock time at which the beacon was sent.
    pub const fn beacon_sent(&mut self, now: Instant) {
        let (prev_course, prev_speed) = match self.state {
            BeaconState::Uninitialized => (None, None),
            BeaconState::Running {
                last_course,
                last_speed,
                ..
            } => (last_course, last_speed),
        };
        self.state = BeaconState::Running {
            last_beacon_time: now,
            last_course: prev_course,
            last_speed: prev_speed,
        };
    }

    /// Mark that a beacon was just sent with the given speed and course.
    ///
    /// The recorded values are sanitized like the [`Self::beacon_reason`]
    /// inputs: a non-finite speed is stored as `0.0`, a non-finite
    /// course is stored as `None` (no heading reference for later turn
    /// detection), and a finite out-of-range course wraps into
    /// `[0, 360)`.
    ///
    /// `now` is the wall-clock time at which the beacon was sent.
    pub const fn beacon_sent_with(&mut self, speed_kmh: f64, course_deg: f64, now: Instant) {
        self.state = BeaconState::Running {
            last_beacon_time: now,
            last_course: sanitize_course(course_deg),
            last_speed: Some(sanitize_speed(speed_kmh)),
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
    /// Callers must pass a finite speed; every public entry point runs
    /// [`sanitize_speed`] first, so a non-finite value can never reach the
    /// division below.
    fn compute_interval(&self, speed_kmh: f64) -> u32 {
        if speed_kmh <= 0.0 || speed_kmh < self.config.low_speed_kmh {
            return self.config.slow_rate_secs;
        }
        if speed_kmh >= self.config.high_speed_kmh {
            return self.config.fast_rate_secs;
        }

        // With reversed thresholds there is no documented intermediate
        // domain in which the division formula applies.
        if self.config.high_speed_kmh < self.config.low_speed_kmh {
            return self.config.slow_rate_secs;
        }

        let interval =
            f64::from(self.config.fast_rate_secs) * self.config.high_speed_kmh / speed_kmh;

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "speed is finite and positive here; f64-to-u32 casts saturate, and the \
                      radio stores whole-second intervals"
        )]
        let interval = interval.round() as u32;
        interval
    }
}

/// Substitute `0.0` for a non-finite speed (`NaN`, `±∞`).
///
/// Mirrors the builder policy of substituting `0.0` for non-finite
/// lat/lon: corrupt GPS speed data means "treat as stopped", which
/// selects the slow beacon rate and disables turn detection, instead
/// of calculating a `NaN` interval that saturates to zero seconds
/// and beacons continuously.
const fn sanitize_speed(speed_kmh: f64) -> f64 {
    if speed_kmh.is_finite() {
        speed_kmh
    } else {
        0.0
    }
}

/// Normalize a course to `[0, 360)` degrees.
///
/// Returns `None` for non-finite input (`NaN`, `±∞`), which means no
/// heading information, so turn detection must not use it. A finite
/// out-of-range value (e.g. `480.0`) wraps into range so
/// [`heading_delta`] sees the true heading instead of computing a
/// bogus (possibly negative) delta.
const fn sanitize_course(course_deg: f64) -> Option<f64> {
    if !course_deg.is_finite() {
        return None;
    }
    let wrapped = course_deg % 360.0;
    if wrapped < 0.0 {
        Some(wrapped + 360.0)
    } else {
        Some(wrapped)
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn default_config_values() {
        let cfg = SmartBeaconingConfig::default();
        assert!((cfg.low_speed_kmh - 8.046_72).abs() < f64::EPSILON);
        assert!((cfg.high_speed_kmh - 112.654_08).abs() < f64::EPSILON);
        assert_eq!(cfg.slow_rate_secs, 1800);
        assert_eq!(cfg.fast_rate_secs, 120);
        assert!((cfg.turn_slope - 41.842_944).abs() < f64::EPSILON);
        assert!((cfg.turn_min_deg - 28.0).abs() < f64::EPSILON);
        assert_eq!(cfg.turn_time_secs, 60);
    }

    #[test]
    fn turn_threshold_matches_hamhud_formula() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig {
            low_speed_kmh: 5.0,
            high_speed_kmh: 70.0,
            turn_slope: 24.0,
            turn_min_deg: 30.0,
            ..SmartBeaconingConfig::default()
        });
        // Stopped / slow: threshold is infinity, no turn beacon possible.
        assert!(sb.current_turn_threshold(0.0).is_infinite());
        assert!(sb.current_turn_threshold(5.0).is_infinite());

        // V1.03 manual example: 30 + (24 * 10) / 60 = 34 degrees.
        let t60 = sb.current_turn_threshold(60.0);
        assert!((t60 - 34.0).abs() < 1e-9);

        // At 10 km/h: 30 + 24 = 54 degrees.
        let t10 = sb.current_turn_threshold(10.0);
        assert!((t10 - 54.0).abs() < 1e-9);
    }

    #[test]
    fn turn_threshold_is_capped_at_120_degrees() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig {
            low_speed_kmh: 2.0,
            high_speed_kmh: 90.0,
            turn_slope: 255.0,
            turn_min_deg: 90.0,
            ..SmartBeaconingConfig::default()
        });
        assert!((sb.current_turn_threshold(3.0) - 120.0).abs() < f64::EPSILON);
    }

    #[test]
    fn first_beacon_always_true() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.should_beacon(0.0, 0.0, t0));
    }

    #[test]
    fn interval_at_low_speed() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.compute_interval(0.0), 1800);
        assert_eq!(sb.compute_interval(8.0), 1800);
        // At exactly Low Speed, V1.03 applies the inverse-speed formula.
        assert_eq!(sb.compute_interval(8.046_72), 1680);
    }

    #[test]
    fn interval_at_high_speed() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert_eq!(sb.compute_interval(112.654_08), 120);
        assert_eq!(sb.compute_interval(150.0), 120);
    }

    #[test]
    fn interval_matches_v103_inverse_speed_examples() {
        let sb = SmartBeaconing::new(SmartBeaconingConfig {
            low_speed_kmh: 5.0,
            high_speed_kmh: 70.0,
            slow_rate_secs: 1800,
            fast_rate_secs: 120,
            ..SmartBeaconingConfig::default()
        });
        assert_eq!(sb.compute_interval(50.0), 168);
        assert_eq!(sb.compute_interval(30.0), 280);
        assert_eq!(sb.compute_interval(20.0), 420);
        assert_eq!(sb.compute_interval(10.0), 840);
        assert_eq!(sb.compute_interval(5.0), 1680);
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
        sb.beacon_sent_with(120.0, 0.0, t0);
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
    fn beacon_sent_with_stores_course_and_speed() -> TestResult {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        sb.beacon_sent_with(50.0, 270.0, t0);
        let BeaconState::Running {
            last_course,
            last_speed,
            ..
        } = sb.state()
        else {
            return Err("expected Running state".into());
        };
        let speed = last_speed.ok_or("expected last_speed to be Some")?;
        let course = last_course.ok_or("expected last_course to be Some")?;
        assert!((speed - 50.0).abs() < f64::EPSILON);
        assert!((course - 270.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn no_beacon_immediately_after_send() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // First beacon is always true.
        assert!(sb.should_beacon(0.0, 0.0, t0));
        sb.beacon_sent_with(0.0, 0.0, t0);

        // Immediately after, should not beacon (interval not elapsed).
        assert!(!sb.should_beacon(0.0, 0.0, t0));
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
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig {
            turn_time_secs: 0, // No minimum turn time for test simplicity.
            ..SmartBeaconingConfig::default()
        });

        // Send initial beacon heading north.
        assert!(sb.should_beacon(3.0, 0.0, t0));
        sb.beacon_sent_with(3.0, 0.0, t0);

        // Large heading change but at low speed: should NOT trigger.
        assert!(!sb.should_beacon(3.0, 90.0, t0));
    }

    #[test]
    fn turn_beacon_triggered_at_high_speed() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig {
            turn_time_secs: 0, // No minimum turn time for test simplicity.
            ..SmartBeaconingConfig::default()
        });

        // Send initial beacon heading north above High Speed.
        assert!(sb.should_beacon(120.0, 0.0, t0));
        sb.beacon_sent_with(120.0, 0.0, t0);

        // Course change above turn_threshold (28 deg) at high speed
        // should trigger an immediate beacon.
        assert!(sb.should_beacon(120.0, 45.0, t0));
    }

    #[test]
    fn turn_beacon_below_threshold_no_trigger() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig {
            turn_time_secs: 0,
            ..SmartBeaconingConfig::default()
        });

        // Send initial beacon heading north above High Speed.
        assert!(sb.should_beacon(120.0, 0.0, t0));
        sb.beacon_sent_with(120.0, 0.0, t0);

        // Course change below turn_threshold (28 deg) should NOT trigger.
        assert!(!sb.should_beacon(120.0, 20.0, t0));
    }

    #[test]
    fn time_expired_triggers_beacon_after_interval() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.should_beacon(0.0, 0.0, t0));
        sb.beacon_sent_with(0.0, 0.0, t0);

        // At low speed, slow_rate is 1800 secs. Advance past interval.
        let later = t0 + Duration::from_secs(1801);
        assert_eq!(
            sb.beacon_reason(0.0, 0.0, later),
            Some(BeaconReason::TimeExpired),
        );
    }

    #[test]
    fn nan_speed_does_not_zero_the_interval() {
        // A non-finite speed must be treated as 0.0 (stopped ⇒ slow
        // rate), not used in a calculation that yields NaN and saturates to a
        // zero-second interval and fires TimeExpired on every call
        // (a continuous-TX beacon storm on air).
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.should_beacon(50.0, 0.0, t0));
        sb.beacon_sent_with(50.0, 0.0, t0);

        let t1 = t0 + Duration::from_secs(1);
        let reason = sb.beacon_reason(f64::NAN, 0.0, t1);
        assert_eq!(
            reason, None,
            "NaN speed one second after a beacon must not trigger anything",
        );
        assert!(!sb.should_beacon(f64::NAN, 0.0, t1));
        assert!(!sb.should_beacon(f64::INFINITY, 0.0, t1));
        assert!(!sb.should_beacon(f64::NEG_INFINITY, 0.0, t1));
    }

    #[test]
    fn out_of_range_course_behaves_like_normalized_course() {
        // 480° is the same heading as 120°; turn detection must see
        // them identically instead of computing a negative delta.
        let t0 = Instant::now();
        let cfg = SmartBeaconingConfig {
            turn_time_secs: 0,
            ..SmartBeaconingConfig::default()
        };
        let mut sb_wrapped = SmartBeaconing::new(cfg.clone());
        let mut sb_plain = SmartBeaconing::new(cfg);
        sb_wrapped.beacon_sent_with(120.0, 0.0, t0);
        sb_plain.beacon_sent_with(120.0, 0.0, t0);

        let t1 = t0 + Duration::from_secs(1);
        let wrapped = sb_wrapped.beacon_reason(120.0, 480.0, t1);
        let plain = sb_plain.beacon_reason(120.0, 120.0, t1);
        assert_eq!(wrapped, plain, "course 480° must behave like 120°");
        assert_eq!(plain, Some(BeaconReason::Turn));
    }

    #[test]
    fn nan_course_fires_no_turn_beacon() {
        // Non-finite course means "no heading information", so it must
        // never fire a turn beacon, whether it arrives as the current
        // course or was recorded at the previous beacon.
        let t0 = Instant::now();
        let cfg = SmartBeaconingConfig {
            turn_time_secs: 0,
            ..SmartBeaconingConfig::default()
        };
        let t1 = t0 + Duration::from_secs(1);

        let mut sb = SmartBeaconing::new(cfg.clone());
        sb.beacon_sent_with(120.0, 0.0, t0);
        assert_eq!(
            sb.beacon_reason(120.0, f64::NAN, t1),
            None,
            "NaN current course must not fire a turn beacon",
        );

        let mut sb_stored = SmartBeaconing::new(cfg);
        sb_stored.beacon_sent_with(120.0, f64::NAN, t0);
        assert_eq!(
            sb_stored.beacon_reason(120.0, 120.0, t1),
            None,
            "NaN stored course must not act as a heading reference",
        );
    }

    #[test]
    fn turn_threshold_non_finite_speed_is_infinite() {
        // Non-finite speed sanitizes to 0.0 (stopped), so no turn
        // beacon is possible: the threshold must be infinite, not NaN
        // (NaN) or a finite value (+∞ input).
        let sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        assert!(sb.current_turn_threshold(f64::NAN).is_infinite());
        assert!(sb.current_turn_threshold(f64::INFINITY).is_infinite());
        assert!(sb.current_turn_threshold(f64::NEG_INFINITY).is_infinite());
    }

    #[test]
    fn turn_time_gates_turn_beacon() {
        let t0 = Instant::now();
        let mut sb = SmartBeaconing::new(SmartBeaconingConfig::default());
        // Default turn_time_secs is 60; less than 60 secs and the turn
        // beacon is suppressed even when the angle threshold is met.
        assert!(sb.should_beacon(120.0, 0.0, t0));
        sb.beacon_sent_with(120.0, 0.0, t0);

        // 5 seconds after the beacon, a 45-degree turn should NOT fire:
        // the turn_time_secs gate is still closed.
        let t5 = t0 + Duration::from_secs(5);
        assert_eq!(sb.beacon_reason(120.0, 45.0, t5), None);

        // 61 seconds after, the gate is open and the turn beacon fires.
        let t61 = t0 + Duration::from_secs(61);
        assert_eq!(sb.beacon_reason(120.0, 45.0, t61), Some(BeaconReason::Turn),);
    }
}
