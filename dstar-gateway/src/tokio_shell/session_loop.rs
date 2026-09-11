//! Tokio event loop driving a sans-io `Session<P, Connected>` over a real `UdpSocket`.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use dstar_gateway_core::error::{Error as CoreError, IoOperation};
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{Connected, Event, Protocol, Session};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use super::{Command, ShellError};

/// Internal loop that drives a sans-io session over a real tokio `UdpSocket`.
///
/// The loop:
/// 1. Drains `session.poll_transmit(now)` to the socket
/// 2. Delivers events in FIFO order; if the consumer channel is full,
///    continues outbound commands and their socket writes while UDP input waits
/// 3. Computes the next deadline via `session.poll_timeout()`
/// 4. Races inbound datagrams, command-channel messages, and timer
///    expiry via `tokio::select!`
/// 5. Repeats
///
/// Dropping the handle closes the `command_rx` channel, which causes
/// the loop to exit on its next iteration.
///
/// The loop is specialized to `Session<P, Connected>`: command
/// dispatch only makes sense on a session that can actually send
/// voice traffic. The shell's spawn path builds the session through
/// the typestate transitions on the main thread, then hands the
/// promoted `Session<P, Connected>` to the loop.
pub(crate) struct SessionLoop<P: Protocol> {
    pub(crate) session: Session<P, Connected>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) event_tx: mpsc::Sender<Event<P>>,
    pub(crate) command_rx: mpsc::Receiver<Command>,
    /// Publishes the arrival instant of every inbound datagram for
    /// link-health consumers on the handle side.
    pub(crate) activity_tx: watch::Sender<Instant>,
}

impl<P: Protocol> SessionLoop<P> {
    /// Drive the session until the loop exits (handle dropped, error, etc.).
    pub(crate) async fn run(mut self) -> Result<(), ShellError> {
        let result = self.run_inner().await;
        match &result {
            Ok(()) => tracing::debug!(
                target: "dstar_gateway::tokio_shell",
                "session loop exited cleanly"
            ),
            Err(e) => tracing::warn!(
                target: "dstar_gateway::tokio_shell",
                error = %e,
                "session loop exited with error"
            ),
        }
        result
    }

    async fn run_inner(&mut self) -> Result<(), ShellError> {
        let mut rx_buf = [0u8; 2048];

        loop {
            // 1. Drain the outbox to the socket.
            self.flush_transmit().await?;

            // 2. Drain events to the consumer channel.
            while let Some(evt) = self.session.poll_event() {
                if self.deliver_event(evt).await?.is_break() {
                    return Ok(());
                }
            }

            // 3. Compute the next deadline.
            let next_wake = self.session.poll_timeout();

            tokio::select! {
                biased;

                cmd = self.command_rx.recv() => {
                    let Some(cmd) = cmd else {
                        tracing::debug!(
                            target: "dstar_gateway::tokio_shell",
                            "command channel closed; exiting loop"
                        );
                        return Ok(());
                    };
                    self.apply_command(cmd);
                }

                recv = self.socket.recv_from(&mut rx_buf) => {
                    let (n, peer) = match recv {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                target: "dstar_gateway::tokio_shell",
                                error = %e,
                                "UDP recv_from failed"
                            );
                            return Err(ShellError::Core(CoreError::Io {
                                source: e,
                                operation: IoOperation::UdpRecv,
                            }));
                        }
                    };
                    if peer != self.session.peer() {
                        tracing::debug!(
                            target: "dstar_gateway::tokio_shell",
                            expected = %self.session.peer(),
                            actual = %peer,
                            bytes_len = n,
                            "ignoring UDP datagram from unexpected peer"
                        );
                        continue;
                    }
                    // Refresh the peer-activity watch for link-health
                    // consumers. Send only fails when every receiver
                    // is gone, which is harmless here.
                    let _unused = self.activity_tx.send(Instant::now());
                    let slice = rx_buf.get(..n).unwrap_or(&[]);
                    if let Err(e) = self.session.handle_input(Instant::now(), peer, slice) {
                        tracing::warn!(
                            target: "dstar_gateway::tokio_shell",
                            error = %e,
                            peer = %peer,
                            bytes_len = slice.len(),
                            "handle_input rejected datagram"
                        );
                        return Err(e.into());
                    }
                }

                () = sleep_until_or_pending(next_wake) => {
                    self.session.handle_timeout(Instant::now());
                }
            }
        }
    }

    /// Retain an event in FIFO order while servicing commands under backpressure.
    ///
    /// Reserving capacity leaves ownership of the pending event here while a
    /// command is handled. Further UDP input waits until the event is delivered,
    /// preserving the bounded receive queue. Either handle channel closing ends
    /// the session task.
    async fn deliver_event(&mut self, event: Event<P>) -> Result<ControlFlow<()>, ShellError> {
        loop {
            let command = tokio::select! {
                biased;

                permit = self.event_tx.reserve() => {
                    let Ok(permit) = permit else {
                        tracing::debug!(
                            target: "dstar_gateway::tokio_shell",
                            "event consumer dropped; exiting loop"
                        );
                        return Ok(ControlFlow::Break(()));
                    };
                    permit.send(event);
                    return Ok(ControlFlow::Continue(()));
                }

                command = self.command_rx.recv() => command,
            };
            let Some(command) = command else {
                tracing::debug!(
                    target: "dstar_gateway::tokio_shell",
                    "command channel closed; exiting loop"
                );
                return Ok(ControlFlow::Break(()));
            };
            self.apply_command(command);
            self.flush_transmit().await?;
        }
    }

    /// Flush every datagram currently queued by the core to the peer socket.
    async fn flush_transmit(&mut self) -> Result<(), ShellError> {
        while let Some(tx) = self.session.poll_transmit(Instant::now()) {
            // Trace every outbound datagram so a KeepaliveInactivity
            // post-mortem can confirm whether POLLs continued through the
            // silent window. The first byte distinguishes keepalives from
            // voice packets without dumping the whole payload.
            let first_byte = tx.payload.first().copied().unwrap_or(0);
            tracing::trace!(
                target: "dstar_gateway::tokio_shell",
                dst = %tx.dst,
                bytes = tx.payload.len(),
                first = format_args!("{first_byte:#04x}"),
                "UDP send_to"
            );
            if let Err(error) = self.socket.send_to(tx.payload, tx.dst).await {
                tracing::warn!(
                    target: "dstar_gateway::tokio_shell",
                    %error,
                    dst = %tx.dst,
                    "UDP send_to failed"
                );
                return Err(ShellError::Core(CoreError::Io {
                    source: error,
                    operation: IoOperation::UdpSend,
                }));
            }
        }
        Ok(())
    }

    /// Apply a command from the handle.
    ///
    /// Dispatches each [`Command`] variant to the corresponding
    /// `Session<P, Connected>` method. For `SendHeader`, `SendVoice`,
    /// and `SendEot`, the reply channel carries the encoder result
    /// (or [`ShellError::Core`] on codec failure). For `Disconnect`,
    /// the reply fires immediately once the UNLINK has been enqueued;
    /// the caller then waits for [`Event::Disconnected`] via
    /// `next_event`.
    fn apply_command(&mut self, cmd: Command) {
        let now = Instant::now();
        match cmd {
            Command::SendHeader {
                header,
                stream_id,
                reply,
            } => {
                let result = self
                    .session
                    .send_header(now, &header, stream_id)
                    .map_err(ShellError::Core);
                // If the receiver was dropped the reply is lost; the
                // caller has already given up, so there's nothing to
                // do about it here.
                drop(reply.send(result));
            }
            Command::SendVoice {
                stream_id,
                seq,
                frame,
                reply,
            } => {
                let result = self
                    .session
                    .send_voice(now, stream_id, seq, &frame)
                    .map_err(ShellError::Core);
                drop(reply.send(result));
            }
            Command::SendEot {
                stream_id,
                seq,
                reply,
            } => {
                let result = self
                    .session
                    .send_eot(now, stream_id, seq)
                    .map_err(ShellError::Core);
                drop(reply.send(result));
            }
            Command::Disconnect { reply } => {
                // `disconnect_in_place` advances the internal state
                // machine to `Disconnecting` without consuming the
                // typestate handle. The reply reports command acceptance;
                // the terminal outcome follows on the event channel.
                let result = self
                    .session
                    .disconnect_in_place(now)
                    .map_err(ShellError::Core);
                drop(reply.send(result));
            }
        }
    }
}

/// Bridge `Option<Instant>` to a future. `None` → never wakes.
async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
#[path = "../../tests/common/mod.rs"]
mod test_support;

#[cfg(test)]
mod tests {
    use super::test_support::fake_reflector::FakeReflector;
    use super::*;
    use crate::tokio_shell::drive_connecting;
    use dstar_gateway_core::codec::dextra::{self, ClientPacket};
    use dstar_gateway_core::session::client::DExtra;
    use dstar_gateway_core::validator::NullSink;
    use dstar_gateway_core::{Callsign, DstarHeader, Module, StreamId, Suffix, VoiceFrame};
    use std::time::Duration;
    use tokio::sync::oneshot;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    async fn voice_packets(
        fake: &FakeReflector,
    ) -> Result<Vec<ClientPacket>, Box<dyn std::error::Error>> {
        let packets = tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                let packets = fake.received_packets().await;
                let voice = packets
                    .into_iter()
                    .filter(|packet| packet.starts_with(b"DSVT"))
                    .collect::<Vec<_>>();
                if voice.len() == 3 {
                    return voice;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "commands were acknowledged but UDP output remained blocked")?;
        packets
            .iter()
            .map(|packet| dextra::decode_client_to_server(packet, &mut NullSink))
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    #[tokio::test]
    async fn full_event_queue_allows_commands_and_udp_output_without_reordering_events()
    -> TestResult {
        let fake = FakeReflector::spawn_dextra().await?;
        let peer = fake.local_addr()?;
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let connecting = Session::<DExtra, _>::builder()
            .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
            .local_module(Module::B)
            .reflector_module(Module::C)
            .peer(peer)
            .build()
            .connect(Instant::now())?;
        let session = drive_connecting(connecting, &socket, TEST_TIMEOUT).await?;
        let (event_tx, mut events) = mpsc::channel(1);
        event_tx.send(Event::PollEcho { peer }).await?;
        let (command_tx, command_rx) = mpsc::channel(1);
        let (activity_tx, _activity_rx) = watch::channel(Instant::now());
        let actor = tokio::spawn(
            SessionLoop {
                session,
                socket,
                event_tx,
                command_rx,
                activity_tx,
            }
            .run(),
        );

        // The handshake's Connected event is blocked behind the occupied
        // one-slot channel. No event receiver is polled until every command
        // has completed and all of its packets have reached the fake peer.
        let header = DstarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"XRF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"XRF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        };
        let stream_id = StreamId::new(0x1234).ok_or("zero stream id")?;
        let frame = VoiceFrame::silence();
        let (header_reply, header_result) = oneshot::channel();
        let (voice_reply, voice_result) = oneshot::channel();
        let (eot_reply, eot_result) = oneshot::channel();
        for (command, result) in [
            (
                Command::SendHeader {
                    header: Box::new(header),
                    stream_id,
                    reply: header_reply,
                },
                header_result,
            ),
            (
                Command::SendVoice {
                    stream_id,
                    seq: 1,
                    frame: Box::new(frame),
                    reply: voice_reply,
                },
                voice_result,
            ),
            (
                Command::SendEot {
                    stream_id,
                    seq: 2,
                    reply: eot_reply,
                },
                eot_result,
            ),
        ] {
            command_tx.send(command).await?;
            tokio::time::timeout(TEST_TIMEOUT, result)
                .await
                .map_err(|_| "outbound command blocked behind a full event queue")???;
        }
        assert_eq!(
            voice_packets(&fake).await?,
            [
                ClientPacket::VoiceHeader { stream_id, header },
                ClientPacket::VoiceData {
                    stream_id,
                    seq: 1,
                    frame
                },
                ClientPacket::VoiceEot {
                    stream_id,
                    seq: 2 | 0x40,
                },
            ]
        );
        let first_event = events.recv().await;
        assert!(
            matches!(first_event, Some(Event::PollEcho { .. })),
            "the prefilled event must be delivered first, got {first_event:?}"
        );
        let second_event = tokio::time::timeout(TEST_TIMEOUT, events.recv()).await?;
        assert!(
            matches!(second_event, Some(Event::Connected { .. })),
            "the pending handshake event must follow the prefilled event, got {second_event:?}"
        );
        drop(command_tx);
        tokio::time::timeout(TEST_TIMEOUT, actor).await???;
        Ok(())
    }
}
