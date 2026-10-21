//! User-facing handle for an async session running in a spawned task.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::session::client::{Connected, DisconnectReason, Event, Protocol, Session};
use dstar_gateway_core::types::{FrameSequence, StreamId};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use super::{Command, SessionLoop, ShellError};

/// Capacity of the command channel between handle and session task.
///
/// Voice commands are small (header/voice/eot) and arrive at a
/// modest rate (≈50 frames/s max). 32 provides headroom for bursts
/// without unbounded memory use. If the consumer is running
/// behind, `send_voice` awaits backpressure rather than blocking
/// unboundedly.
const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the event channel from session task to handle.
///
/// Events are produced by the loop and consumed by the user via
/// `next_event`. A deeper buffer here lets the loop keep running
/// while the consumer is processing the previous batch. 256 frames
/// holds about five seconds at 50 voice frames per second, with control
/// events sharing that capacity.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Upper bound for delivering a disconnect command and observing the
/// core's terminal event.
///
/// Every supported protocol has a two-second UNLINK deadline. The
/// additional margin covers command dispatch and event delivery even
/// when the consumer has allowed the bounded event queue to fill.
const DISCONNECT_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

/// Async handle to a session running in a spawned tokio task.
///
/// Methods translate to commands sent over an internal channel and
/// reply over a oneshot. Dropping the handle severs the connection
/// from the consumer side; the spawned task exits on its next loop.
///
/// Incoming events use a bounded FIFO queue. If it fills, outbound commands
/// continue to complete and flush their packets to the socket. Further UDP
/// input and protocol timer processing wait for event capacity, so callers
/// must keep consuming [`Self::next_event`] for incoming traffic and link
/// maintenance to progress.
///
/// **Drop is not graceful.** For graceful shutdown call
/// [`AsyncSession::disconnect`]. Drop just severs the connection from
/// the consumer's side; the reflector eventually times the link out
/// via inactivity.
#[derive(Debug)]
pub struct AsyncSession<P> {
    pub(crate) command_tx: mpsc::Sender<Command>,
    pub(crate) event_rx: mpsc::Receiver<Event<P>>,
    /// Instant of the most recent datagram from the peer, published
    /// by the session loop. Cloned out via [`Self::activity`].
    pub(crate) activity_rx: watch::Receiver<Instant>,
    /// Whether a disconnect command has been accepted by the command
    /// channel. This survives cancellation so a later call can resume
    /// waiting instead of enqueueing a second state transition.
    pub(crate) disconnect_requested: bool,
    /// Terminal outcome already observed through `next_event`.
    pub(crate) disconnect_reason: Option<DisconnectReason>,
    /// Retained until joined, including across cancellation of `join`.
    pub(crate) completion: Option<tokio::task::JoinHandle<Result<(), ShellError>>>,
    pub(crate) _protocol: PhantomData<P>,
}

impl<P: Protocol> AsyncSession<P> {
    /// Spawn the session loop on the current tokio runtime and
    /// return a handle for controlling it.
    ///
    /// The `session` must already be in the [`Connected`] state
    /// (typically via `Session::<P, Connecting>::promote` after
    /// observing [`Event::Connected`] from the handshake). The
    /// `socket` must be bound (typically via `UdpSocket::bind`).
    /// The caller must not concurrently receive from that shared socket;
    /// another reader can consume replies intended for this session.
    ///
    /// The loop runs until the handle is dropped, the consumer's
    /// command channel closes, or a fatal I/O error occurs.
    /// [`Self::join`] returns its typed completion result.
    ///
    /// # Panics
    ///
    /// Panics when no Tokio runtime is current. The runtime must enable
    /// I/O and time; unavailable facilities can instead fail the spawned task.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use dstar_gateway::tokio_shell::AsyncSession;
    /// use dstar_gateway_core::session::client::{Connected, DExtra, Session};
    /// use tokio::net::UdpSocket;
    ///
    /// # async fn demo(connected: Session<DExtra, Connected>) -> Result<(), Box<dyn std::error::Error>> {
    /// let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    /// let mut shell = AsyncSession::spawn(connected, sock);
    /// while let Some(event) = shell.next_event().await {
    ///     println!("{event:?}");
    /// }
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn spawn(session: Session<P, Connected>, socket: Arc<UdpSocket>) -> Self
    where
        P: Send + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (activity_tx, activity_rx) = watch::channel(Instant::now());

        let inner_loop = SessionLoop {
            session,
            socket,
            event_tx,
            command_rx,
            activity_tx,
        };

        let completion = tokio::spawn(inner_loop.run());

        Self {
            command_tx,
            event_rx,
            activity_rx,
            disconnect_requested: false,
            disconnect_reason: None,
            completion: Some(completion),
            _protocol: PhantomData,
        }
    }

    /// Watch channel holding the instant the most recent datagram
    /// arrived from the peer (keepalives included). Borrow the
    /// receiver and compare against `Instant::now()` to compute the
    /// link-inactivity age for health displays.
    ///
    /// The initial value is the spawn instant, so the age is well
    /// defined before the first datagram arrives.
    #[must_use]
    pub fn activity(&self) -> watch::Receiver<Instant> {
        self.activity_rx.clone()
    }

    /// Pull the next event from the inbound stream.
    ///
    /// Returns `None` once the session task has exited and the event
    /// channel has been fully drained.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. It only awaits a `tokio::sync::mpsc`
    /// receiver, which is documented as cancel-safe: dropping the future
    /// leaves the channel in a clean state and any undelivered events
    /// remain queued for the next call.
    pub async fn next_event(&mut self) -> Option<Event<P>> {
        let event = self.event_rx.recv().await;
        if let Some(Event::Disconnected { reason }) = event.as_ref() {
            self.disconnect_reason = Some(*reason);
        }
        event
    }

    /// Join the session task and return its typed result once.
    ///
    /// This drains queued events while waiting, retaining a terminal disconnect
    /// reason. Call after disconnect or event-channel closure; a healthy linked
    /// session continues running. After the result is consumed, repeated calls
    /// return `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns the task's socket/protocol failure or [`ShellError::Task`] if
    /// it panicked or was externally cancelled.
    ///
    /// # Cancellation
    ///
    /// Cancelling this wait retains the task handle for a later call.
    pub async fn join(&mut self) -> Result<(), ShellError> {
        let Some(task) = self.completion.as_mut() else {
            return Ok(());
        };
        let mut events_open = true;
        let result = loop {
            tokio::select! {
                result = &mut *task => break result.map_err(ShellError::Task),
                event = self.event_rx.recv(), if events_open => {
                    match event {
                        Some(Event::Disconnected { reason }) => self.disconnect_reason = Some(reason),
                        Some(_) => {}
                        None => events_open = false,
                    }
                }
            }
        };
        self.completion = None;
        result?
    }

    /// Terminal core disconnect reason, if already observed.
    #[must_use]
    pub const fn disconnect_reason(&self) -> Option<DisconnectReason> {
        self.disconnect_reason
    }

    /// Send a voice header and start a new outbound voice stream.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::Core`] if the core rejects the operation
    /// - [`ShellError::SessionFailed`] if UDP transmission fails
    ///
    /// Success means every ready datagram was handed to the local UDP socket;
    /// it is not a remote acknowledgement.
    ///
    /// # Cancellation safety
    ///
    /// Cancellation before enqueue sends no command. After enqueue, the task
    /// completes the operation and may transmit even if the reply future is
    /// dropped. Call `join` after task termination to inspect its result.
    pub async fn send_header(
        &mut self,
        header: DstarHeader,
        stream_id: StreamId,
    ) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendHeader {
                header: Box::new(header),
                stream_id,
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Send a voice data frame.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::Core`] if the encoder rejects the frame
    ///
    /// # Cancellation safety
    ///
    /// Enqueue, UDP completion and cancellation follow [`Self::send_header`].
    pub async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: FrameSequence,
        frame: VoiceFrame,
    ) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendVoice {
                stream_id,
                seq,
                frame: Box::new(frame),
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Send a voice EOT and close the outbound stream.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::Core`] if the encoder rejects the EOT
    ///
    /// # Cancellation safety
    ///
    /// Enqueue, UDP completion and cancellation follow [`Self::send_header`].
    pub async fn send_eot(
        &mut self,
        stream_id: StreamId,
        seq: FrameSequence,
    ) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendEot {
                stream_id,
                seq,
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Gracefully disconnect and return the terminal outcome.
    ///
    /// Sends an UNLINK, drains any queued events that would otherwise
    /// backpressure the session loop, and waits until
    /// [`Event::Disconnected`] reports either an acknowledgement or the
    /// protocol's own two-second deadline. The entire operation has a
    /// five-second shell deadline, so a stalled task cannot hold shutdown
    /// indefinitely.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::DisconnectStalled`] if the session task does not
    ///   report a terminal outcome before the shell deadline
    /// - [`ShellError::DisconnectUnacknowledged`] if the reflector does
    ///   not acknowledge UNLINK before the protocol deadline
    /// - [`ShellError::DisconnectedBeforeUnlink`] if the session has
    ///   already ended for another reason
    /// - [`ShellError::Core`] if the core rejects the state transition
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. Once the command is enqueued, that fact
    /// is retained in the handle. Calling `disconnect` again resumes
    /// draining and waiting for the same terminal event rather than
    /// attempting a second transition.
    pub async fn disconnect(&mut self) -> Result<(), ShellError> {
        let deadline = tokio::time::Instant::now() + DISCONNECT_COMPLETION_TIMEOUT;
        if let Some(reason) = self.disconnect_reason {
            self.join_before(deadline).await?;
            return disconnect_result(reason);
        }

        // Free one queue's capacity before dispatch. Events arriving during
        // the drain must not postpone the disconnect command indefinitely.
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            let Ok(event) = self.event_rx.try_recv() else {
                break;
            };
            if let Event::Disconnected { reason } = event {
                self.disconnect_reason = Some(reason);
                self.join_before(deadline).await?;
                return disconnect_result(reason);
            }
        }

        let mut command_reply = if self.disconnect_requested {
            None
        } else {
            let (reply, receiver) = tokio::sync::oneshot::channel();
            let sent = tokio::time::timeout_at(
                deadline,
                self.command_tx.send(Command::Disconnect { reply }),
            )
            .await
            .map_err(|_| ShellError::DisconnectStalled)?;
            if sent.is_err() {
                self.join_before(deadline).await?;
                return Err(ShellError::SessionClosed);
            }
            self.disconnect_requested = true;
            Some(receiver)
        };

        let completion = async {
            loop {
                if let Some(receiver) = command_reply.as_mut() {
                    tokio::select! {
                        biased;

                        event = self.event_rx.recv() => {
                            let Some(event) = event else {
                                return Err(ShellError::SessionClosed);
                            };
                            if let Event::Disconnected { reason } = event {
                                self.disconnect_reason = Some(reason);
                                return Ok(reason);
                            }
                        }

                        reply = receiver => {
                            match reply.map_err(|_| ShellError::SessionClosed)? {
                                Ok(()) => command_reply = None,
                                Err(error) => {
                                    // State rejection can be retried; a task
                                    // failure is joined before returning.
                                    self.disconnect_requested = false;
                                    return Err(error);
                                }
                            }
                        }
                    }
                } else {
                    let Some(event) = self.event_rx.recv().await else {
                        return Err(ShellError::SessionClosed);
                    };
                    if let Event::Disconnected { reason } = event {
                        self.disconnect_reason = Some(reason);
                        return Ok(reason);
                    }
                }
            }
        };

        let outcome = tokio::time::timeout_at(deadline, completion)
            .await
            .map_err(|_| ShellError::DisconnectStalled)?;
        let reason = match outcome {
            Ok(reason) => reason,
            Err(error) => {
                if matches!(
                    &error,
                    ShellError::SessionClosed | ShellError::SessionFailed(_)
                ) {
                    self.join_before(deadline).await?;
                }
                return Err(error);
            }
        };
        self.join_before(deadline).await?;
        disconnect_result(reason)
    }

    async fn join_before(&mut self, deadline: tokio::time::Instant) -> Result<(), ShellError> {
        tokio::time::timeout_at(deadline, self.join())
            .await
            .map_err(|_| ShellError::DisconnectStalled)?
    }
}

const fn disconnect_result(reason: DisconnectReason) -> Result<(), ShellError> {
    match reason {
        DisconnectReason::UnlinkAcked => Ok(()),
        DisconnectReason::DisconnectTimeout => Err(ShellError::DisconnectUnacknowledged),
        reason => Err(ShellError::DisconnectedBeforeUnlink { reason }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::session::client::DExtra;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_session(
        command_tx: mpsc::Sender<Command>,
        event_rx: mpsc::Receiver<Event<DExtra>>,
    ) -> AsyncSession<DExtra> {
        let (_activity_tx, activity_rx) = watch::channel(Instant::now());
        AsyncSession {
            command_tx,
            event_rx,
            activity_rx,
            disconnect_requested: false,
            disconnect_reason: None,
            completion: None,
            _protocol: PhantomData,
        }
    }

    fn poll_echo() -> Event<DExtra> {
        Event::PollEcho {
            peer: ([127, 0, 0, 1], 20_001).into(),
        }
    }

    fn disconnected() -> Event<DExtra> {
        Event::Disconnected {
            reason: DisconnectReason::UnlinkAcked,
        }
    }

    #[tokio::test]
    async fn cancelled_join_retains_the_running_task_and_its_failure() -> TestResult {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (_event_tx, event_rx) = mpsc::channel(1);
        let mut session = test_session(command_tx, event_rx);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        session.completion = Some(tokio::spawn(async move {
            let _sent = started_tx.send(());
            let _released = release_rx.await;
            Err(ShellError::SendStalled)
        }));
        let mut first = Box::pin(session.join());
        tokio::select! {
            result = first.as_mut() => return Err(format!("join completed before release: {result:?}").into()),
            started = started_rx => started?,
        }
        drop(first);
        let _sent = release_tx.send(());
        let result = session.join().await;
        assert!(matches!(result, Err(ShellError::SendStalled)), "{result:?}");
        assert!(
            session.completion.is_none(),
            "completed result is consumed once"
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_drains_a_full_event_channel_before_command_dispatch() -> TestResult {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        event_tx.send(poll_echo()).await?;

        let (blocked_send_started, blocked_send_observed) = tokio::sync::oneshot::channel();
        let loop_task = tokio::spawn(async move {
            let _send_result = blocked_send_started.send(());
            event_tx.send(poll_echo()).await?;
            let Some(Command::Disconnect { reply }) = command_rx.recv().await else {
                return Err("disconnect command channel closed".into());
            };
            drop(reply.send(Ok(())));
            event_tx.send(disconnected()).await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });
        blocked_send_observed.await?;

        let mut session = test_session(command_tx, event_rx);
        tokio::time::timeout(Duration::from_secs(1), session.disconnect()).await??;
        loop_task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_dispatches_before_draining_events_beyond_one_queue_capacity() -> TestResult
    {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        // The extra event represents a producer replenishing the queue while
        // the initial capacity is drained. Its terminal reason remains visible.
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY + 1);
        for _ in 0..EVENT_CHANNEL_CAPACITY {
            event_tx.send(poll_echo()).await?;
        }
        event_tx.send(disconnected()).await?;

        let mut session = test_session(command_tx, event_rx);
        session.disconnect().await?;
        assert!(
            matches!(command_rx.try_recv(), Ok(Command::Disconnect { .. })),
            "events beyond the initial queue capacity must not delay command dispatch"
        );
        assert_eq!(
            session.disconnect_reason(),
            Some(DisconnectReason::UnlinkAcked),
            "the terminal event after the initial drain must remain observable"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_deadline_includes_join_after_the_terminal_event() -> TestResult {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        let mut session = test_session(command_tx, event_rx);
        session.completion = Some(tokio::spawn(async move {
            let Some(Command::Disconnect { reply }) = command_rx.recv().await else {
                return Err(ShellError::SessionClosed);
            };
            drop(reply.send(Ok(())));
            event_tx
                .send(disconnected())
                .await
                .map_err(|_| ShellError::SessionClosed)?;
            std::future::pending::<Result<(), ShellError>>().await
        }));

        let result = tokio::time::timeout(
            DISCONNECT_COMPLETION_TIMEOUT + Duration::from_secs(1),
            session.disconnect(),
        )
        .await?;
        assert!(
            matches!(result, Err(ShellError::DisconnectStalled)),
            "joining the task must share the disconnect deadline: {result:?}"
        );
        let task = session
            .completion
            .take()
            .ok_or("timed-out join lost its task")?;
        task.abort();
        assert!(task.await.is_err(), "aborted task must be joined");
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_disconnect_resumes_without_a_second_command() -> TestResult {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        let (command_seen, command_observed) = tokio::sync::oneshot::channel();
        let (release_event, event_released) = tokio::sync::oneshot::channel();
        let loop_task = tokio::spawn(async move {
            let Some(Command::Disconnect { reply }) = command_rx.recv().await else {
                return Err("disconnect command channel closed".into());
            };
            drop(reply.send(Ok(())));
            let _send_result = command_seen.send(());
            event_released.await?;
            event_tx.send(disconnected()).await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });

        let mut session = test_session(command_tx, event_rx);
        let mut first_attempt = Box::pin(session.disconnect());
        tokio::select! {
            result = first_attempt.as_mut() => {
                return Err(format!("disconnect completed before cancellation: {result:?}").into());
            }
            observed = command_observed => {
                observed?;
            }
        }
        drop(first_attempt);

        let _send_result = release_event.send(());
        session.disconnect().await?;
        loop_task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn rejected_disconnect_can_be_requested_again() -> TestResult {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        let loop_task = tokio::spawn(async move {
            let Some(Command::Disconnect { reply: first }) = command_rx.recv().await else {
                return Err("first disconnect command channel closed".into());
            };
            drop(first.send(Err(ShellError::DisconnectStalled)));

            let Some(Command::Disconnect { reply: second }) = command_rx.recv().await else {
                return Err("second disconnect command channel closed".into());
            };
            drop(second.send(Ok(())));
            event_tx.send(disconnected()).await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });

        let mut session = test_session(command_tx, event_rx);
        let first = session.disconnect().await;
        assert!(
            matches!(first, Err(ShellError::DisconnectStalled)),
            "command rejection was not surfaced: {first:?}"
        );
        session.disconnect().await?;
        loop_task.await??;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_has_a_shell_deadline_when_no_terminal_event_arrives() -> TestResult {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        let loop_task = tokio::spawn(async move {
            let Some(Command::Disconnect { reply }) = command_rx.recv().await else {
                return;
            };
            drop(reply.send(Ok(())));
            std::future::pending::<()>().await;
            drop(event_tx);
        });

        let mut session = test_session(command_tx, event_rx);
        let result = session.disconnect().await;
        assert!(
            matches!(result, Err(ShellError::DisconnectStalled)),
            "expected bounded shell timeout, got {result:?}"
        );
        loop_task.abort();
        Ok(())
    }
}
