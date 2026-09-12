//! TH-D75 endpoint selection, serial presets, and native Bluetooth access.
//!
//! Model-neutral I/O contracts, mocks, and byte-stream adapters live in
//! [`kenwood_transport`]. This module owns the TH-D75's physical-interface
//! choices and reopening policy, not generic transport mechanics.
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
//! - [`kenwood_transport::MockTransport`]: Programmed exchanges for testing
//!
//! On macOS, use `BluetoothTransport` for Bluetooth connections. Apple's
//! Bluetooth serial driver drops data for this radio; `BluetoothTransport`
//! bypasses that device node and talks directly to RFCOMM in an isolated
//! helper process.

#[cfg(any(target_os = "macos", all(doc, unix)))]
pub mod bluetooth;
pub mod broker;
pub mod either;
pub mod serial;

#[cfg(any(target_os = "macos", all(doc, unix)))]
pub use bluetooth::{BluetoothOpenCancellation, BluetoothTransport, PairedBluetoothDevice};
pub use broker::{BrokerHandle, MainThreadBroker};
pub use either::EitherTransport;
pub use serial::SerialTransport;
