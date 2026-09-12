//! Errors reported by byte transports and their platform connection owners.

use thiserror::Error;

/// Errors originating from the transport layer (serial port / Bluetooth).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Failed to open the serial port at the given path.
    #[error("failed to open serial port at {path}")]
    Open {
        /// The filesystem path that could not be opened.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The isolated macOS Bluetooth helper could not select one paired radio,
    /// be prepared or launched, or complete its readiness handshake.
    #[error("Bluetooth helper failed during {context}")]
    BluetoothHelper {
        /// Operation or resource that failed.
        context: String,
        /// The underlying process or pipe error.
        source: std::io::Error,
    },

    /// More than one paired Bluetooth device has the requested display name.
    #[error(
        "multiple paired Bluetooth devices have the requested name; pass an exact Bluetooth address instead"
    )]
    BluetoothDeviceNameAmbiguous,

    /// A caller cancelled a bounded macOS Bluetooth discovery or open.
    #[error("Bluetooth helper open was interrupted")]
    BluetoothOpenInterrupted,

    /// No matching serial device was found.
    #[error("no matching serial device found")]
    NotFound,

    /// The serial connection was lost.
    #[error("serial connection lost")]
    Disconnected(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// A write to the serial port failed.
    #[error("serial write failed")]
    Write(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// A read from the serial port failed.
    #[error("serial read failed")]
    Read(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// The transport cannot re-establish its own connection.
    ///
    /// Returned by the default [`Transport::reopen`] implementation.
    /// Callers must build a fresh transport instead.
    ///
    /// [`Transport::reopen`]: crate::Transport::reopen
    #[error("this transport cannot reopen its connection")]
    ReopenUnsupported,

    /// A thread-affine third-party transport refused this reopen call.
    ///
    /// Process-isolated transports do not have this restriction. This variant
    /// is available to custom transports whose platform API requires its
    /// original opening thread.
    #[error("reopen must run on the thread that opened the transport")]
    WrongThread,

    /// The main-thread broker has been dropped and cannot execute more jobs.
    #[error("the main-thread transport broker is no longer available")]
    BrokerUnavailable,
}
