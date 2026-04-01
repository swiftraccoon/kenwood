#![forbid(unsafe_code)]
//! Async Rust library for controlling the Kenwood TH-D75 transceiver via
//! CAT (Computer Aided Transceiver) — the serial command protocol Kenwood
//! uses for remote radio control.
//!
//! This library supports all 55 CAT commands over USB serial or Bluetooth
//! SPP connections. Command definitions and validation rules are based on
//! analysis of TH-D75 firmware v1.03.000.
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
//! - [`kiss`] — KISS TNC framing, AX.25 packet parsing, and APRS position decoding.
//! - [`error`] — Error types for transport, protocol, and validation failures.

pub mod error;
pub mod kiss;
pub mod memory;
pub mod protocol;
pub mod radio;
pub mod sdcard;
pub mod transport;
pub mod types;

// Convenience re-exports for the most commonly used types.
pub use error::Error;
pub use radio::Radio;
pub use radio::programming::McpSpeed;
pub use transport::{MockTransport, SerialTransport, Transport};
