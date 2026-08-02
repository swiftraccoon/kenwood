//! Bridge between MCP (radio memory) and runtime APRS types.
//!
//! The TH-D75 stores `SmartBeaconing` speeds and turn slope in the unit
//! selected by Menu No. 970, while the slow rate is stored in whole minutes
//! (see [`crate::types::aprs::McpSmartBeaconingConfig`]). The runtime
//! [`aprs::SmartBeaconingConfig`] normalizes speed arithmetic to km/h and
//! rates to seconds. This module provides the `From` conversion so callers
//! can do:
//!
//! ```no_run
//! use kenwood_thd75::types::aprs::McpSmartBeaconingConfig;
//! use aprs::SmartBeaconingConfig;
//!
//! let mcp = McpSmartBeaconingConfig::default();
//! let runtime: SmartBeaconingConfig = mcp.into();
//! ```

use aprs::SmartBeaconingConfig;

use crate::types::{aprs::McpSmartBeaconingConfig, settings::SpeedDistanceUnit};

const fn configured_speed_to_kmh(unit: SpeedDistanceUnit) -> f64 {
    match unit {
        SpeedDistanceUnit::MilesPerHour => 1.609_344,
        SpeedDistanceUnit::KilometersPerHour => 1.0,
        SpeedDistanceUnit::Knots => 1.852,
    }
}

/// Converts a radio-memory `SmartBeaconing` config to the runtime form.
///
/// Field mapping:
///
/// | MCP field    | Runtime field       | Conversion                               |
/// |--------------|---------------------|------------------------------------------|
/// | `low_speed`  | `low_speed_kmh`     | Menu 970 unit → km/h                     |
/// | `high_speed` | `high_speed_kmh`    | Menu 970 unit → km/h                     |
/// | `slow_rate`  | `slow_rate_secs`    | minutes → seconds (× 60)                 |
/// | `fast_rate`  | `fast_rate_secs`    | seconds (widened `u8` → `u32`)           |
/// | `turn_slope` | `turn_slope`        | scaled by Menu 970 unit → km/h factor    |
/// | `turn_angle` | `turn_min_deg`      | widened `u8` → `f64`                     |
/// | `turn_time`  | `turn_time_secs`    | widened `u8` → `u32`                     |
impl From<McpSmartBeaconingConfig> for SmartBeaconingConfig {
    fn from(mcp: McpSmartBeaconingConfig) -> Self {
        let speed_factor = configured_speed_to_kmh(mcp.speed_distance_unit);
        Self {
            low_speed_kmh: f64::from(u8::from(mcp.low_speed)) * speed_factor,
            high_speed_kmh: f64::from(u8::from(mcp.high_speed)) * speed_factor,
            slow_rate_secs: u32::from(u8::from(mcp.slow_rate)) * 60,
            fast_rate_secs: u32::from(u8::from(mcp.fast_rate)),
            // The threshold formula divides by speed. Scaling the speed to
            // km/h therefore requires the same scale on the slope to preserve
            // the radio's angle at every physical speed.
            turn_slope: f64::from(u8::from(mcp.turn_slope)) * speed_factor,
            turn_min_deg: f64::from(u8::from(mcp.turn_angle)),
            turn_time_secs: u32::from(u8::from(mcp.turn_time)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mcp_converts_to_runtime() {
        let mcp = McpSmartBeaconingConfig::default();
        let runtime: SmartBeaconingConfig = mcp.into();
        // Default MCP: low_speed = 5 mph → 5 × 1.609344 ≈ 8.046 km/h
        assert!((runtime.low_speed_kmh - 8.046_72).abs() < 1e-4);
        // Default MCP: high_speed = 70 mph → 70 × 1.609344 ≈ 112.65 km/h
        assert!((runtime.high_speed_kmh - 112.654_08).abs() < 1e-4);
        assert_eq!(runtime.slow_rate_secs, 1800);
        assert_eq!(runtime.fast_rate_secs, 120);
        assert!((runtime.turn_slope - 41.842_944).abs() < 1e-6);
        assert!((runtime.turn_min_deg - 28.0).abs() < f64::EPSILON);
        assert_eq!(runtime.turn_time_secs, 60);
    }

    #[test]
    fn minimum_valid_mcp_values_convert_with_explicit_units()
    -> Result<(), crate::error::ValidationError> {
        let mcp = McpSmartBeaconingConfig {
            speed_distance_unit: SpeedDistanceUnit::KilometersPerHour,
            low_speed: crate::types::aprs::McpLowSpeed::try_from(2)?,
            high_speed: crate::types::aprs::McpHighSpeed::try_from(2)?,
            slow_rate: crate::types::aprs::McpSlowRateMinutes::try_from(1)?,
            fast_rate: crate::types::aprs::McpFastRateSeconds::try_from(10)?,
            turn_angle: crate::types::aprs::McpTurnAngleDegrees::try_from(5)?,
            turn_slope: crate::types::aprs::McpTurnSlope::try_from(1)?,
            turn_time: crate::types::aprs::McpTurnTimeSeconds::try_from(5)?,
        };
        let runtime: SmartBeaconingConfig = mcp.into();
        assert!((runtime.low_speed_kmh - 2.0).abs() < f64::EPSILON);
        assert!((runtime.high_speed_kmh - 2.0).abs() < f64::EPSILON);
        assert_eq!(runtime.slow_rate_secs, 60);
        assert_eq!(runtime.fast_rate_secs, 10);
        assert!((runtime.turn_slope - 1.0).abs() < f64::EPSILON);
        assert!((runtime.turn_min_deg - 5.0).abs() < f64::EPSILON);
        assert_eq!(runtime.turn_time_secs, 5);
        Ok(())
    }

    #[test]
    fn knots_scale_speeds_and_slope_together() {
        let mcp = McpSmartBeaconingConfig {
            speed_distance_unit: SpeedDistanceUnit::Knots,
            ..McpSmartBeaconingConfig::default()
        };
        let runtime: SmartBeaconingConfig = mcp.into();
        assert!((runtime.low_speed_kmh - 9.26).abs() < 1e-9);
        assert!((runtime.high_speed_kmh - 129.64).abs() < 1e-9);
        assert!((runtime.turn_slope - 48.152).abs() < 1e-9);
    }
}
