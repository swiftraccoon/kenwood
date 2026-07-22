//! `Command` enum for the channel between an [`super::AsyncModem`]
//! handle and the spawned [`super::ModemLoop`] task.

use mmdvm_core::ModemMode;
use tokio::sync::oneshot;

use crate::error::ShellError;

/// Commands the consumer sends to the modem loop via
/// [`super::AsyncModem`].
#[derive(Debug)]
pub(crate) enum Command {
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
    /// Send a raw frame: escape hatch for modes we haven't modelled
    /// yet.
    SendRaw {
        /// The command byte.
        command: u8,
        /// The payload bytes (may be empty).
        payload: Vec<u8>,
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
