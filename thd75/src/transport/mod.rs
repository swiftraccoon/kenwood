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
//! - CDC virtual COM port (same driver as TH-D74, available at kenwood.com)
//! - USB audio output: 48 kHz / 16-bit / mono, output only (same as speaker
//!   output). Adjustable via Menu No. 91A.
//! - USB Mass Storage: Menu No. 980 (Windows only for mass storage feature)
//!
//! Implementations:
//! - [`SerialTransport`] — USB serial connections (and BT via `/dev/cu.*`)
//! - [`BluetoothTransport`] — Native macOS `IOBluetooth` RFCOMM (macOS only)
//! - [`MockTransport`] — Programmed exchanges for testing
//!
//! On macOS, prefer [`BluetoothTransport`] over [`SerialTransport`] for BT
//! connections. The macOS serial port driver has a bug where closing and
//! reopening `/dev/cu.TH-D75` kills the RFCOMM channel permanently.
//! [`BluetoothTransport`] talks directly to the RFCOMM channel via
//! `IOBluetooth` and can be closed and reopened without issues.

#[cfg(any(target_os = "macos", doc))]
pub mod bluetooth;
pub mod either;
pub mod mock;
pub mod serial;

#[cfg(any(target_os = "macos", doc))]
pub use bluetooth::BluetoothTransport;
pub use either::EitherTransport;
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

    /// Read available bytes into buffer, return count of bytes read.
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
}
