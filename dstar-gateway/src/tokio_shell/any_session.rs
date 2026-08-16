//! Protocol-erased wrapper over [`AsyncSession`].
//!
//! `Event<P>` is phantom-generic: every variant carries the same data
//! for all three protocols, with `P` confined to an uninhabitable
//! hidden variant. Consumers that hold "whichever protocol the
//! operator picked" (dashboards, recorders, relays) therefore each
//! hand-rolled the same three-armed enum plus an event mirror.
//! [`AnyAsyncSession`] erases the parameter once, and [`AnyEvent`] is
//! the erased event type, carrying everything `Event<P>` carries.

use std::net::SocketAddr;
use std::time::Instant;

use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::session::client::{
    DExtra, DPlus, Dcs, DisconnectReason, Event, Protocol, VoiceEndReason,
};
use dstar_gateway_core::types::{ProtocolKind, StreamId};
use dstar_gateway_core::validator::Diagnostic;
use dstar_gateway_core::voice::VoiceFrame;
use tokio::sync::watch;

use super::{AsyncSession, ShellError};

/// One session over whichever protocol the operator picked.
///
/// Every method delegates to the wrapped [`AsyncSession`]; events come
/// back as the protocol-erased [`AnyEvent`]. Construct via the `From`
/// impls: `AnyAsyncSession::from(AsyncSession::spawn(connected, socket))`.
#[derive(Debug)]
pub enum AnyAsyncSession {
    /// A `DPlus` (REF) session.
    DPlus(AsyncSession<DPlus>),
    /// A `DExtra` (XRF/XLX) session.
    DExtra(AsyncSession<DExtra>),
    /// A DCS session.
    Dcs(AsyncSession<Dcs>),
}

impl From<AsyncSession<DPlus>> for AnyAsyncSession {
    fn from(session: AsyncSession<DPlus>) -> Self {
        Self::DPlus(session)
    }
}

impl From<AsyncSession<DExtra>> for AnyAsyncSession {
    fn from(session: AsyncSession<DExtra>) -> Self {
        Self::DExtra(session)
    }
}

impl From<AsyncSession<Dcs>> for AnyAsyncSession {
    fn from(session: AsyncSession<Dcs>) -> Self {
        Self::Dcs(session)
    }
}

impl AnyAsyncSession {
    /// Which protocol family this session speaks.
    #[must_use]
    pub const fn protocol_kind(&self) -> ProtocolKind {
        match self {
            Self::DPlus(_) => ProtocolKind::DPlus,
            Self::DExtra(_) => ProtocolKind::DExtra,
            Self::Dcs(_) => ProtocolKind::Dcs,
        }
    }

    /// Receive the next session event, erased to [`AnyEvent`].
    ///
    /// Returns `None` when the session task has exited; see
    /// [`AsyncSession::next_event`].
    pub async fn next_event(&mut self) -> Option<AnyEvent> {
        match self {
            Self::DPlus(s) => s.next_event().await.map(AnyEvent::from),
            Self::DExtra(s) => s.next_event().await.map(AnyEvent::from),
            Self::Dcs(s) => s.next_event().await.map(AnyEvent::from),
        }
    }

    /// Watch receiver for the instant of the last datagram from the
    /// reflector; see [`AsyncSession::activity`].
    #[must_use]
    pub fn activity(&self) -> watch::Receiver<Instant> {
        match self {
            Self::DPlus(s) => s.activity(),
            Self::DExtra(s) => s.activity(),
            Self::Dcs(s) => s.activity(),
        }
    }

    /// Send a D-STAR voice-stream header; see [`AsyncSession::send_header`].
    ///
    /// # Errors
    ///
    /// Propagates the wrapped session's [`ShellError`].
    pub async fn send_header(
        &mut self,
        header: DstarHeader,
        stream_id: StreamId,
    ) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_header(header, stream_id).await,
            Self::DExtra(s) => s.send_header(header, stream_id).await,
            Self::Dcs(s) => s.send_header(header, stream_id).await,
        }
    }

    /// Send a voice data frame; see [`AsyncSession::send_voice`].
    ///
    /// # Errors
    ///
    /// Propagates the wrapped session's [`ShellError`].
    pub async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: VoiceFrame,
    ) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_voice(stream_id, seq, frame).await,
            Self::DExtra(s) => s.send_voice(stream_id, seq, frame).await,
            Self::Dcs(s) => s.send_voice(stream_id, seq, frame).await,
        }
    }

    /// Send a voice EOT and close the outbound stream; see
    /// [`AsyncSession::send_eot`].
    ///
    /// # Errors
    ///
    /// Propagates the wrapped session's [`ShellError`].
    pub async fn send_eot(&mut self, stream_id: StreamId, seq: u8) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_eot(stream_id, seq).await,
            Self::DExtra(s) => s.send_eot(stream_id, seq).await,
            Self::Dcs(s) => s.send_eot(stream_id, seq).await,
        }
    }

    /// Gracefully tear the link down; see [`AsyncSession::disconnect`].
    ///
    /// # Errors
    ///
    /// Propagates the wrapped session's [`ShellError`].
    pub async fn disconnect(&mut self) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.disconnect().await,
            Self::DExtra(s) => s.disconnect().await,
            Self::Dcs(s) => s.disconnect().await,
        }
    }
}

/// Protocol-erased mirror of [`Event<P>`], carrying identical payloads.
///
/// `Event<P>` is phantom-generic, so no information is lost in the
/// erasure. The header is boxed to keep the frequent [`AnyEvent::VoiceFrame`]
/// variant small.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum AnyEvent {
    /// Session has transitioned to `Connected`.
    Connected {
        /// Peer address of the reflector.
        peer: SocketAddr,
    },
    /// Session has transitioned to `Disconnected`.
    Disconnected {
        /// Why the session disconnected.
        reason: DisconnectReason,
    },
    /// Reflector keepalive echo received.
    PollEcho {
        /// Peer that sent the echo.
        peer: SocketAddr,
    },
    /// A new voice stream started.
    VoiceStart {
        /// Stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: Box<DstarHeader>,
        /// Diagnostics observed during header parsing.
        diagnostics: Vec<Diagnostic>,
    },
    /// A voice data frame within an active stream.
    VoiceFrame {
        /// Stream id.
        stream_id: StreamId,
        /// Frame seq.
        seq: u8,
        /// Voice frame.
        frame: VoiceFrame,
    },
    /// Voice stream ended (real EOT or synthesized after timeout).
    VoiceEnd {
        /// Stream id.
        stream_id: StreamId,
        /// Real EOT vs synthesized after inactivity.
        reason: VoiceEndReason,
    },
}

impl<P: Protocol> From<Event<P>> for AnyEvent {
    fn from(event: Event<P>) -> Self {
        match event {
            Event::Connected { peer } => Self::Connected { peer },
            Event::Disconnected { reason } => Self::Disconnected { reason },
            Event::PollEcho { peer } => Self::PollEcho { peer },
            Event::VoiceStart {
                stream_id,
                header,
                diagnostics,
            } => Self::VoiceStart {
                stream_id,
                header: Box::new(header),
                diagnostics,
            },
            Event::VoiceFrame {
                stream_id,
                seq,
                frame,
            } => Self::VoiceFrame {
                stream_id,
                seq,
                frame,
            },
            Event::VoiceEnd { stream_id, reason } => Self::VoiceEnd { stream_id, reason },
            // `Event<P>` is non_exhaustive with an uninhabitable
            // phantom variant; new real variants must be mirrored here.
            _ => unreachable!("Event<P> is exhaustively matched above"),
        }
    }
}

#[cfg(test)]
mod tests {
    use dstar_gateway_core::header::DstarHeader;
    use dstar_gateway_core::session::client::{DExtra, Event};
    use dstar_gateway_core::types::{Callsign, Module, StreamId, Suffix};
    use dstar_gateway_core::voice::VoiceFrame;

    use super::AnyEvent;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn header() -> DstarHeader {
        DstarHeader::for_relay(
            Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            Callsign::from_wire_bytes(*b"XRF001  "),
            Module::C,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Suffix::from_wire_bytes(*b"D75 "),
        )
    }

    #[test]
    fn any_event_erases_voice_start_preserving_payload() -> TestResult {
        let sid = StreamId::new(0x1234).ok_or("non-zero id")?;
        let event = Event::<DExtra>::VoiceStart {
            stream_id: sid,
            header: header(),
            diagnostics: Vec::new(),
        };

        let erased = AnyEvent::from(event);
        assert!(
            matches!(
                &erased,
                AnyEvent::VoiceStart { stream_id, header: h, diagnostics }
                    if *stream_id == sid
                        && h.my_call == Callsign::from_wire_bytes(*b"W1AW    ")
                        && diagnostics.is_empty()
            ),
            "erasure must preserve the VoiceStart payload: {erased:?}"
        );
        Ok(())
    }

    #[test]
    fn any_event_erases_voice_frame_preserving_payload() -> TestResult {
        let sid = StreamId::new(0x0042).ok_or("non-zero id")?;
        let frame = VoiceFrame {
            ambe: [7; 9],
            slow_data: [1, 2, 3],
        };
        let event = Event::<DExtra>::VoiceFrame {
            stream_id: sid,
            seq: 20,
            frame,
        };

        let erased = AnyEvent::from(event);
        assert!(
            matches!(
                &erased,
                AnyEvent::VoiceFrame { stream_id, seq: 20, frame: f }
                    if *stream_id == sid && f.ambe == [7; 9] && f.slow_data == [1, 2, 3]
            ),
            "erasure must preserve the VoiceFrame payload: {erased:?}"
        );
        Ok(())
    }
}
