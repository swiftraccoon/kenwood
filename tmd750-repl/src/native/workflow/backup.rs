//! Guarded standard-page reads retaining the original native owner until close.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::{DvGatewayMode, Identity, McpBackupReport, Radio};
use kenwood_transport::Transport;

use crate::capture::{CaptureTransport, Failure, TranscriptSummary};
use crate::output;

/// Read evidence and final cleanup remain independent after owner retirement.
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

/// A completed or interrupted read phase, with no exposed protocol access.
pub(super) struct PendingClose<T> {
    transport: CaptureTransport<T, File>,
    backup: Option<McpBackupReport>,
}

impl<T: Transport> PendingClose<T> {
    /// A successful flush and exit permit a silent retained-owner wait only.
    pub(super) fn ready_to_settle(&mut self, cancelled: &AtomicBool) -> bool {
        self.transport.synchronize().is_ok()
            && !cancelled.load(Ordering::Relaxed)
            && self.backup.as_ref().is_some_and(completed)
    }

    /// Retire the original owner even after cancellation or failed framing.
    pub(super) async fn finish(self) -> Observation {
        let Self {
            mut transport,
            backup,
        } = self;
        let close_error = crate::native::close(&mut transport).await;
        let mut recorder = transport.into_recorder();
        // Synchronization failure is sticky in the summary and never replaces cleanup.
        let _synchronized = recorder.synchronize();
        Observation {
            backup,
            close_error,
            transcript: recorder.summary(),
        }
    }
}

/// Use the library's read-only standard engine with exact Gateway-Off admission.
pub(super) async fn read<T: Transport>(
    mut transport: CaptureTransport<T, File>,
    cancelled: &AtomicBool,
) -> PendingClose<T> {
    let ready = transport.synchronize().is_ok() && !cancelled.load(Ordering::Relaxed);
    let mut radio = Radio::new(transport);
    radio.set_timeout(crate::native::cat::EXCHANGE_TIMEOUT);
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
