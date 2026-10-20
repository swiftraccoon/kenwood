//! TH-D75 endpoint selection, serial presets, and native Bluetooth access.
//!
//! Model-neutral I/O contracts, mocks, and byte-stream adapters live in
//! [`kenwood_transport`]. This module owns the TH-D75's physical-interface
//! choices and reopening policy, not generic transport mechanics.
//!
//! [Kenwood's TH-D75A/E Operating Tips, §§5.12-5.13][operating-tips],
//! describes serial access through USB CDC and Bluetooth SPP over RFCOMM.
//!
//! Implementations:
//! - [`SerialTransport`]: USB serial connections, plus serial RFCOMM on
//!   Linux and Windows
//! - `BluetoothTransport`: Native macOS `IOBluetooth` RFCOMM (macOS only)
//! - [`kenwood_transport::MockTransport`]: Programmed exchanges for testing
//!
//! On macOS, use `BluetoothTransport` for Bluetooth connections. Apple's
//! Bluetooth serial driver drops data for this radio; `BluetoothTransport`
//! bypasses that device node and talks directly to RFCOMM in an isolated
//! helper process.
//!
//! [operating-tips]: https://www.kenwood.com/i/products/info/amateur/pdf/TH-D75AE_IDM.pdf

#[cfg(target_os = "macos")]
pub use bluetooth::BluetoothTransport;
pub use either::EitherTransport;
pub use serial::SerialTransport;

#[cfg(target_os = "macos")]
pub mod bluetooth;
pub mod either;
pub mod serial;
