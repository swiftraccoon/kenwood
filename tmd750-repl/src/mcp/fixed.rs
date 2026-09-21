//! Fixed two-fragment MCP read, shared by the transport-specific workflows.
//!
//! Every read covers the same two library-defined fragments, address 8 for 40
//! bytes and address 327681 for 255 bytes, and ends at the acknowledged MCP
//! exit. No address, length or write frame is caller supplied, and the whole
//! exchange is recorded through a [`CaptureTransport`].

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::{DvGatewayMode, McpProbeReport, Radio};
use kenwood_transport::Transport;

use crate::capture::{CaptureTransport, Failure, TranscriptSummary};

/// Acceptance criterion a workflow applies to a completed fixed MCP read.
#[derive(Clone, Copy)]
pub(crate) enum Admission {
    /// Accept a complete read and exit; the identity tuple is the only check.
    FixedIdentity,
    /// Additionally require a `GW` reading of Off taken in the same session.
    GatewayOff,
}

/// Result of one fixed MCP read, after the connection has been closed.
///
/// Holds the probe report, the observed Gateway mode, any close failure and the
/// transcript summary. A `None` `close_error` alone does not mean the read
/// completed; test success with [`Observation::verified_identity`].
pub(crate) struct Observation {
    admission: Admission,
    pub(crate) probe: Option<McpProbeReport>,
    pub(crate) gateway_mode: Option<DvGatewayMode>,
    pub(crate) close_error: Option<Failure>,
    pub(crate) transcript: TranscriptSummary,
}

impl Observation {
    /// Identity tuple of a read that satisfied this observation's [`Admission`].
    ///
    /// Returns `None` when the Gateway criterion is unmet, the two fragments or
    /// the exit ACK are missing, the close failed, the transcript is incomplete,
    /// or cancellation was requested.
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

/// Run the fixed read on an already open connection, then close it.
pub(crate) async fn observe(
    transport: CaptureTransport<impl Transport, File>,
    admission: Admission,
    cancelled: &AtomicBool,
) -> Observation {
    read(transport, admission, cancelled).await.finish().await
}

/// A completed fixed read whose connection is still open.
///
/// No protocol call is exposed while a workflow waits out the settle delay;
/// [`PendingClose::finish`] closes the connection and returns the
/// [`Observation`].
pub(crate) struct PendingClose<T> {
    transport: CaptureTransport<T, File>,
    admission: Admission,
    probe: Option<McpProbeReport>,
    gateway_mode: Option<DvGatewayMode>,
}

impl<T: Transport> PendingClose<T> {
    /// True when the caller may hold this connection open through the settle delay.
    ///
    /// True requires a synchronized transcript, a satisfied [`Admission`] and an
    /// eligible probe. It does not mean the connection has been closed.
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

    /// Close the connection within 2 s, synchronize the capture, and report.
    ///
    /// The close runs whatever the settle wait returned; a close or
    /// synchronization failure is recorded in the returned [`Observation`].
    pub(crate) async fn finish(self) -> Observation {
        let Self {
            mut transport,
            admission,
            probe,
            gateway_mode,
        } = self;
        let close_error = super::close_transport(&mut transport).await;
        let mut recorder = transport.into_recorder();
        // A synchronization failure lands in the summary and cancellation flag.
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

/// Run the fixed read up to the MCP exit, leaving the connection open.
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
