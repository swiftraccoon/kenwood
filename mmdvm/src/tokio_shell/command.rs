//! Requests between an [`super::AsyncModem`]
//! handle and the spawned [`super::ModemLoop`] task.

use mmdvm_core::{MmdvmFrame, ModemMode};
use tokio::sync::oneshot;

use crate::error::ShellError;

use super::DstarConfiguration;

/// Commands the consumer sends to the modem loop via
/// [`super::AsyncModem`].
#[derive(Debug)]
pub(crate) enum Request {
    /// Configure D-STAR after confirming the selected protocol version.
    ConfigureDstar {
        /// Validated version-specific configuration.
        configuration: DstarConfiguration,
        /// Total submission, version, and ACK deadline.
        deadline: tokio::time::Instant,
        /// Resolves with the configuration ACK, NAK, or pre-write rejection.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Send a `GetVersion` request.
    GetVersion {
        /// Reply channel: `Ok(())` once the request is framed and
        /// written to the transport.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Send a `GetStatus` request.
    GetStatus {
        /// Reply channel: `Ok(())` once the request is framed and
        /// written to the transport.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Send a `SetMode` command.
    SetMode {
        /// Target mode.
        mode: ModemMode,
        /// Absolute deadline, including channel backpressure.
        deadline: tokio::time::Instant,
        /// Reply channel, resolved with the modem's ACK/NAK for the
        /// mode change, not merely with the write result.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Enqueue a D-STAR header (41 bytes) in the loop's TX queue.
    ///
    /// Actual wire transmission is gated on the modem reporting
    /// strictly more than 4 free D-STAR FIFO slots, matching
    /// `MMDVMHost/Modem.cpp:1053`.
    SendDStarHeader {
        /// The 41 header bytes.
        bytes: [u8; 41],
        /// Reply channel: `Ok(())` once the frame has been queued.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Enqueue a D-STAR voice data frame (12 bytes).
    SendDStarData {
        /// 9 AMBE + 3 slow-data bytes.
        bytes: [u8; 12],
        /// Reply channel.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Enqueue a D-STAR end-of-transmission marker.
    SendDStarEot {
        /// Reply channel.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Send a raw frame: the escape hatch for modes without a typed
    /// command.
    SendRaw {
        /// A frame whose payload length has already been validated.
        frame: MmdvmFrame,
        /// Reply channel.
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    /// Trigger graceful shutdown of the loop.
    Shutdown {
        /// Reply channel, fired when the loop acknowledges the
        /// shutdown request.
        reply: oneshot::Sender<()>,
    },
}
