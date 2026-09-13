//! Errors reported by byte transports and their platform connection owners.

use thiserror::Error;

/// Host-observed stage at which one native Bluetooth opening attempt failed.
///
/// A stage identifies the helper's boundary, not a cause in the peer's firmware
/// or evidence that another attempt would succeed. Timeout stages use the
/// shared opening deadline. Retry policy belongs to the model or caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothOpenStage {
    /// The opening deadline expired before startup event processing admitted SDP.
    StartupDeadline,
    /// The helper could not allocate its native opening context.
    ContextAllocation,
    /// The framework rejected dispatch of service discovery.
    SdpStart,
    /// Service discovery or its required baseband state failed to complete.
    SdpCompletion,
    /// The opening deadline expired before service discovery was admitted.
    SdpDeadline,
    /// Service records did not resolve to exactly one valid Serial Port channel.
    ServiceResolution,
    /// The framework rejected dispatch of the RFCOMM opening request.
    RfcommStart,
    /// RFCOMM completion failed or the channel closed before admission.
    RfcommCompletion,
    /// The opening deadline expired before RFCOMM admission.
    RfcommDeadline,
    /// The opened channel was absent or did not match its requested channel.
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

/// Why native channel release could not be proved within its cleanup bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothCloseFailure {
    /// Native close confirmation or callback-delegate detachment was not proved.
    ChannelUnconfirmed,
    /// The helper exited with an unsuccessful code or signal.
    HelperExited {
        /// Process exit code, absent when termination was signal-driven.
        code: Option<i32>,
    },
    /// Cleanup had to terminate the helper instead of confirming native close.
    ForcedTermination,
    /// The helper remains owned by deferred cleanup; release is not yet proved.
    ReapPending,
}

/// Errors originating from the transport layer (serial port / Bluetooth).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// A native opening attempt failed at a known host-observed stage.
    #[error("Bluetooth open failed during {stage}")]
    BluetoothOpen {
        /// Host-observed failure boundary, not an inferred firmware cause.
        stage: BluetoothOpenStage,
    },
    /// An opening failure and independent native cleanup uncertainty.
    ///
    /// The shared transport produces this variant only after receiving a
    /// complete failure record and reaping the helper with its matching exit
    /// status. Reaping proves process retirement, not channel closure or
    /// cancellation of any operation still owned by the operating system.
    /// Retry permission remains an explicit model or caller policy.
    #[error(
        "Bluetooth open failed during {stage}; native cleanup was not proved: {cleanup:?} (helper reaped)"
    )]
    BluetoothOpenWithCleanup {
        /// The original host-observed opening failure, retained without replacement.
        stage: BluetoothOpenStage,
        /// Independent native cleanup failure, not the cause of the opening failure.
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

    /// Native Bluetooth release failed or could not be proved.
    #[error("Bluetooth channel closure was not proved: {failure:?}")]
    BluetoothClose {
        /// Conservative outcome of bounded cleanup.
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
