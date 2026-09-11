//! Cooperative input handling for a session with non-atomic I/O operations.
//!
//! A completed input requests a transition at an operation boundary. Once a
//! cycle has started, its future remains alive until every queued side effect
//! and the corresponding local state update have completed.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

/// Whether consumed voice events should be relayed and announced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RelayMode {
    /// Forward live voice in both directions and announce stream events.
    Monitoring,
    /// Consume events without forwarding or retaining their voice payloads.
    Paused,
}

/// Operations required to leave both voice directions at a stream boundary.
pub(super) trait StreamLifecycle: Send {
    /// Free event capacity and discard speech that will no longer be relayed.
    fn discard_queued_events(&mut self) -> impl Future<Output = ()> + Send;

    /// Complete EOT for an active radio-to-reflector stream.
    fn finish_network_stream(&mut self, context: &'static str) -> impl Future<Output = ()> + Send;

    /// Complete EOT for an active reflector-to-radio stream.
    fn finish_radio_stream(&mut self, context: &'static str) -> impl Future<Output = ()> + Send;
}

/// End both streams after releasing reflector event-channel backpressure.
///
/// The reflector task services commands only after delivering its events, so
/// its event queue must have room before EOT can be acknowledged. Both monitor
/// pause and final shutdown use this ordering, after settling their poll cycle.
pub(super) async fn settle_streams<S: StreamLifecycle>(session: &mut S, context: &'static str) {
    session.discard_queued_events().await;
    session.finish_network_stream(context).await;
    session.finish_radio_stream(context).await;
}

/// Service one cycle, completing any started operation before returning input.
///
/// A pending input does not suspend event consumption. An already-ready input
/// does not begin another cycle. If input arrives during a cycle, that cycle
/// finishes before its result is returned; dropping a modem or reflector send
/// after enqueueing it could otherwise orphan its local stream bookkeeping.
pub(super) async fn complete_cycle_or_input<C, I>(
    cycle: C,
    mut input: Pin<&mut I>,
) -> Option<I::Output>
where
    C: Future<Output = ()> + Send,
    I: Future + Send,
    I::Output: Send,
{
    let started = AtomicBool::new(false);
    let mut cycle = pin!(async {
        started.store(true, Ordering::Relaxed);
        cycle.await;
    });
    tokio::select! {
        biased;
        result = &mut input => {
            if started.load(Ordering::Relaxed) {
                cycle.await;
            }
            Some(result)
        }
        () = &mut cycle => None,
    }
}

/// Consume an immediately available event without waiting for future traffic.
///
/// Only use this with a cancellation-safe receive operation. It is used when
/// resuming monitoring to discard events queued before the operator resumed.
/// The receive gets one poll without a cooperative scheduling limit, because
/// exhausting Tokio's task budget also returns `Pending` for a nonempty queue.
/// Callers drain bounded event queues; no waiting I/O runs unconstrained.
pub(super) async fn take_ready<F: Future + Send>(future: F) -> Option<F::Output> {
    let mut future = pin!(tokio::task::coop::unconstrained(future));
    poll_fn(|context| match future.as_mut().poll(context) {
        Poll::Ready(value) => Poll::Ready(Some(value)),
        Poll::Pending => Poll::Ready(None),
    })
    .await
}

/// Receive queued events until the producer also has had a quiet interval.
///
/// A producer blocked on a full channel may still own an undelivered event
/// when an immediate receive first finds the channel empty. Give that producer
/// a short interval to deliver its backlog before treating the stream as live.
pub(super) async fn next_event_before_quiet<F: Future + Send>(
    future: F,
    quiet_interval: Duration,
) -> Option<F::Output> {
    let mut future = pin!(future);
    if let Some(event) = take_ready(future.as_mut()).await {
        return Some(event);
    }
    tokio::time::timeout(quiet_interval, future).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::{mpsc, oneshot};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct TestSession {
        events: mpsc::Receiver<usize>,
        commands: mpsc::Sender<oneshot::Sender<()>>,
        network_active: bool,
        radio_active: bool,
        ended: Vec<&'static str>,
    }

    impl StreamLifecycle for TestSession {
        async fn discard_queued_events(&mut self) {
            while let Some(Some(_)) = take_ready(self.events.recv()).await {}
        }

        async fn finish_network_stream(&mut self, _context: &'static str) {
            if self.network_active {
                let (reply, received) = oneshot::channel();
                assert!(self.commands.send(reply).await.is_ok());
                assert!(received.await.is_ok());
                self.network_active = false;
                self.ended.push("network");
            }
        }

        async fn finish_radio_stream(&mut self, _context: &'static str) {
            if self.radio_active {
                self.radio_active = false;
                self.ended.push("radio");
            }
        }
    }

    #[tokio::test]
    async fn pause_releases_a_full_event_queue_and_ends_both_streams() -> TestResult {
        let (events, received) = mpsc::channel(2);
        let (commands, mut requests) = mpsc::channel::<oneshot::Sender<()>>(1);
        let (filled, observed_full) = oneshot::channel();
        let actor = tokio::spawn(async move {
            events.send(0).await?;
            events.send(1).await?;
            filled.send(()).map_err(|()| "queue observer closed")?;
            // Match the reflector task: delivery of an event precedes
            // command processing, and a full event queue blocks both.
            events.send(2).await?;
            let reply = requests.recv().await.ok_or("EOT command not delivered")?;
            reply.send(()).map_err(|()| "EOT reply abandoned")?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        });
        observed_full.await?;
        let mut session = TestSession {
            events: received,
            commands,
            network_active: true,
            radio_active: true,
            ended: Vec::new(),
        };

        tokio::time::timeout(
            Duration::from_secs(1),
            settle_streams(&mut session, "monitor stopped"),
        )
        .await?;
        assert!(
            !session.network_active,
            "radio-to-reflector EOT is required"
        );
        assert!(!session.radio_active, "reflector-to-radio EOT is required");
        assert_eq!(session.ended, ["network", "radio"]);
        actor.await?.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn prompt_wait_keeps_a_bounded_event_producer_running() -> TestResult {
        let (events, mut received) = mpsc::channel(2);
        let (input, prompt) = oneshot::channel();
        let producer = tokio::spawn(async move {
            for event in 0..300 {
                events.send(event).await?;
            }
            input.send(()).map_err(|()| "prompt receiver closed")?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        });

        let mut prompt = pin!(prompt);
        let mut consumed = 0;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let cycle = async {
                    if received.recv().await.is_some() {
                        consumed += 1;
                    }
                };
                if let Some(result) = complete_cycle_or_input(cycle, prompt.as_mut()).await {
                    result?;
                    break;
                }
            }
            Ok::<_, oneshot::error::RecvError>(())
        })
        .await??;
        while let Some(Some(_)) = take_ready(received.recv()).await {
            consumed += 1;
        }
        assert_eq!(consumed, 300, "every paused event must be consumed");
        assert_eq!(take_ready(received.recv()).await, Some(None));
        producer.await?.map_err(|error| error.to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn ready_stop_does_not_enqueue_a_new_operation() {
        let operations = AtomicUsize::new(0);
        let mut stop = pin!(std::future::ready(()));
        let result = complete_cycle_or_input(
            async {
                let _previous = operations.fetch_add(1, Ordering::SeqCst);
            },
            stop.as_mut(),
        )
        .await;
        assert_eq!(result, Some(()));
        assert_eq!(operations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stop_after_enqueue_waits_for_reply_and_stream_bookkeeping() -> TestResult {
        let (enqueued, observed_enqueue) = oneshot::channel();
        let (reply, received_reply) = oneshot::channel();
        let (stop, received_stop) = oneshot::channel();
        let committed = Arc::new(AtomicBool::new(false));
        let completed = Arc::clone(&committed);
        let mut worker = pin!(async move {
            let mut stop = pin!(received_stop);
            let mut reply_received = false;
            let cycle = async {
                assert!(enqueued.send(()).is_ok());
                reply_received = received_reply.await.is_ok();
                completed.store(reply_received, Ordering::SeqCst);
            };
            let result = complete_cycle_or_input(cycle, stop.as_mut()).await;
            (result, reply_received)
        });

        assert!(take_ready(worker.as_mut()).await.is_none());
        observed_enqueue.await?;
        stop.send(()).map_err(|()| "stop receiver closed")?;
        assert!(
            take_ready(worker.as_mut()).await.is_none(),
            "stop must retain the queued operation"
        );
        assert!(!committed.load(Ordering::SeqCst));
        reply
            .send(())
            .map_err(|()| "in-flight reply was abandoned")?;
        let (result, reply_received) = worker.await;
        assert!(matches!(result, Some(Ok(()))));
        assert!(reply_received);
        assert!(committed.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn resume_discards_queued_events_and_keeps_future_events() -> TestResult {
        let (events, mut received) = mpsc::channel(300);
        for _ in 0..300 {
            events.try_send("paused speech")?;
        }
        let mut discarded = 0;
        while let Some(Some(_)) = take_ready(received.recv()).await {
            discarded += 1;
        }
        assert_eq!(
            discarded, 300,
            "a cooperative yield must not be mistaken for an empty queue"
        );
        assert_eq!(take_ready(received.recv()).await, None);

        events.send("live header").await?;
        assert_eq!(received.recv().await, Some("live header"));
        Ok(())
    }

    #[tokio::test]
    async fn resume_waits_for_an_event_parked_in_the_producer() -> TestResult {
        let (events, mut received) = mpsc::channel(2);
        events.try_send("paused header")?;
        events.try_send("paused voice")?;
        let producer = tokio::spawn(async move {
            events.send("parked voice").await?;
            Ok::<_, mpsc::error::SendError<&'static str>>(events)
        });

        let mut discarded = Vec::new();
        while let Some(Some(event)) =
            next_event_before_quiet(received.recv(), Duration::from_millis(5)).await
        {
            discarded.push(event);
        }
        assert_eq!(discarded, ["paused header", "paused voice", "parked voice"]);
        let events = producer.await??;
        events.send("live header").await?;
        assert_eq!(received.recv().await, Some("live header"));
        Ok(())
    }
}
