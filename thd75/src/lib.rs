#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]
//! Async Rust library for controlling the Kenwood TH-D75 transceiver via
//! CAT (Computer Aided Transceiver) -- the serial command protocol Kenwood
//! uses for remote radio control.
//!
//! This library exposes typed, individually qualified CAT operations over USB
//! serial or Bluetooth SPP. Unresolved, service-only, or lossy write paths are
//! excluded or fail before I/O. Command definitions and validation rules are
//! aligned with the stock Kenwood V1.03 schema and live hardware validation;
//! closed-loop screen/input automation additionally requires exact
//! `1.03.AZM` with ABI 3.
//!
//! # TH-D75 overview (per User Manual Chapter 28)
//!
//! - **Models**: TH-D75A (144/220/430 MHz tribander, Americas) and
//!   TH-D75E (144/430 MHz dual bander, Europe/UK).
//! - **TX power**: 5 W / 2 W / 0.5 W / 0.05 W (4 steps).
//! - **Modulation**: FM, NFM, DV (D-STAR GMSK), AM, LSB, USB, CW, WFM.
//! - **Frequency stability**: +/-2.0 ppm.
//! - **Operating temperature**: -20 to +60 C (-10 to +50 C with KNB-75LA).
//! - **Receiver**: Band A double superheterodyne (1st IF 57.15 MHz,
//!   2nd IF 450 kHz); Band B double/triple superheterodyne (1st IF
//!   58.05 MHz, 2nd IF 450 kHz, 3rd IF 10.8 kHz for SSB/CW/AM).
//! - **Audio output**: 400 mW or more at 8 ohm (7.4 V, 10% distortion).
//! - **Memory**: 1000 channels, 1500 repeater lists, 30 hotspot lists.
//! - **Weatherproof**: IP54/55.
//! - **Bluetooth**: 3.0, Class 2, HSP + SPP profiles.
//! - **GPS**: built-in receiver, TTFF cold ~40s / hot ~5s, 10 m accuracy.
//! - **microSD**: 2-32 GB (FAT32), for config, recordings, GPS logs.
//!
//! # Usage
//!
//! ```rust,no_run
//! use kenwood_thd75::transport::SerialTransport;
//! use kenwood_thd75::radio::Radio;
//! use kenwood_thd75::types::Band;
//!
//! # async fn example() -> Result<(), kenwood_thd75::error::Error> {
//! // Connect over USB serial.
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
//! let mut radio = Radio::new(transport);
//!
//! // Verify the radio identity.
//! let info = radio.identify().await?;
//! println!("Connected to: {}", info.model);
//!
//! // Read the current frequency on Band A.
//! let frequency = radio.get_frequency(Band::A).await?;
//! println!("RX frequency: {} Hz", frequency.as_hz());
//!
//! // Disconnect cleanly.
//! radio.disconnect().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Modules
//!
//! - [`types`]: validated newtypes for frequencies, tones, modes, and channels.
//! - [`protocol`]: pure-logic CAT command codec (serialize / parse).
//! - [`transport`]: async I/O trait and serial / mock implementations.
//! - [`radio`]: high-level async API wrapping the protocol and transport layers.
//! - [`screen`]: exact RGB565 LCD frames, stock-compatible BMP rendering, CRC-32,
//!   and macOS Vision OCR with strict unique-text assertions.
//! - [`memory`]: typed accessors over a TH-D75 memory image (from MCP or `.d75` files).
//! - [`sdcard`]: parsers for TH-D75 SD card files (`.d75` config, `.tsv` lists, `.nme` logs, and more).
//! - [`aprs`]: TH-D75-specific APRS glue, namely [`AprsClient`] owning a [`Radio`]
//!   and [`KissSession`], the stored-settings bridge, and digipeater-path helpers.
//!   Generic KISS/AX.25/APRS decoding lives in the `kiss-tnc`, `ax25-codec`,
//!   and `aprs` sibling crates. Requires the `aprs` cargo feature (default).
//! - [`dstar_gateway`]: D-STAR gateway client ([`DstarGateway`]) for Reflector Terminal
//!   Mode, built on the `mmdvm-core` framing codec and `dstar-gateway-core`
//!   protocol crates. Requires the `dstar` cargo feature (default).
//! - [`session`]: explicit link recovery, covering the reconnect backoff policy
//!   and the opt-in wrapper that drives [`Radio::reconnect`](radio::Radio::reconnect).
//! - [`error`]: error types for transport, protocol, and validation failures.
//!
//! # Cargo features
//!
//! Both features are on by default; a CAT-only consumer can use
//! `default-features = false` for the core control surface alone.
//!
//! - **`aprs`** enables the APRS client stack: the [`aprs`] module ([`AprsClient`]
//!   owning the radio, KISS session, and APRS-IS uplink glue), the
//!   [`KissSession`] binary TNC session, and the `aprs-is`/`kiss-tnc`
//!   re-exports. CAT-level APRS settings, GPS position types, and TNC mode
//!   commands are always available; the sans-io `aprs` and `ax25-codec`
//!   crates back those core types and are unconditional dependencies.
//! - **`dstar`** enables the D-STAR reflector-gateway stack: the [`dstar_gateway`]
//!   module ([`DstarGateway`]) and the [`MmdvmSession`] modem session over
//!   the tokio `mmdvm` crate. The Menu 650 terminal-mode lifecycle, MMDVM
//!   link diagnosis, and CAT D-STAR settings are always available (they
//!   need only the sans-io `mmdvm-core`).

#[cfg(feature = "aprs")]
pub mod aprs;
#[cfg(feature = "dstar")]
pub mod dstar_gateway;
pub mod error;
pub mod memory;
pub mod protocol;
pub mod radio;
pub mod screen;
pub mod sdcard;
pub mod session;
pub mod transport;
pub mod types;
pub mod verify;

// Dev-dependencies used only by the integration tests in `tests/`.
// Acknowledge them at the lib level so `unused_crate_dependencies`
// stays silent for the lib-test compilation unit, which sees every
// dev-dep but exercises none of these directly.
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use serde_json as _;
// The client-stack crates are unconditional dev-dependencies (for tests and
// examples); with the corresponding lib feature off, the lib-test unit sees
// them without using them, so acknowledge them for that configuration only.
#[cfg(all(test, not(feature = "aprs")))]
use aprs_is as _;
#[cfg(all(test, not(feature = "aprs")))]
use kiss_tnc as _;
#[cfg(all(test, not(feature = "dstar")))]
use mmdvm as _;

// Convenience re-exports for the most commonly used types.
pub use error::Error;
pub use radio::diagnostics::LinkDiagnosis;
pub use radio::if_tap::{
    IfTapConfig, IfTapEnterError, IfTapRestoreReport, IfTapRestoreStep, IfTapSavedState,
    IfTapSession,
};
pub use radio::programming::{McpPage, McpSession, WritableMcpPage};
pub use radio::state_monitor::{BandState, StateChange, StateMonitor};
pub use radio::terminal_mode::{TerminalModeTransition, TerminalModeTransitionError};
pub use radio::{DesyncedRadio, FirmwareProfile, Radio};
#[cfg(target_os = "macos")]
pub use transport::{BluetoothOpenCancellation, BluetoothTransport, PairedBluetoothCandidate};
pub use transport::{EitherTransport, MockTransport, SerialTransport, Transport};
pub use types::{
    ChannelDisplayName, FirmwareIdentity, HardwareVariant, ModelCode, RadioModel, RadioRegion,
    RadioType, RegularChannel, SerialInformation, SerialNumber,
};

// Memory image re-exports.
pub use memory::{MemoryError, MemoryImage};

// Generic crate re-exports at crate root for consumer convenience.
//
// These let existing downstream code keep using `kenwood_thd75::AprsClient`,
// `kenwood_thd75::KissFrame`, etc. without importing the generic crates
// directly. The items themselves live in `kiss-tnc`, `ax25-codec`, `aprs`,
// and `aprs-is`; inside this crate, use those crate paths directly rather
// than routing through these re-exports.
pub use ::aprs::{
    AprsData, AprsDataExtension, AprsError, AprsItem, AprsMessage, AprsMessenger, AprsObject,
    AprsPosition, AprsPositionlessWeatherReport, AprsQuery, AprsReportTimestamp,
    AprsReportTimestampFormat, AprsStatus, AprsStatusTimestamp, AprsSymbol, AprsTelemetry,
    AprsTextError, AprsTextField, AprsWeather, AprsWeatherTimestamp, BarometricPressure,
    CompressedPositionText, Course, DigiAction, DigipeaterConfig, Fahrenheit, Heading, Humidity,
    ItemName, Latitude, Longitude, Luminosity, MessageAddressee, MessageId, MessageText, MiceSpeed,
    MiceStatusText, ObjectName, Phg, PhgDirectivity, PositionReportText, SmartBeaconing,
    SmartBeaconingConfig, Speed, StationEntry, StationList, StatusText, SymbolTable,
    ThreeDigitWeatherValue, TimestampedStatusText, WeatherComment, WeatherValueError,
    WindDirection, build_aprs_item, build_aprs_message, build_aprs_mice, build_aprs_object,
    build_aprs_position_compressed, build_aprs_position_report, build_aprs_status,
    build_aprs_timestamped_status, build_aprs_weather, build_query_response_position,
    parse_aprs_extensions,
};
#[cfg(feature = "aprs")]
pub use aprs_is::{
    AprsIsClient, AprsIsConfig, AprsIsError, AprsIsEvent, AprsIsUplinkLine, AprsIsUplinkLineError,
    IGateFormatError, Passcode, aprs_is_passcode, build_login_string, format_is_packet,
    parse_is_line,
};
pub use ax25_codec::{Ax25Address, Ax25Error, Ax25Packet, DigipeaterPath};
#[cfg(feature = "aprs")]
pub use kiss_tnc::{KissError, KissFrame};

// D75-specific re-exports.
#[cfg(feature = "aprs")]
pub use aprs::client::{AprsClient, AprsClientConfig, AprsEvent, IGateRfLocality, IGateToRfConfig};

// KISS session re-export.
#[cfg(feature = "aprs")]
pub use radio::kiss_session::KissSession;

// MMDVM session re-export.
#[cfg(feature = "dstar")]
pub use radio::mmdvm_session::MmdvmSession;

// Link-recovery policy.
pub use session::ReconnectPolicy;

// D-STAR gateway re-exports. Raw codec types live in mmdvm-core; the
// async event loop lives in mmdvm. The types re-exported here
// compose those crates into the D-STAR-specific surface
// TH-D75 consumers use.
#[cfg(feature = "dstar")]
pub use dstar_gateway::{
    DstarEvent, DstarGateway, DstarGatewayConfig, DstarHeader, DstarProtocolViolation,
    DstarStatusReflector, DstarStatusReflectorError, LastHeardEntry, MmdvmError, ModemMode,
    ModemStatus, NakReason, ObservedDstarCallsign, PersistentMmdvm, SlowDataTextMessage,
    TransientMmdvm, WireTextError,
};

// SD card re-exports.
pub use sdcard::SdCardError;
pub use sdcard::config::{ConfigFileModel, ConfigHeader, RadioConfig, parse_config, write_config};
