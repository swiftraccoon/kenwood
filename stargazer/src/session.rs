// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Per-target session supervision: connect (with protocol-specific
//! ladders), pump events into the capture core, write completed
//! recordings, and heal drops with jittered exponential backoff.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dstar_gateway::tokio_shell::{AnyAsyncSession, AnyEvent, AsyncSession, drive_connecting};
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::session::client::{
    Configured, DExtra, DPlus, Dcs, Session, VoiceEndReason,
};
use dstar_gateway_core::{Callsign, DstarHeader, Module, StreamId, VoiceFrame};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::capture::{
    CaptureDurationLimit, CaptureManager, CompletedRecording, ConcurrentCaptureLimit, EndReason,
    StreamOrigin,
};
use crate::config::{ProtocolChoice, Target};
use crate::writer::Writer;

/// Jittered exponential backoff: full jitter over a doubling window,
/// 1 s initial, 60 s cap.
#[derive(Debug)]
pub struct Backoff {
    attempt: u32,
    state: u64,
}

impl Backoff {
    /// Create a backoff with a jitter seed (pass clock nanos for
    /// production, a constant for deterministic tests).
    #[must_use]
    pub const fn new(seed: u64) -> Self {
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
    pub fn next_delay(&mut self) -> Duration {
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
    pub const fn reset(&mut self) {
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
        reason: String,
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
        /// Frame seq (0..=20).
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
        AnyEvent::Disconnected { reason } => LinkEvent::Disconnected {
            reason: format!("{reason:?}"),
        },
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

/// Disconnect, downgrading failures to a debug log; recorder teardown
/// must not abort on an already-dead link.
async fn disconnect_quietly(session: &mut RuntimeSession) {
    if let Err(e) = session.disconnect().await {
        tracing::debug!(error = %e, "disconnect");
    }
}

async fn resolve(host: &str, port: u16) -> Result<std::net::SocketAddr, String> {
    tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("resolve {host}:{port}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {host}:{port}: no addresses"))
}

async fn bind_socket() -> Result<Arc<UdpSocket>, String> {
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| format!("bind UDP: {e}"))?;
    Ok(Arc::new(sock))
}

/// Overall deadline for the UDP connect handshake (the shared pump in
/// `dstar-gateway` polls inside this window).
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

async fn connect(
    target: &Target,
    callsign: Callsign,
    local_module: Module,
) -> Result<(RuntimeSession, std::net::SocketAddr), String> {
    let peer = resolve(&target.host, target.port).await?;
    let sock = bind_socket().await?;
    let session = match target.protocol {
        ProtocolChoice::Dplus => {
            // Best-effort DPlus auth: the auth server also yields the
            // authoritative REF host list; without it most reflectors
            // still accept the link.
            let host_list = match dstar_gateway::auth::AuthClient::new()
                .authenticate(callsign)
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(error = ?e, "DPlus auth failed, continuing unauthenticated");
                    HostList::new()
                }
            };
            let configured = Session::<DPlus, Configured>::builder()
                .callsign(callsign)
                .local_module(local_module)
                .reflector_module(target.module)
                .reflector_callsign(target.reflector_callsign)
                .peer(peer)
                .build();
            let authed = configured
                .authenticate(host_list)
                .map_err(|f| format!("authenticate: {}", f.error))?;
            let connecting = authed
                .connect(Instant::now())
                .map_err(|f| format!("connect: {}", f.error))?;
            let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
                .await
                .map_err(|e| format!("handshake: {e}"))?;
            RuntimeSession::DPlus(AsyncSession::spawn(connected, sock))
        }
        ProtocolChoice::Dextra => {
            let configured = Session::<DExtra, Configured>::builder()
                .callsign(callsign)
                .local_module(local_module)
                .reflector_module(target.module)
                .reflector_callsign(target.reflector_callsign)
                .peer(peer)
                .build();
            let connecting = configured
                .connect(Instant::now())
                .map_err(|f| format!("connect: {}", f.error))?;
            let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
                .await
                .map_err(|e| format!("handshake: {e}"))?;
            RuntimeSession::DExtra(AsyncSession::spawn(connected, sock))
        }
        ProtocolChoice::Dcs => {
            let configured = Session::<Dcs, Configured>::builder()
                .callsign(callsign)
                .local_module(local_module)
                .reflector_module(target.module)
                .reflector_callsign(target.reflector_callsign)
                .peer(peer)
                .build();
            let connecting = configured
                .connect(Instant::now())
                .map_err(|f| format!("connect: {}", f.error))?;
            let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
                .await
                .map_err(|e| format!("handshake: {e}"))?;
            RuntimeSession::Dcs(AsyncSession::spawn(connected, sock))
        }
    };
    Ok((session, peer))
}

async fn write_recording(writer: &Arc<Writer>, rec: CompletedRecording) {
    let callsign = rec.header.as_ref().map_or_else(
        || "UNKNOWN".to_string(),
        |h| h.my_call.as_str().trim_end().to_string(),
    );
    let duration = rec.duration_s();
    let gaps = rec.gaps;
    let w = Arc::clone(writer);
    match tokio::task::spawn_blocking(move || w.write(&rec)).await {
        Ok(Ok(path)) => tracing::info!(
            path = %path.display(),
            callsign,
            duration_s = duration,
            gaps,
            "recording finalized"
        ),
        Ok(Err(e)) => tracing::error!(error = %e, callsign, "recording write failed"),
        Err(e) => tracing::error!(error = %e, "recording write task panicked"),
    }
}

/// Why [`pump_session`] returned.
enum SessionOutcome {
    /// Session ended (disconnect or loop death): reconnect.
    Dropped,
    /// Shutdown was requested: supervisor should return.
    ShutdownRequested,
}

/// Consume one connected session's events until it drops or
/// shutdown is requested; open captures are always finalized.
///
/// Owns its state and runs as its own task so that a panic (e.g. a
/// decode-path bug tripped by hostile wire input) is contained by
/// the task boundary: the supervisor observes the `JoinError` and
/// reconnects instead of silently losing the target forever.
async fn pump_session(
    mut session: RuntimeSession,
    mut mgr: CaptureManager,
    writer: Arc<Writer>,
    mut shutdown: watch::Receiver<bool>,
    label: String,
) -> SessionOutcome {
    let session = &mut session;
    let mgr = &mut mgr;
    let writer = &writer;
    let shutdown = &mut shutdown;
    let label: &str = &label;
    loop {
        tokio::select! {
            ev = session.next_event() => match ev.map(erase) {
                None => {
                    tracing::warn!(target = %label, "session loop exited");
                    for rec in mgr.finalize_all(EndReason::Disconnect, Utc::now()) {
                        write_recording(writer, rec).await;
                    }
                    return SessionOutcome::Dropped;
                }
                Some(LinkEvent::Disconnected { reason }) => {
                    tracing::warn!(target = %label, reason, "disconnected");
                    for rec in mgr.finalize_all(EndReason::Disconnect, Utc::now()) {
                        write_recording(writer, rec).await;
                    }
                    return SessionOutcome::Dropped;
                }
                Some(LinkEvent::VoiceStart { stream_id, header, diagnostics }) => {
                    tracing::debug!(target = %label, sid = stream_id.get(), "voice start");
                    if let Err(error) = mgr.on_voice_start(stream_id, header, diagnostics, Utc::now()) {
                        tracing::error!(
                            target = %label,
                            error = %error,
                            "concurrent capture limit reached; disconnecting to reject the untracked stream"
                        );
                        for rec in mgr.finalize_all(EndReason::Disconnect, Utc::now()) {
                            write_recording(writer, rec).await;
                        }
                        disconnect_quietly(session).await;
                        return SessionOutcome::Dropped;
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
                            write_recording(writer, rec).await;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            tracing::error!(
                                target = %label,
                                error = %error,
                                "concurrent capture limit reached; disconnecting to reject the untracked stream"
                            );
                            for rec in mgr.finalize_all(EndReason::Disconnect, Utc::now()) {
                                write_recording(writer, rec).await;
                            }
                            disconnect_quietly(session).await;
                            return SessionOutcome::Dropped;
                        }
                    }
                }
                Some(LinkEvent::VoiceEnd { stream_id, end_reason }) => {
                    if let Some(rec) = mgr.on_voice_end(stream_id, end_reason, Utc::now()) {
                        write_recording(writer, rec).await;
                    }
                }
                Some(LinkEvent::Other) => {}
            },
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!(target = %label, "shutting down");
                    for rec in mgr.finalize_all(EndReason::Shutdown, Utc::now()) {
                        write_recording(writer, rec).await;
                    }
                    disconnect_quietly(session).await;
                    return SessionOutcome::ShutdownRequested;
                }
            }
        }
    }
}

/// Supervise one record target forever: connect, record, and heal
/// drops with backoff until `shutdown` flips true.
pub async fn run_supervisor(
    target: Target,
    callsign: Callsign,
    local_module: Module,
    capture_limit: CaptureDurationLimit,
    concurrent_capture_limit: ConcurrentCaptureLimit,
    writer: Arc<Writer>,
    mut shutdown: watch::Receiver<bool>,
) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| u64::from(d.subsec_nanos()) | 1);
    let mut backoff = Backoff::new(seed ^ u64::from(target.port));
    let label = format!("{}-{}", target.reflector, target.module.as_char());

    loop {
        if *shutdown.borrow_and_update() {
            return;
        }

        match connect(&target, callsign, local_module).await {
            Err(e) => {
                tracing::warn!(target = %label, error = %e, "connect failed");
            }
            Ok((session, peer)) => {
                tracing::info!(target = %label, peer = %peer, "connected");
                let connected_at = Instant::now();
                let origin = StreamOrigin {
                    reflector: target.reflector.clone(),
                    module: target.module,
                    protocol: target.protocol.name(),
                    host: target.host.clone(),
                    port: target.port,
                    peer,
                };
                let mgr = CaptureManager::new(origin, capture_limit, concurrent_capture_limit);

                let pump = tokio::spawn(pump_session(
                    session,
                    mgr,
                    Arc::clone(&writer),
                    shutdown.clone(),
                    label.clone(),
                ));
                match pump.await {
                    Ok(SessionOutcome::ShutdownRequested) => return,
                    Ok(SessionOutcome::Dropped) => {}
                    Err(e) => {
                        // A panic inside the session task must not end
                        // this target's supervision: log it loudly,
                        // accept the open captures as lost, reconnect.
                        tracing::error!(
                            target = %label,
                            panicked = e.is_panic(),
                            error = %e,
                            "session task died, reconnecting"
                        );
                    }
                }
                if *shutdown.borrow_and_update() {
                    return;
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
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
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
}
