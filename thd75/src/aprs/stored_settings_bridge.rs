//! Bridge between stored radio settings and runtime APRS types.
//!
//! The TH-D75 stores `SmartBeaconing` speeds and turn slope in the unit
//! selected by Menu No. 970, while the slow rate is stored in whole minutes
//! (see [`crate::types::aprs::StoredSmartBeaconingSettings`]). The runtime
//! [`aprs::SmartBeaconingConfig`] normalizes speed arithmetic to km/h and
//! rates to seconds. This module provides the `From` conversion so callers
//! can do:
//!
//! ```no_run
//! use kenwood_thd75::types::aprs::StoredSmartBeaconingSettings;
//! use aprs::SmartBeaconingConfig;
//!
//! let stored = StoredSmartBeaconingSettings::factory_default(
//!     kenwood_thd75::types::SpeedDistanceUnit::MilesPerHour,
//! );
//! let runtime = SmartBeaconingConfig::try_from(stored)?;
//! # Ok::<(), aprs::AprsError>(())
//! ```

use aprs::{AprsError, Heading, SmartBeaconingConfig, Speed};

use crate::types::{aprs::StoredSmartBeaconingSettings, settings::SpeedDistanceUnit};

const fn configured_speed_to_kmh(unit: SpeedDistanceUnit) -> f64 {
    match unit {
        SpeedDistanceUnit::MilesPerHour => 1.609_344,
        SpeedDistanceUnit::KilometersPerHour => 1.0,
        SpeedDistanceUnit::Knots => 1.852,
    }
}

/// Converts stored radio `SmartBeaconing` settings to the runtime form.
///
/// Field mapping:
///
/// | Stored field | Runtime field       | Conversion                               |
/// |--------------|---------------------|------------------------------------------|
/// | `low_speed`  | `low_speed()`       | Menu 970 unit → km/h                     |
/// | `high_speed` | `high_speed()`      | Menu 970 unit → km/h                     |
/// | `slow_rate`  | `slow_rate_secs()`  | minutes → seconds (× 60)                 |
/// | `fast_rate`  | `fast_rate_secs()`  | seconds (widened `u8` → `u32`)           |
/// | `turn_slope` | `turn_slope()`      | scaled by Menu 970 unit → km/h factor    |
/// | `turn_angle` | `turn_minimum()`    | widened `u8` → validated heading         |
/// | `turn_time`  | `turn_time_secs()`  | widened `u8` → `u32`                     |
impl TryFrom<StoredSmartBeaconingSettings> for SmartBeaconingConfig {
    type Error = AprsError;

    fn try_from(stored: StoredSmartBeaconingSettings) -> Result<Self, Self::Error> {
        let speed_factor = configured_speed_to_kmh(stored.speed_distance_unit);
        Self::new(
            Speed::from_kmh(f64::from(u8::from(stored.low_speed)) * speed_factor)?,
            Speed::from_kmh(f64::from(u8::from(stored.high_speed)) * speed_factor)?,
            u32::from(u8::from(stored.slow_rate)) * 60,
            u32::from(u8::from(stored.fast_rate)),
            // The threshold formula divides by speed. Scaling the speed to
            // km/h therefore requires the same scale on the slope to preserve
            // the radio's angle at every physical speed.
            f64::from(u8::from(stored.turn_slope)) * speed_factor,
            Heading::new(f64::from(u8::from(stored.turn_angle)))?,
            u32::from(u8::from(stored.turn_time)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn factory_stored_settings_convert_to_runtime() -> TestResult {
        let stored = StoredSmartBeaconingSettings::factory_default(SpeedDistanceUnit::MilesPerHour);
        let runtime = SmartBeaconingConfig::try_from(stored)?;
        // Factory stored low_speed = 5 mph → 5 × 1.609344 ≈ 8.046 km/h
        assert!((runtime.low_speed().as_kmh() - 8.046_72).abs() < 1e-4);
        // Factory stored high_speed = 70 mph → 70 × 1.609344 ≈ 112.65 km/h
        assert!((runtime.high_speed().as_kmh() - 112.654_08).abs() < 1e-4);
        assert_eq!(runtime.slow_rate_secs(), 1800);
        assert_eq!(runtime.fast_rate_secs(), 120);
        assert!((runtime.turn_slope() - 41.842_944).abs() < 1e-6);
        assert!((runtime.turn_minimum().as_degrees() - 28.0).abs() < f64::EPSILON);
        assert_eq!(runtime.turn_time_secs(), 60);
        Ok(())
    }

    #[test]
    fn minimum_valid_stored_values_convert_with_explicit_units() -> TestResult {
        let stored = StoredSmartBeaconingSettings {
            speed_distance_unit: SpeedDistanceUnit::KilometersPerHour,
            low_speed: crate::types::aprs::StoredLowSpeed::try_from(2)?,
            high_speed: crate::types::aprs::StoredHighSpeed::try_from(2)?,
            slow_rate: crate::types::aprs::StoredSlowRateMinutes::try_from(1)?,
            fast_rate: crate::types::aprs::StoredFastRateSeconds::try_from(10)?,
            turn_angle: crate::types::aprs::StoredTurnAngleDegrees::try_from(5)?,
            turn_slope: crate::types::aprs::StoredTurnSlope::try_from(1)?,
            turn_time: crate::types::aprs::StoredTurnTimeSeconds::try_from(5)?,
        };
        let runtime = SmartBeaconingConfig::try_from(stored)?;
        assert!((runtime.low_speed().as_kmh() - 2.0).abs() < f64::EPSILON);
        assert!((runtime.high_speed().as_kmh() - 2.0).abs() < f64::EPSILON);
        assert_eq!(runtime.slow_rate_secs(), 60);
        assert_eq!(runtime.fast_rate_secs(), 10);
        assert!((runtime.turn_slope() - 1.0).abs() < f64::EPSILON);
        assert!((runtime.turn_minimum().as_degrees() - 5.0).abs() < f64::EPSILON);
        assert_eq!(runtime.turn_time_secs(), 5);
        Ok(())
    }

    #[test]
    fn knots_scale_speeds_and_slope_together() -> TestResult {
        let stored = StoredSmartBeaconingSettings {
            speed_distance_unit: SpeedDistanceUnit::Knots,
            ..StoredSmartBeaconingSettings::factory_default(SpeedDistanceUnit::Knots)
        };
        let runtime = SmartBeaconingConfig::try_from(stored)?;
        assert!((runtime.low_speed().as_kmh() - 9.26).abs() < 1e-9);
        assert!((runtime.high_speed().as_kmh() - 129.64).abs() < 1e-9);
        assert!((runtime.turn_slope() - 48.152).abs() < 1e-9);
        Ok(())
    }
}
