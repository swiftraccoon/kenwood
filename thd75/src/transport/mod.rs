//! Async transport trait and implementations for radio communication.
//!
//! The TH-D75 communicates over USB CDC ACM (Communications Device Class
//! Abstract Control Model) which presents as a standard serial port, and
//! Bluetooth SPP (Serial Port Profile) via RFCOMM.
//!
//! # Bluetooth (per Operating Tips §5.12)
//!
//! - Bluetooth version 3.0, Class 2 (range ~10m)
//! - Profiles: HSP (Headset Profile) + SPP (Serial Port Profile)
//! - No BLE (Bluetooth Low Energy) and no HFP (Hands-Free Profile)
//! - BT headset provides mic + earphone for voice; PTT remains on the
//!   radio body (no BT PTT except via VOX)
//! - Menu No. 112: BT microphone sensitivity adjustment
//! - When a BT headset is connected, audio is NOT routed to the USB
//!   port or external speaker jack
//! - Menu No. 933: view/manage connected BT devices
//!
//! # USB (per Operating Tips §5.13)
//!
//! - CDC virtual COM port
//! - USB audio output: 48 kHz / 16-bit / mono, output only (same as speaker
//!   output). Adjustable via Menu No. 91A.
//! - USB Mass Storage: Menu No. 980 (Windows only for mass storage feature)
//!
//! Implementations:
//! - [`SerialTransport`]: USB serial connections, plus serial RFCOMM on
//!   Linux and Windows
//! - `BluetoothTransport`: Native macOS `IOBluetooth` RFCOMM (macOS only)
//! - [`MockTransport`]: Programmed exchanges for testing
//!
//! On macOS, use `BluetoothTransport` for Bluetooth connections. Apple's
//! Bluetooth serial driver drops data for this radio; `BluetoothTransport`
//! bypasses that device node and talks directly to RFCOMM in an isolated
//! helper process.

#[cfg(any(target_os = "macos", all(doc, unix)))]
pub mod bluetooth;
pub mod broker;
pub mod either;
pub mod mmdvm_adapter;
pub mod mock;
pub mod serial;

#[cfg(any(target_os = "macos", all(doc, unix)))]
pub use bluetooth::{BluetoothOpenCancellation, BluetoothTransport, PairedBluetoothCandidate};
pub use broker::{BrokerHandle, MainThreadBroker};
pub use either::EitherTransport;
pub use mmdvm_adapter::{MmdvmTransportAdapter, MmdvmTransportRecoveryError};
pub use mock::MockTransport;
pub use serial::SerialTransport;

use std::future::Future;

use crate::error::TransportError;

/// Async transport for communicating with the radio.
///
/// Implemented for USB serial (CDC ACM), Bluetooth SPP (Serial Port
/// Profile), and mock (testing).
pub trait Transport: Send + Sync {
    /// Send raw bytes to the radio.
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Read available bytes into `buf`, returning the initialized byte count.
    ///
    /// # Cancellation safety
    ///
    /// This future must be cancellation-safe: if it is dropped while pending,
    /// the next call must still be able to deliver every byte that was not
    /// returned to the caller. [`MmdvmTransportAdapter`] deliberately races a
    /// pending read against outbound work so one blocked read cannot prevent a
    /// write; that race cancels and recreates the losing read future.
    ///
    /// Returning a count larger than `buf.len()` violates this trait's
    /// contract. Consumers treat that as terminal transport corruption rather
    /// than indexing beyond the initialized region.
    fn read(
        &mut self,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<usize, TransportError>> + Send;

    /// Close the connection.
    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Change the transport baud rate.
    ///
    /// Used when switching between CAT mode (115200 baud over CDC ACM)
    /// and programming mode (9600 baud for the entire session). No-op
    /// for transports that do not support baud rate changes (e.g., mock).
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] if the baud rate cannot be applied.
    fn set_baud_rate(&mut self, _baud: u32) -> Result<(), TransportError> {
        Ok(())
    }

    /// Re-establish a dropped connection using the same identity
    /// (device path / name / discovery parameters) this transport was
    /// opened with.
    ///
    /// Implementations own their platform's full recovery sequence,
    /// including any mandatory release/settle delays. The default
    /// declines: transports that cannot recover their own connection
    /// report [`TransportError::ReopenUnsupported`] and the caller
    /// must build a fresh transport instead.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::ReopenUnsupported`] if this transport
    /// cannot reopen; implementation-specific errors otherwise.
    fn reopen(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        async { Err(TransportError::ReopenUnsupported) }
    }
}
