#![doc = include_str!("../README.md")]

//! Swift-facing control core for Azimuth.
//!
//! The crate deliberately contains no platform USB implementation. Swift owns
//! the iPadOS or macOS USB connection and implements [`ByteTransport`]; Rust
//! owns all TH-D75 framing, exact automation qualification, guarded input, screen
//! authentication, and MCP setting validation.

#[forbid(unsafe_code)]
mod aprs;
#[forbid(unsafe_code)]
mod automation;
#[forbid(unsafe_code)]
mod catalog;
#[forbid(unsafe_code)]
mod if_dsp;
#[forbid(unsafe_code)]
mod if_dsp_radio;
#[forbid(unsafe_code)]
mod transport;

pub use aprs::{
    AprsActivityDirection, AprsActivityKind, AprsActivityRecord, AprsOperationalSnapshot,
    AprsSessionConfig, AprsSessionPhase, AprsSessionStatus, AprsStationRecord, AprsTncBaud,
};
pub use automation::{
    AutomationAbiRecord, AutomationController, AutomationError, FrontPanelKey,
    GuardedTapDisposition, GuardedTapResult, IfDspRadioPhase, IfDspRadioStatus, RemoteScreenFrame,
    SettingApplyReport, SettingChangeOutcome, SettingChangeResult, SettingReadResult,
    SettingValueRecord, connect_automation,
};
pub use catalog::{
    SettingChange, SettingChangeValidation, SettingConversionError, SettingMenu, SettingOption,
    SettingPlanValidation, SettingPresentation, SettingRecord, SettingStorageTransform,
    SettingTextEncoding, SettingValue, SettingValueKind, decode_setting_display_value,
    encode_setting_display_value, setting_catalog, validate_setting_changes,
};
pub use if_dsp::{
    IfDspConfiguration, IfDspError, IfDspFrame, IfDspMode, IfDspProcessor, IfDspSpectrum,
};
pub use transport::{ByteTransport, ByteTransportError};

uniffi::setup_scaffolding!();

/// Return this core's semantic version.
#[must_use]
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_matches_package() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}
