//! Tokio event loop driving a sans-io `Session<P, Connected>` over a real `UdpSocket`.

// `SessionLoop` is `pub(crate)` because the handle / spawn constructor
// in `handle.rs` needs to reference it from its sibling submodule,
// but it must not be part of the crate's public API. Clippy's
// `redundant_pub_crate` lint wants us to drop the `(crate)`; that's a
// spurious suggestion for a genuinely crate-private item that merely
// happens to live inside a private module.
#![expect(
    clippy::redundant_pub_crate,
    reason = "SessionLoop is crate-internal infrastructure; pub(crate) documents the intent"
)]

use std::sync::Arc;
use std::time::Instant;

use dstar_gateway_core::error::{Error as CoreError, IoOperation};
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{Connected, Event, Protocol, Session};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::{Command, ShellError};

/// Internal loop that drives a sans-io session over a real tokio `UdpSocket`.
///
/// The loop:
/// 1. Drains `session.poll_transmit(now)` to the socket
/// 2. Drains `session.poll_event()` to the consumer event channel
/// 3. Computes the next deadline via `session.poll_timeout()`
/// 4. Races inbound datagrams, command-channel messages, and timer
///    expiry via `tokio::select!`
/// 5. Repeats
///
/// Dropping the handle closes the `command_rx` channel, which causes
/// the loop to exit on its next iteration.
///
/// The loop is specialized to `Session<P, Connected>` — command
/// dispatch only makes sense on a session that can actually send
/// voice traffic. The shell's spawn path builds the session through
/// the typestate transitions on the main thread, then hands the
/// promoted `Session<P, Connected>` to the loop.
pub(crate) struct SessionLoop<P: Protocol> {
    pub(crate) session: Session<P, Connected>,
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) event_tx: mpsc::Sender<Event<P>>,
    pub(crate) command_rx: mpsc::Receiver<Command>,
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
            while let Some(tx) = self.session.poll_transmit(Instant::now()) {
                if let Err(e) = self.socket.send_to(tx.payload, tx.dst).await {
                    tracing::warn!(
                        target: "dstar_gateway::tokio_shell",
                        error = %e,
                        dst = %tx.dst,
                        "UDP send_to failed"
                    );
                    return Err(ShellError::Core(CoreError::Io {
                        source: e,
                        operation: IoOperation::UdpSend,
                    }));
                }
            }

            // 2. Drain events to the consumer channel.
            while let Some(evt) = self.session.poll_event() {
                if self.event_tx.send(evt).await.is_err() {
                    tracing::debug!(
                        target: "dstar_gateway::tokio_shell",
                        "event consumer dropped; exiting loop"
                    );
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
                // If the receiver was dropped the reply is lost — the
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
                // typestate handle. We intentionally swallow any
                // encoder failure here — the caller only waits for
                // the signal that the request was observed; they
                // then drain events until `Event::Disconnected`
                // arrives (or the channel closes).
                drop(self.session.disconnect_in_place(now));
                // `Result<(), ()>` is `Copy`, so `drop` is a no-op
                // lint trigger; assign to `_` to explicitly discard.
                let _send_result: Result<(), ()> = reply.send(());
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
