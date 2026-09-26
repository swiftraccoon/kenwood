//! Read the standard configuration pages, holding the original connection
//! open until the read phase finishes.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::{DvGatewayMode, Identity, McpBackupReport};
use kenwood_transport::Transport;

use crate::capture::{CaptureTransport, Failure, TranscriptSummary};
use crate::output;

/// Result of a standard-page read: the backup report when the read ran, any
/// close error, and the transcript summary. A close or capture failure never
/// discards the pages already read.
pub(super) struct Observation {
    pub(super) backup: Option<McpBackupReport>,
    pub(super) close_error: Option<Failure>,
    pub(super) transcript: TranscriptSummary,
}

impl Observation {
    pub(super) fn verified_identity(&self, cancelled: &AtomicBool) -> Option<&Identity> {
        if cancelled.load(Ordering::Relaxed)
            || self.close_error.is_some()
            || !self.transcript.complete
        {
            return None;
        }
        self.backup
            .as_ref()
            .filter(|report| completed(report))?
            .identity
            .as_ref()
    }
}

fn completed(report: &McpBackupReport) -> bool {
    report.has_complete_configuration() && report.gateway_mode == Some(DvGatewayMode::Off)
}

/// A finished or interrupted read phase whose connection is not yet closed.
///
/// The transport is private, so no further protocol traffic can be sent.
pub(super) struct PendingClose<T> {
    transport: CaptureTransport<T, File>,
    backup: Option<McpBackupReport>,
}

impl<T: Transport> PendingClose<T> {
    /// Whether the settle wait may run: the transcript flushed, no
    /// cancellation, and a complete backup that reached the MCP exit.
    pub(super) fn ready_to_settle(&mut self, cancelled: &AtomicBool) -> bool {
        self.transport.synchronize().is_ok()
            && !cancelled.load(Ordering::Relaxed)
            && self.backup.as_ref().is_some_and(completed)
    }

    /// Close the connection and return the observation, in every path.
    pub(super) async fn finish(self) -> Observation {
        let Self {
            mut transport,
            backup,
        } = self;
        let close_error = crate::native::close(&mut transport).await;
        let mut recorder = transport.into_recorder();
        // A synchronization failure stays in the summary; it never replaces
        // the close result.
        let _synchronized = recorder.synchronize();
        Observation {
            backup,
            close_error,
            transcript: recorder.summary(),
        }
    }
}

/// Read every standard configuration page, requiring Gateway Off.
///
/// The read is skipped, leaving `backup` as `None`, when the transcript cannot
/// be flushed or `cancelled` is already set.
pub(super) async fn read<T: Transport>(
    mut transport: CaptureTransport<T, File>,
    cancelled: &AtomicBool,
) -> PendingClose<T> {
    let ready = transport.synchronize().is_ok() && !cancelled.load(Ordering::Relaxed);
    let mut radio = crate::native::cat::wrap(transport);
    let backup = if ready {
        Some(
            radio
                .backup_mcp_gateway_off_until_exit(
                    || cancelled.load(Ordering::Relaxed),
                    |progress| {
                        if progress.done == 1
                            || progress.done % 64 == 0
                            || progress.done == progress.total
                        {
                            output::line(format_args!(
                                "Configuration pages: {}/{}.",
                                progress.done, progress.total
                            ));
                        }
                    },
                )
                .await,
        )
    } else {
        None
    };
    PendingClose {
        transport: radio.into_transport(),
        backup,
    }
}
