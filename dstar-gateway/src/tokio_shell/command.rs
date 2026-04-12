//! `Command` enum for the channel between an `AsyncSession` handle
//! and the spawned `SessionLoop` task.

use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::types::StreamId;
use dstar_gateway_core::voice::VoiceFrame;

use super::ShellError;

/// Commands sent from a user-facing [`super::AsyncSession`] handle
/// to the spawned tokio task that drives the sans-io core.
#[derive(Debug)]
pub enum Command {
    /// Send a voice header and start a new outbound stream.
    SendHeader {
        /// The header to transmit.
        header: Box<DStarHeader>,
        /// Stream id for the voice burst.
        stream_id: StreamId,
        /// Reply channel — `Ok(())` on success, or the shell error.
        reply: tokio::sync::oneshot::Sender<Result<(), ShellError>>,
    },

    /// Send a voice data frame.
    SendVoice {
        /// Stream id.
        stream_id: StreamId,
        /// Frame seq (0..21 cycle).
        seq: u8,
        /// 9 AMBE bytes + 3 slow data bytes.
        frame: Box<VoiceFrame>,
        /// Reply channel.
        reply: tokio::sync::oneshot::Sender<Result<(), ShellError>>,
    },

    /// Send a voice EOT and close the outbound stream.
    SendEot {
        /// Stream id.
        stream_id: StreamId,
        /// Final seq.
        seq: u8,
        /// Reply channel.
        reply: tokio::sync::oneshot::Sender<Result<(), ShellError>>,
    },

    /// Initiate a graceful disconnect and wait for UNLINK ACK or timeout.
    Disconnect {
        /// Reply channel — fires when disconnect is complete.
        reply: tokio::sync::oneshot::Sender<()>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::types::StreamId;

    #[expect(clippy::unwrap_used, reason = "const-validated: 0x1234 is non-zero")]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
    }

    #[test]
    fn command_send_eot_constructs() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let cmd = Command::SendEot {
            stream_id: sid(0x1234),
            seq: 5,
            reply: tx,
        };
        assert!(matches!(cmd, Command::SendEot { .. }));
    }
}
