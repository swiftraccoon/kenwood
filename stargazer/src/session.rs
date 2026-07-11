// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Per-target session supervision: connect (with protocol-specific
//! ladders), pump events into the capture core, write completed
//! recordings, and heal drops with jittered exponential backoff.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, Connecting, DExtra, DPlus, Dcs, Event, Protocol, Session,
    VoiceEndReason,
};
use dstar_gateway_core::{Callsign, DStarHeader, Module, StreamId, VoiceFrame};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::timeout;

use crate::capture::{CaptureManager, CompletedRecording, EndReason, StreamOrigin};
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
        header: DStarHeader,
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

fn erase<P: Protocol>(ev: Event<P>) -> LinkEvent {
    match ev {
        Event::Disconnected { reason } => LinkEvent::Disconnected {
            reason: format!("{reason:?}"),
        },
        Event::VoiceStart {
            stream_id,
            header,
            diagnostics,
        } => LinkEvent::VoiceStart {
            stream_id,
            header,
            diagnostics: diagnostics.iter().map(|d| format!("{d:?}")).collect(),
        },
        Event::VoiceFrame {
            stream_id,
            seq,
            frame,
        } => LinkEvent::VoiceFrame {
            stream_id,
            seq,
            frame,
        },
        Event::VoiceEnd { stream_id, reason } => LinkEvent::VoiceEnd {
            stream_id,
            end_reason: match reason {
                VoiceEndReason::Eot => EndReason::Eot,
                // `VoiceEndReason` is non_exhaustive; anything else
                // is inactivity-shaped.
                _ => EndReason::Inactivity,
            },
        },
        // Connected / PollEcho / future variants — nothing to record.
        _ => LinkEvent::Other,
    }
}

/// A connected session of any protocol.
enum RuntimeSession {
    /// `DPlus` session.
    DPlus(AsyncSession<DPlus>),
    /// `DExtra` session.
    DExtra(AsyncSession<DExtra>),
    /// DCS session.
    Dcs(AsyncSession<Dcs>),
}

impl RuntimeSession {
    async fn next_event(&mut self) -> Option<LinkEvent> {
        match self {
            Self::DPlus(s) => s.next_event().await.map(erase),
            Self::DExtra(s) => s.next_event().await.map(erase),
            Self::Dcs(s) => s.next_event().await.map(erase),
        }
    }

    async fn disconnect(&mut self) {
        let result = match self {
            Self::DPlus(s) => s.disconnect().await,
            Self::DExtra(s) => s.disconnect().await,
            Self::Dcs(s) => s.disconnect().await,
        };
        if let Err(e) = result {
            tracing::debug!(error = %e, "disconnect");
        }
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

/// Drive the protocol-generic UDP handshake until Connected.
async fn run_handshake<P>(
    connecting: &mut Session<P, Connecting>,
    sock: &UdpSocket,
) -> Result<(), String>
where
    P: Protocol,
{
    for _ in 0..4_u8 {
        if let Some(tx) = connecting.poll_transmit(Instant::now()) {
            let _bytes = sock
                .send_to(tx.payload, tx.dst)
                .await
                .map_err(|e| format!("send handshake: {e}"))?;
        }
        let mut buf = [0u8; 128];
        match timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await {
            Ok(Ok((n, src))) => {
                let slice = buf.get(..n).unwrap_or(&[]);
                connecting
                    .handle_input(Instant::now(), src, slice)
                    .map_err(|e| format!("handshake input: {e}"))?;
                match connecting.state_kind() {
                    ClientStateKind::Connected => return Ok(()),
                    ClientStateKind::Closed => {
                        return Err("reflector refused the link".to_string());
                    }
                    _ => {}
                }
            }
            Ok(Err(e)) => return Err(format!("recv handshake: {e}")),
            Err(_) => return Err("handshake timeout".to_string()),
        }
    }
    if connecting.state_kind() == ClientStateKind::Connected {
        Ok(())
    } else {
        Err("handshake did not reach Connected".to_string())
    }
}

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
                    tracing::warn!(error = ?e, "DPlus auth failed — continuing unauthenticated");
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
            let mut connecting = authed
                .connect(Instant::now())
                .map_err(|f| format!("connect: {}", f.error))?;
            run_handshake(&mut connecting, &sock).await?;
            let connected = connecting
                .promote()
                .map_err(|f| format!("promote: {}", f.error))?;
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
            let mut connecting = configured
                .connect(Instant::now())
                .map_err(|f| format!("connect: {}", f.error))?;
            run_handshake(&mut connecting, &sock).await?;
            let connected = connecting
                .promote()
                .map_err(|f| format!("promote: {}", f.error))?;
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
            let mut connecting = configured
                .connect(Instant::now())
                .map_err(|f| format!("connect: {}", f.error))?;
            run_handshake(&mut connecting, &sock).await?;
            let connected = connecting
                .promote()
                .map_err(|f| format!("promote: {}", f.error))?;
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
    /// Session ended (disconnect or loop death) — reconnect.
    Dropped,
    /// Shutdown was requested — supervisor should return.
    ShutdownRequested,
}

/// Consume one connected session's events until it drops or
/// shutdown is requested; open captures are always finalized.
async fn pump_session(
    session: &mut RuntimeSession,
    mgr: &mut CaptureManager,
    writer: &Arc<Writer>,
    shutdown: &mut watch::Receiver<bool>,
    label: &str,
) -> SessionOutcome {
    loop {
        tokio::select! {
            ev = session.next_event() => match ev {
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
                    mgr.on_voice_start(stream_id, header, diagnostics, Utc::now());
                }
                Some(LinkEvent::VoiceFrame { stream_id, seq, frame }) => {
                    mgr.on_voice_frame(stream_id, seq, &frame, Utc::now());
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
                    session.disconnect().await;
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
            Ok((mut session, peer)) => {
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
                let mut mgr = CaptureManager::new(origin);

                match pump_session(&mut session, &mut mgr, &writer, &mut shutdown, &label).await {
                    SessionOutcome::ShutdownRequested => return,
                    SessionOutcome::Dropped => {}
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
