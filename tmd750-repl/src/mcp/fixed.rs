//! Captured fixed MCP reads shared by explicitly selected transport workflows.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::{DvGatewayMode, McpProbeReport, Radio};
use kenwood_transport::Transport;

use crate::capture::{CaptureTransport, Failure, TranscriptSummary};

/// Admission belongs to the selected workflow, not to a transport implementation.
#[derive(Clone, Copy)]
pub(crate) enum Admission {
    /// Preserve the established USB fixed-read identity and exchange schedule.
    FixedIdentity,
    /// Require the library's exact qualification tuple and a fresh Gateway Off.
    GatewayOff,
}

/// Protocol evidence and cleanup remain independent; dropping is not success.
pub(crate) struct Observation {
    admission: Admission,
    pub(crate) probe: Option<McpProbeReport>,
    pub(crate) gateway_mode: Option<DvGatewayMode>,
    pub(crate) close_error: Option<Failure>,
    pub(crate) transcript: TranscriptSummary,
}

impl Observation {
    pub(crate) fn verified_identity(
        &self,
        cancelled: &AtomicBool,
    ) -> Option<&kenwood_tmd750::Identity> {
        if matches!(self.admission, Admission::GatewayOff)
            && self.gateway_mode != Some(DvGatewayMode::Off)
        {
            return None;
        }
        self.probe.as_ref().and_then(|probe| {
            super::verification_eligibility(
                probe,
                self.close_error.as_ref(),
                &self.transcript,
                cancelled,
            )
            .ok()
        })
    }
}

/// Own one already opened connection through the library's fixed read schedule.
pub(crate) async fn observe(
    transport: CaptureTransport<impl Transport, File>,
    admission: Admission,
    cancelled: &AtomicBool,
) -> Observation {
    read(transport, admission, cancelled).await.finish().await
}

/// An exited read phase retaining its owner until explicit bounded cleanup.
/// No protocol access is exposed while a workflow waits for the exit to settle.
pub(crate) struct PendingClose<T> {
    transport: CaptureTransport<T, File>,
    admission: Admission,
    probe: Option<McpProbeReport>,
    gateway_mode: Option<DvGatewayMode>,
}

impl<T: Transport> PendingClose<T> {
    /// Require complete synchronized read/exit evidence before a silent wait.
    /// This is not close confirmation or admission to another connection.
    pub(crate) fn ready_to_settle(&mut self, cancelled: &AtomicBool) -> bool {
        self.transport.synchronize().is_ok()
            && (!matches!(self.admission, Admission::GatewayOff)
                || self.gateway_mode == Some(DvGatewayMode::Off))
            && self.probe.as_ref().is_some_and(|probe| {
                super::verification_eligibility(
                    probe,
                    None,
                    &self.transport.transcript_summary(),
                    cancelled,
                )
                .is_ok()
            })
    }

    /// Always retire the retained owner, independently of a wait's outcome.
    pub(crate) async fn finish(self) -> Observation {
        let Self {
            mut transport,
            admission,
            probe,
            gateway_mode,
        } = self;
        let close_error = super::close_transport(&mut transport).await;
        let mut recorder = transport.into_recorder();
        // Synchronization records failure in the summary and cancellation flag.
        let _synchronized = recorder.synchronize();
        Observation {
            admission,
            probe,
            gateway_mode,
            close_error,
            transcript: recorder.summary(),
        }
    }
}

/// Read through the library's fixed exit boundary without releasing its owner.
pub(crate) async fn read<T: Transport>(
    mut transport: CaptureTransport<T, File>,
    admission: Admission,
    cancelled: &AtomicBool,
) -> PendingClose<T> {
    let ready = transport.synchronize().is_ok() && !cancelled.load(Ordering::Relaxed);
    let mut radio = Radio::new(transport);
    let (probe, gateway_mode) = if ready {
        match admission {
            Admission::FixedIdentity => (
                Some(radio.probe_mcp(|| cancelled.load(Ordering::Relaxed)).await),
                None,
            ),
            Admission::GatewayOff => {
                let observed = radio
                    .probe_mcp_gateway_off_until_exit(|| cancelled.load(Ordering::Relaxed))
                    .await;
                (Some(observed.probe), observed.gateway_mode)
            }
        }
    } else {
        (None, None)
    };
    PendingClose {
        transport: radio.into_transport(),
        admission,
        probe,
        gateway_mode,
    }
}
