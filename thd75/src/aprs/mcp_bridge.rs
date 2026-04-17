//! Bridge between MCP (radio memory) and runtime APRS types.
//!
//! The TH-D75 stores `SmartBeaconing` parameters in its MCP memory using
//! mph-based units (see [`crate::types::aprs::McpSmartBeaconingConfig`]).
//! The runtime [`aprs::SmartBeaconingConfig`] uses km/h instead so the
//! algorithm arithmetic matches the `HamHUD` reference. This module
//! provides the `From` conversion so callers can do:
//!
//! ```no_run
//! use kenwood_thd75::types::aprs::McpSmartBeaconingConfig;
//! use aprs::SmartBeaconingConfig;
//!
//! let mcp = McpSmartBeaconingConfig::default();
//! let runtime: SmartBeaconingConfig = mcp.into();
//! ```

use aprs::SmartBeaconingConfig;

use crate::types::aprs::McpSmartBeaconingConfig;

/// Converts a radio-memory `SmartBeaconing` config (mph/seconds) to the
/// runtime form (km/h / seconds / `f64`).
///
/// Field mapping (all rates are in seconds on both sides):
///
/// | MCP field    | Runtime field       | Conversion                          |
/// |--------------|---------------------|-------------------------------------|
/// | `low_speed`  | `low_speed_kmh`     | mph → km/h (× `1.609_344`)          |
/// | `high_speed` | `high_speed_kmh`    | mph → km/h (× `1.609_344`)          |
/// | `slow_rate`  | `slow_rate_secs`    | seconds (widened `u16` → `u32`)     |
/// | `fast_rate`  | `fast_rate_secs`    | seconds (widened `u8` → `u32`)      |
/// | `turn_slope` | `turn_slope`        | widened `u8` → `u16`                |
/// | `turn_angle` | `turn_min_deg`      | widened `u8` → `f64`                |
/// | `turn_time`  | `turn_time_secs`    | widened `u8` → `u32`                |
impl From<McpSmartBeaconingConfig> for SmartBeaconingConfig {
    fn from(mcp: McpSmartBeaconingConfig) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mcp_converts_to_runtime() {
        let mcp = McpSmartBeaconingConfig::default();
        let runtime: SmartBeaconingConfig = mcp.into();
        // Default MCP: low_speed = 5 mph → 5 × 1.609344 ≈ 8.046 km/h
        assert!((runtime.low_speed_kmh - 8.046_72).abs() < 1e-4);
        // Default MCP: high_speed = 60 mph → 60 × 1.609344 ≈ 96.56 km/h
        assert!((runtime.high_speed_kmh - 96.560_64).abs() < 1e-4);
        assert_eq!(runtime.slow_rate_secs, 1800);
        assert_eq!(runtime.fast_rate_secs, 60);
        assert_eq!(runtime.turn_slope, 26);
        assert!((runtime.turn_min_deg - 28.0).abs() < f64::EPSILON);
        assert_eq!(runtime.turn_time_secs, 30);
    }

    #[test]
    fn zero_mph_converts_to_zero_kmh() {
        let mcp = McpSmartBeaconingConfig {
            low_speed: 0,
            high_speed: 0,
            slow_rate: 0,
            fast_rate: 0,
            turn_angle: 0,
            turn_slope: 0,
            turn_time: 0,
        };
        let runtime: SmartBeaconingConfig = mcp.into();
        assert!(runtime.low_speed_kmh.abs() < f64::EPSILON);
        assert!(runtime.high_speed_kmh.abs() < f64::EPSILON);
        assert_eq!(runtime.slow_rate_secs, 0);
        assert_eq!(runtime.fast_rate_secs, 0);
        assert_eq!(runtime.turn_slope, 0);
        assert!(runtime.turn_min_deg.abs() < f64::EPSILON);
        assert_eq!(runtime.turn_time_secs, 0);
    }
}
