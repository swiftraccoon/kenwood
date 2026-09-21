//! Errors reported by byte transports and their platform backends.

use thiserror::Error;

/// Host-observed stage at which one native Bluetooth opening attempt failed.
///
/// A stage names the helper operation that failed on this host, not a cause in
/// the peer's firmware. Every deadline stage refers to the one absolute opening
/// deadline shared by startup processing, service discovery and RFCOMM opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothOpenStage {
    /// The opening deadline expired during the startup run-loop slice, before
    /// the service-discovery query was issued.
    StartupDeadline,
    /// The helper could not allocate its native opening context or delegates.
    ContextAllocation,
    /// The framework rejected dispatch of the service-discovery query.
    SdpStart,
    /// The service-discovery callback reported failure, or the device never
    /// reported a connected baseband.
    SdpCompletion,
    /// The opening deadline expired while waiting for the service-discovery
    /// callback, or before the RFCOMM opening request was dispatched.
    SdpDeadline,
    /// Service records did not resolve to exactly one Serial Port channel in
    /// the range 1 to 30.
    ServiceResolution,
    /// The framework rejected dispatch of the RFCOMM opening request.
    RfcommStart,
    /// The RFCOMM open callback reported failure, or the channel closed before
    /// that callback fired.
    RfcommCompletion,
    /// The opening deadline expired before the RFCOMM open callback fired.
    RfcommDeadline,
    /// The opened channel was absent or carried a channel number other than
    /// the requested one.
    RfcommEndpoint,
}

impl std::fmt::Display for BluetoothOpenStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::StartupDeadline => "native startup event-processing deadline",
            Self::ContextAllocation => "native context allocation",
            Self::SdpStart => "SDP request dispatch",
            Self::SdpCompletion => "SDP completion",
            Self::SdpDeadline => "SDP completion deadline",
            Self::ServiceResolution => "SDP Serial Port service resolution",
            Self::RfcommStart => "RFCOMM opening request dispatch",
            Self::RfcommCompletion => "RFCOMM opening completion",
            Self::RfcommDeadline => "RFCOMM opening deadline",
            Self::RfcommEndpoint => "RFCOMM endpoint validation",
        })
    }
}

/// Why native channel release could not be confirmed within its cleanup bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothCloseFailure {
    /// The channel-closed callback did not arrive, or detaching the callback
    /// delegate failed.
    ChannelUnconfirmed,
    /// The helper exited with an unsuccessful code or signal.
    HelperExited {
        /// Process exit code, absent when termination was signal-driven.
        code: Option<i32>,
    },
    /// Cleanup had to terminate the helper instead of confirming native close.
    ForcedTermination,
    /// The helper has not been reaped yet, so its channel release is unconfirmed.
    ReapPending,
}

/// Errors originating from the transport layer (serial port / Bluetooth).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// A native opening attempt failed at a known host-observed stage.
    #[error("Bluetooth open failed during {stage}")]
    BluetoothOpen {
        /// Helper operation at which the attempt failed.
        stage: BluetoothOpenStage,
    },
    /// An opening failure plus an unconfirmed native channel close.
    ///
    /// Produced only after the helper writes a complete failure record and is
    /// reaped with the exit status that record names. The helper process is
    /// gone, but the RFCOMM channel may still be open in the operating system
    /// and any operation the OS still owns is not cancelled. This crate never
    /// retries after this error.
    #[error(
        "Bluetooth open failed during {stage}; native cleanup unconfirmed: {cleanup:?} (helper reaped)"
    )]
    BluetoothOpenWithCleanup {
        /// Helper operation at which the attempt failed, unchanged by cleanup.
        stage: BluetoothOpenStage,
        /// Cleanup failure recorded while abandoning the attempt; it has its
        /// own cause and did not produce `stage`.
        cleanup: BluetoothCloseFailure,
    },
    /// A typed Bluetooth selector or channel could not be constructed.
    #[error("invalid Bluetooth {parameter}: {value:?}")]
    BluetoothParameter {
        /// Parameter whose domain was violated.
        parameter: &'static str,
        /// Rejected input, retained without normalization.
        value: String,
    },

    /// Native Bluetooth release failed or could not be confirmed.
    #[error("Bluetooth channel closure unconfirmed: {failure:?}")]
    BluetoothClose {
        /// Why bounded cleanup could not confirm the close.
        failure: BluetoothCloseFailure,
    },
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

    /// No matching device was found.
    #[error("no matching device found")]
    NotFound,

    /// The transport connection was lost.
    #[error("transport connection lost")]
    Disconnected(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// A transport write failed.
    #[error("transport write failed")]
    Write(
        /// The underlying I/O error.
        #[source]
        std::io::Error,
    ),

    /// A transport read failed.
    #[error("transport read failed")]
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
