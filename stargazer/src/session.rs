// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Per-target session supervision: connect (with protocol-specific
//! ladders), pump events into the capture core, write completed
//! recordings, and heal drops with jittered exponential backoff.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dstar_gateway::tokio_shell::{
    AnyAsyncSession, AnyEvent, AsyncSession, ShellError, drive_connecting,
};
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::session::client::{
    Configured, DExtra, DPlus, Dcs, DisconnectReason, Session, VoiceEndReason,
};
use dstar_gateway_core::{
    DcsReflector, DstarHeader, Module, ProtocolKind, RoutingCallsign, StreamId, VoiceFrame,
};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::capture::{
    CaptureDurationLimit, CaptureManager, CompletedRecording, ConcurrentCaptureLimit, EndReason,
    StreamOrigin,
};
use crate::config::Target;
use crate::writer::Writer;

/// Jittered exponential backoff: full jitter over a doubling window,
/// 1 s initial, 60 s cap.
#[derive(Debug)]
struct Backoff {
    attempt: u32,
    state: u64,
}

impl Backoff {
    /// Create a backoff with a jitter seed (pass clock nanos for
    /// production, a constant for deterministic tests).
    #[must_use]
    const fn new(seed: u64) -> Self {
        Self {
            attempt: 0,
            // xorshift needs a nonzero state.
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// Next delay: uniform in `[0, min(60 s, 1 s × 2^attempt))`.
    fn next_delay(&mut self) -> Duration {
        let exp = self.attempt.min(6);
        let cap_ms = (1_000u64 << exp).min(60_000);
        self.attempt = self.attempt.saturating_add(1);
        // xorshift64
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        Duration::from_millis(x % cap_ms)
    }

    /// Return to the first backoff window (call after a stable link).
    const fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Connection considered stable (and backoff reset) after this long.
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// Protocol-erased session events the supervisor consumes.
enum LinkEvent {
    /// Session reported a disconnect (reason stringified).
    Disconnected {
        /// Debug-formatted disconnect reason.
        reason: DisconnectReason,
    },
    /// A new voice stream opened.
    VoiceStart {
        /// Stream id.
        stream_id: StreamId,
        /// Decoded header.
        header: DstarHeader,
        /// Stringified lenient-parse diagnostics.
        diagnostics: Vec<String>,
    },
    /// One voice frame.
    VoiceFrame {
        /// Stream id.
        stream_id: StreamId,
        /// Raw received sequence byte.
        seq: u8,
        /// The frame payload.
        frame: VoiceFrame,
    },
    /// A voice stream ended.
    VoiceEnd {
        /// Stream id.
        stream_id: StreamId,
        /// Mapped end reason.
        end_reason: EndReason,
    },
    /// Anything the recorder does not act on (connect, poll echo).
    Other,
}

fn erase(ev: AnyEvent) -> LinkEvent {
    match ev {
        AnyEvent::Disconnected { reason } => LinkEvent::Disconnected { reason },
        AnyEvent::VoiceStart {
            stream_id,
            header,
            diagnostics,
        } => LinkEvent::VoiceStart {
            stream_id,
            header: *header,
            diagnostics: diagnostics.iter().map(|d| format!("{d:?}")).collect(),
        },
        AnyEvent::VoiceFrame {
            stream_id,
            seq,
            frame,
        } => LinkEvent::VoiceFrame {
            stream_id,
            seq,
            frame,
        },
        AnyEvent::VoiceEnd { stream_id, reason } => LinkEvent::VoiceEnd {
            stream_id,
            end_reason: match reason {
                VoiceEndReason::Eot => EndReason::Eot,
                // `VoiceEndReason` is non_exhaustive; anything else
                // is inactivity-shaped.
                _ => EndReason::Inactivity,
            },
        },
        // Connected / PollEcho / future variants: nothing to record.
        _ => LinkEvent::Other,
    }
}

/// A connected session of any protocol.
type RuntimeSession = AnyAsyncSession;

/// Typed failure while resolving, authenticating or linking one target.
#[derive(Debug, thiserror::Error)]
pub enum SessionConnectError {
    /// DNS resolution failed.
    #[error("resolve target: {0}")]
    Resolve(#[source] std::io::Error),
    /// Resolution returned no socket address.
    #[error("target resolved to no addresses")]
    NoAddresses,
    /// The matching IPv4/IPv6 local UDP socket could not be bound.
    #[error("bind UDP: {0}")]
    Bind(#[source] std::io::Error),
    /// Core configuration/transition failed.
    #[error("session transition: {0}")]
    Core(#[source] dstar_gateway_core::error::Error),
    /// The complete DNS/authentication/handshake budget expired.
    #[error("session connection exceeded its 30-second total deadline")]
    Deadline,
    /// UDP linking failed within its handshake budget.
    #[error("session handshake: {0}")]
    Handshake(#[source] dstar_gateway::tokio_shell::ConnectError),
    /// The target's protocol has no recorder implementation.
    #[error("unsupported reflector protocol")]
    UnsupportedProtocol,
    /// DCS routing identity validation failed.
    #[error("DCS reflector: {0}")]
    Identity(#[source] dstar_gateway_core::TypeError),
}

async fn resolve(target: &Target) -> Result<std::net::SocketAddr, SessionConnectError> {
    if let Some(ip) = target.host.ip() {
        return Ok(std::net::SocketAddr::new(ip, target.port.get()));
    }
    let host = target
        .host
        .dns_name()
        .ok_or(SessionConnectError::NoAddresses)?;
    tokio::net::lookup_host((host, target.port.get()))
        .await
        .map_err(SessionConnectError::Resolve)?
        .next()
        .ok_or(SessionConnectError::NoAddresses)
}

async fn bind_socket(peer: std::net::SocketAddr) -> Result<Arc<UdpSocket>, SessionConnectError> {
    let local = if peer.is_ipv4() {
        std::net::SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        std::net::SocketAddr::from(([0_u16; 8], 0))
    };
    let sock = UdpSocket::bind(local)
        .await
        .map_err(SessionConnectError::Bind)?;
    Ok(Arc::new(sock))
}

/// Overall deadline for the UDP connect handshake (the shared pump in
/// `dstar-gateway` polls inside this window).
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
const CONNECT_DEADLINE: Duration = Duration::from_secs(30);

struct ConnectedSession {
    session: RuntimeSession,
    peer: std::net::SocketAddr,
    authentication_error: Option<dstar_gateway::auth::AuthError>,
}

enum ConnectionAttempt {
    Stopped,
    Finished(Result<ConnectedSession, SessionConnectError>),
}

async fn connection_attempt(
    connection: impl Future<Output = Result<ConnectedSession, SessionConnectError>>,
    shutdown: &mut watch::Receiver<bool>,
    deadline: Duration,
) -> ConnectionAttempt {
    if *shutdown.borrow() {
        return ConnectionAttempt::Stopped;
    }
    let connection = tokio::time::timeout(deadline, connection);
    tokio::pin!(connection);
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return ConnectionAttempt::Stopped; }
            }
            result = &mut connection => return ConnectionAttempt::Finished(result.unwrap_or(Err(SessionConnectError::Deadline))),
        }
    }
}

async fn connect(
    target: &Target,
    callsign: RoutingCallsign,
    local_module: Module,
) -> Result<ConnectedSession, SessionConnectError> {
    let peer = resolve(target).await?;
    let sock = bind_socket(peer).await?;
    let mut authentication_error = None;
    let session = match target.protocol {
        ProtocolKind::DPlus => {
            // Best-effort DPlus auth: the auth server also yields the
            // authoritative REF host list; without it most reflectors
            // still accept the link.
            let host_list = match dstar_gateway::auth::AuthClient::new()
                .authenticate(callsign.callsign())
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(error = ?e, "DPlus auth failed, continuing unauthenticated");
                    authentication_error = Some(e);
                    HostList::new()
                }
            };
            let configured = Session::<DPlus, Configured>::builder()
                .callsign(callsign)
                .local_module(local_module)
                .reflector_module(target.module)
                .peer(peer)
                .build();
            let authed = configured
                .authenticate(host_list)
                .map_err(|f| SessionConnectError::Core(f.error))?;
            let connecting = authed
                .connect(Instant::now())
                .map_err(|f| SessionConnectError::Core(f.error))?;
            let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
                .await
                .map_err(SessionConnectError::Handshake)?;
            RuntimeSession::DPlus(AsyncSession::spawn(connected, sock))
        }
        ProtocolKind::DExtra => {
            let configured = Session::<DExtra, Configured>::builder()
                .callsign(callsign)
                .local_module(local_module)
                .reflector_module(target.module)
                .peer(peer)
                .build();
            let connecting = configured
                .connect(Instant::now())
                .map_err(|f| SessionConnectError::Core(f.error))?;
            let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
                .await
                .map_err(SessionConnectError::Handshake)?;
            RuntimeSession::DExtra(AsyncSession::spawn(connected, sock))
        }
        ProtocolKind::Dcs => {
            let configured = Session::<Dcs, Configured>::builder()
                .callsign(callsign)
                .local_module(local_module)
                .reflector_module(target.module)
                .dcs_reflector(
                    DcsReflector::try_from(target.reflector_callsign)
                        .map_err(SessionConnectError::Identity)?,
                )
                .peer(peer)
                .build();
            let connecting = configured
                .connect(Instant::now())
                .map_err(|f| SessionConnectError::Core(f.error))?;
            let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
                .await
                .map_err(SessionConnectError::Handshake)?;
            RuntimeSession::Dcs(AsyncSession::spawn(connected, sock))
        }
        _ => return Err(SessionConnectError::UnsupportedProtocol),
    };
    Ok(ConnectedSession {
        session,
        peer,
        authentication_error,
    })
}

/// Bounded completion statistics retained until a supervisor is joined.
///
/// Counters saturate at `u64::MAX`; only the most recent failure is retained.
#[derive(Debug, Default)]
pub struct SupervisorSummary {
    recordings_written: u64,
    failed_writes: u64,
    failed_connections: u64,
    failed_sessions: u64,
    failed_authentications: u64,
    last_failure: Option<SupervisorFailure>,
}

impl SupervisorSummary {
    /// Recordings whose complete metadata publication succeeded.
    #[must_use]
    pub const fn recordings_written(&self) -> u64 {
        self.recordings_written
    }
    /// Failed recording writes or writer tasks.
    #[must_use]
    pub const fn failed_writes(&self) -> u64 {
        self.failed_writes
    }
    /// Failed or expired connection attempts.
    #[must_use]
    pub const fn failed_connections(&self) -> u64 {
        self.failed_connections
    }
    /// Session, capacity or disconnect/join failures.
    #[must_use]
    pub const fn failed_sessions(&self) -> u64 {
        self.failed_sessions
    }
    /// Best-effort `DPlus` authentication failures followed by unauthenticated linking.
    #[must_use]
    pub const fn failed_authentications(&self) -> u64 {
        self.failed_authentications
    }
    /// Most recent typed failure, if one occurred.
    #[must_use]
    pub const fn last_failure(&self) -> Option<&SupervisorFailure> {
        self.last_failure.as_ref()
    }

    fn record_failure(&mut self, failure: SupervisorFailure) {
        match &failure {
            SupervisorFailure::Connect(_) => {
                self.failed_connections = self.failed_connections.saturating_add(1);
            }
            SupervisorFailure::Authentication(_) => {
                self.failed_authentications = self.failed_authentications.saturating_add(1);
            }
            SupervisorFailure::Write(_) | SupervisorFailure::WriterTask(_) => {
                self.failed_writes = self.failed_writes.saturating_add(1);
            }
            SupervisorFailure::Session(_) | SupervisorFailure::Capacity(_) => {
                self.failed_sessions = self.failed_sessions.saturating_add(1);
            }
        }
        tracing::warn!(error = %failure, "recording supervisor failure");
        self.last_failure = Some(failure);
    }
}

/// Typed failure retained by a joined recording supervisor.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorFailure {
    /// A bounded connect attempt failed.
    #[error(transparent)]
    Connect(#[from] SessionConnectError),
    /// `DPlus` authentication failed before best-effort unauthenticated linking.
    #[error("DPlus authentication: {0}")]
    Authentication(#[source] dstar_gateway::auth::AuthError),
    /// A recording failed to publish.
    #[error(transparent)]
    Write(#[from] crate::writer::WriteError),
    /// A blocking writer task panicked or was cancelled.
    #[error("writer task: {0}")]
    WriterTask(#[source] tokio::task::JoinError),
    /// The connected shell failed or did not acknowledge unlink.
    #[error("reflector session: {0}")]
    Session(#[source] ShellError),
    /// Untracked streams exceeded the configured simultaneous-capture bound.
    #[error(transparent)]
    Capacity(#[from] crate::capture::CaptureCapacityError),
}

async fn write_recording(
    writer: &Arc<Writer>,
    rec: CompletedRecording,
    summary: &mut SupervisorSummary,
) {
    let writer = Arc::clone(writer);
    match tokio::task::spawn_blocking(move || writer.write(&rec)).await {
        Ok(Ok(path)) => {
            summary.recordings_written = summary.recordings_written.saturating_add(1);
            tracing::info!(path = %path.display(), "recording finalized");
        }
        Ok(Err(error)) => summary.record_failure(SupervisorFailure::Write(error)),
        Err(error) => summary.record_failure(SupervisorFailure::WriterTask(error)),
    }
}

/// Why [`pump_session`] returned.
enum SessionOutcome {
    /// Session ended (disconnect or loop death): reconnect.
    Dropped,
    /// Shutdown was requested: supervisor should return.
    ShutdownRequested,
    /// Capture capacity was exceeded; unlink before reconnecting.
    CapacityReached,
}

/// Consume events, finalize retained captures, then join the shell task.
async fn pump_session(
    mut session: RuntimeSession,
    mut mgr: CaptureManager,
    writer: Arc<Writer>,
    mut shutdown: watch::Receiver<bool>,
    label: &str,
    summary: &mut SupervisorSummary,
) -> SessionOutcome {
    let session = &mut session;
    let mgr = &mut mgr;
    let writer = &writer;
    let shutdown = &mut shutdown;
    let outcome = loop {
        if *shutdown.borrow() {
            break SessionOutcome::ShutdownRequested;
        }
        tokio::select! {
            ev = session.next_event() => match ev.map(erase) {
                None => {
                    tracing::warn!(target = %label, "session loop exited");
                    break SessionOutcome::Dropped;
                }
                Some(LinkEvent::Disconnected { reason }) => {
                    tracing::warn!(target = %label, reason = ?reason, "disconnected");
                    break SessionOutcome::Dropped;
                }
                Some(LinkEvent::VoiceStart { stream_id, header, diagnostics }) => {
                    tracing::debug!(target = %label, sid = stream_id.get(), "voice start");
                    if let Err(error) = mgr.on_voice_start(stream_id, header, diagnostics, Utc::now()) {
                        tracing::error!(
                            target = %label,
                            error = %error,
                            "concurrent capture limit reached; disconnecting to reject the untracked stream"
                        );
                        summary.record_failure(SupervisorFailure::Capacity(error));
                        break SessionOutcome::CapacityReached;
                    }
                }
                Some(LinkEvent::VoiceFrame { stream_id, seq, frame }) => {
                    match mgr.on_voice_frame(stream_id, seq, &frame, Utc::now()) {
                        Ok(Some(rec)) => {
                            tracing::error!(
                                target = %label,
                                sid = stream_id.get(),
                                retained_frames = rec.frames.len(),
                                "capture limit reached; writing the retained prefix and discarding this stream until its end event"
                            );
                            write_recording(writer, rec, summary).await;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            tracing::error!(
                                target = %label,
                                error = %error,
                                "concurrent capture limit reached; disconnecting to reject the untracked stream"
                            );
                            summary.record_failure(SupervisorFailure::Capacity(error));
                            break SessionOutcome::CapacityReached;
                        }
                    }
                }
                Some(LinkEvent::VoiceEnd { stream_id, end_reason }) => {
                    if let Some(rec) = mgr.on_voice_end(stream_id, end_reason, Utc::now()) {
                        write_recording(writer, rec, summary).await;
                    }
                }
                Some(LinkEvent::Other) => {}
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break SessionOutcome::ShutdownRequested; }
            }
        }
    };
    let end_reason = if matches!(outcome, SessionOutcome::ShutdownRequested) {
        EndReason::Shutdown
    } else {
        EndReason::Disconnect
    };
    for rec in mgr.finalize_all(end_reason, Utc::now()) {
        write_recording(writer, rec, summary).await;
    }
    if !matches!(outcome, SessionOutcome::Dropped)
        && let Err(error) = session.disconnect().await
    {
        summary.record_failure(SupervisorFailure::Session(error));
    }
    if let Err(error) = session.join().await {
        summary.record_failure(SupervisorFailure::Session(error));
    }
    outcome
}

/// Supervise a target until a stop request or closed control channel.
///
/// Connect includes DNS/authentication/UDP linking in one 30-second budget and
/// races stop requests. Finalization awaits every writer and the connected
/// shell task before returning bounded completion statistics.
pub(crate) async fn run_supervisor(
    target: Target,
    callsign: RoutingCallsign,
    local_module: Module,
    capture_limit: CaptureDurationLimit,
    concurrent_capture_limit: ConcurrentCaptureLimit,
    writer: Arc<Writer>,
    mut shutdown: watch::Receiver<bool>,
) -> SupervisorSummary {
    let mut summary = SupervisorSummary::default();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| u64::from(d.subsec_nanos()) | 1);
    let mut backoff = Backoff::new(seed ^ u64::from(target.port.get()));
    let label = target.key().to_string();

    loop {
        if *shutdown.borrow_and_update() {
            return summary;
        }

        let connection = match connection_attempt(
            connect(&target, callsign, local_module),
            &mut shutdown,
            CONNECT_DEADLINE,
        )
        .await
        {
            ConnectionAttempt::Stopped => return summary,
            ConnectionAttempt::Finished(result) => result,
        };
        match connection {
            Err(e) => {
                summary.record_failure(SupervisorFailure::Connect(e));
            }
            Ok(ConnectedSession {
                session,
                peer,
                authentication_error,
            }) => {
                if let Some(error) = authentication_error {
                    summary.record_failure(SupervisorFailure::Authentication(error));
                }
                tracing::info!(target = %label, peer = %peer, "connected");
                let connected_at = Instant::now();
                let origin = StreamOrigin::new(target.clone(), peer);
                let mgr = CaptureManager::new(origin, capture_limit, concurrent_capture_limit);

                let outcome = pump_session(
                    session,
                    mgr,
                    Arc::clone(&writer),
                    shutdown.clone(),
                    &label,
                    &mut summary,
                )
                .await;
                if matches!(outcome, SessionOutcome::ShutdownRequested) {
                    return summary;
                }
                if *shutdown.borrow_and_update() {
                    return summary;
                }

                if connected_at.elapsed() >= STABLE_AFTER {
                    backoff.reset();
                }
            }
        }

        let delay = backoff.next_delay();
        tracing::debug!(
            target = %label,
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "reconnect backoff"
        );
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return summary; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_respect_exponential_caps_with_jitter() {
        let mut b = Backoff::new(42);
        // attempt 0 → cap 1 s, 1 → 2 s, 2 → 4 s … 6+ → 60 s
        let caps_ms = [
            1_000u64, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000,
        ];
        for cap in caps_ms {
            let d = b.next_delay();
            assert!(
                d.as_millis() < u128::from(cap),
                "delay {d:?} under cap {cap}"
            );
        }
    }

    #[test]
    fn reset_returns_to_first_window() {
        let mut b = Backoff::new(7);
        for _ in 0..5 {
            let _ = b.next_delay();
        }
        b.reset();
        assert!(b.next_delay().as_millis() < 1_000);
    }

    #[test]
    fn seeded_backoff_is_deterministic() {
        let mut a = Backoff::new(99);
        let mut b = Backoff::new(99);
        for _ in 0..8 {
            assert_eq!(a.next_delay(), b.next_delay());
        }
    }

    #[tokio::test]
    async fn total_connect_deadline_and_closed_control_cancel_pending_work()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, mut receiver) = watch::channel(false);
        let pending = std::future::pending::<Result<ConnectedSession, SessionConnectError>>();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            connection_attempt(pending, &mut receiver, Duration::from_millis(1)),
        )
        .await?;
        assert!(matches!(
            result,
            ConnectionAttempt::Finished(Err(SessionConnectError::Deadline))
        ));
        drop(sender);
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            connection_attempt(
                std::future::pending(),
                &mut receiver,
                Duration::from_secs(30),
            ),
        )
        .await?;
        assert!(matches!(result, ConnectionAttempt::Stopped));
        let (sender, mut receiver) = watch::channel(false);
        sender.send(true)?;
        let result = connection_attempt(
            std::future::pending(),
            &mut receiver,
            Duration::from_secs(30),
        )
        .await;
        assert!(matches!(result, ConnectionAttempt::Stopped));
        Ok(())
    }
}
