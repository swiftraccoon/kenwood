#![deny(unsafe_code)]
//! Async Rust library for controlling the Kenwood TH-D75 transceiver via
//! CAT (Computer Aided Transceiver) -- the serial command protocol Kenwood
//! uses for remote radio control.
//!
//! This library supports all 55 CAT commands over USB serial or Bluetooth
//! SPP connections. Command definitions and validation rules are based on
//! analysis of TH-D75 firmware v1.03.000.
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
//! let transport = SerialTransport::open("/dev/cu.usbmodem1234", 115_200)?;
//! let mut radio = Radio::connect(transport).await?;
//!
//! // Verify the radio identity.
//! let info = radio.identify().await?;
//! println!("Connected to: {}", info.model);
//!
//! // Read the current frequency on Band A.
//! let channel = radio.get_frequency(Band::A).await?;
//! println!("RX frequency: {} Hz", channel.rx_frequency.as_hz());
//!
//! // Disconnect cleanly.
//! radio.disconnect().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Modules
//!
//! - [`types`] — Validated newtypes for frequencies, tones, modes, and channels.
//! - [`protocol`] — Pure-logic CAT command codec (serialize / parse).
//! - [`transport`] — Async I/O trait and serial / mock implementations.
//! - [`radio`] — High-level async API wrapping the protocol and transport layers.
//! - [`aprs`] — KISS TNC framing, AX.25 packet parsing, and APRS position decoding.
//! - [`mmdvm`] — MMDVM serial protocol codec, D-STAR header, and slow data decoder.
//! - [`error`] — Error types for transport, protocol, and validation failures.

pub mod aprs;
pub mod error;
pub mod memory;
pub mod mmdvm;
pub mod protocol;
pub mod radio;
pub mod sdcard;
pub mod transport;
pub mod types;

// Convenience re-exports for the most commonly used types.
pub use error::Error;
pub use radio::Radio;
pub use radio::programming::McpSpeed;
#[cfg(target_os = "macos")]
pub use transport::BluetoothTransport;
pub use transport::{EitherTransport, MockTransport, SerialTransport, Transport};

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
    AprsPosition, AprsQuery, AprsStatus, AprsTelemetry, AprsWeather, DigiAction, DigipeaterConfig,
    Phg, SmartBeaconing, SmartBeaconingConfig, StationEntry, StationList, build_aprs_item,
    build_aprs_message, build_aprs_mice, build_aprs_object, build_aprs_position_compressed,
    build_aprs_position_report, build_aprs_status, build_aprs_weather,
    build_query_response_position, parse_aprs_extensions,
};
pub use aprs_is::{
    AprsIsClient, AprsIsConfig, AprsIsError, AprsIsEvent, aprs_is_passcode, build_login_string,
    format_is_packet, parse_is_line,
};
pub use ax25_codec::{Ax25Address, Ax25Error, Ax25Packet};
pub use kiss_tnc::{KissError, KissFrame};

// D75-specific re-exports.
pub use aprs::client::{AprsClient, AprsClientConfig, AprsEvent};

// KISS session re-export.
pub use radio::kiss_session::KissSession;

// MMDVM session re-export.
pub use radio::mmdvm_session::MmdvmSession;

// MMDVM gateway re-exports. Raw codec types live in mmdvm-core; the
// async event loop lives in mmdvm. The types re-exported here
// compose those crates into the D-STAR-specific surface
// TH-D75 consumers use.
pub use mmdvm::{
    DStarEvent, DStarGateway, DStarGatewayConfig, LastHeardEntry, MmdvmError, ModemMode,
    ModemStatus, NakReason, ReconnectPolicy,
};

// SD card re-exports.
pub use sdcard::SdCardError;
pub use sdcard::config::{ConfigHeader, write_d75};
