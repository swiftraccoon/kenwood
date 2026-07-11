//! Main-thread job broker for transports with thread-affine platform
//! APIs.
//!
//! macOS `IOBluetooth` can only (re)open an RFCOMM channel on the
//! thread that runs the `CFRunLoop`. The application constructs a
//! [`MainThreadBroker`] on that thread and calls
//! [`MainThreadBroker::pump`] from its existing loop tick; any thread
//! holding a [`BrokerHandle`] can then submit a synchronous job and
//! await its result.

use std::sync::mpsc;

use crate::error::TransportError;

/// A synchronous job shipped to the broker's thread, paired with the
/// channel that carries its result back to the submitter.
type Job = (
    Box<dyn FnOnce() -> Result<(), TransportError> + Send>,
    tokio::sync::oneshot::Sender<Result<(), TransportError>>,
);

/// Receiving side of the broker: owned and pumped by the thread the
/// platform API is affine to.
#[derive(Debug)]
pub struct MainThreadBroker {
    rx: mpsc::Receiver<Job>,
    tx: mpsc::Sender<Job>,
}

/// Cloneable submission handle usable from any thread.
#[derive(Debug, Clone)]
pub struct BrokerHandle {
    tx: mpsc::Sender<Job>,
}

impl MainThreadBroker {
    /// Create a broker. Call this on the thread that must run the
    /// jobs (the `CFRunLoop` thread for `IOBluetooth`).
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { rx, tx }
    }

    /// A handle for submitting jobs from other threads.
    #[must_use]
    pub fn handle(&self) -> BrokerHandle {
        BrokerHandle {
            tx: self.tx.clone(),
        }
    }

    /// Run every queued job on the calling thread; returns how many
    /// jobs ran. Call from the affine thread's existing loop tick.
    pub fn pump(&mut self) -> usize {
        let mut ran = 0;
        while let Ok((job, reply)) = self.rx.try_recv() {
            // A dropped awaiter is fine — the job still ran.
            drop(reply.send(job()));
            ran += 1;
        }
        ran
    }
}

impl Default for MainThreadBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl BrokerHandle {
    /// Submit `job` and return a future resolving to its result.
    ///
    /// The job is enqueued immediately (before the returned future is
    /// first polled), so a caller on the pumping thread itself can
    /// submit, [`pump`](MainThreadBroker::pump), and then await.
    ///
    /// # Errors
    ///
    /// The future resolves to [`TransportError::WrongThread`] if the
    /// broker is gone (no thread will ever run the job); otherwise it
    /// carries whatever the job itself returns.
    pub fn run(
        &self,
        job: Box<dyn FnOnce() -> Result<(), TransportError> + Send>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send + use<> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let sent = self.tx.send((job, reply_tx)).is_ok();
        async move {
            if !sent {
                return Err(TransportError::WrongThread);
            }
            reply_rx.await.map_err(|_| TransportError::WrongThread)?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn job_runs_on_pumping_thread_and_returns_result() -> TestResult {
        let mut broker = MainThreadBroker::new();
        let handle = broker.handle();
        let pump_thread = std::thread::current().id();
        let fut = handle.run(Box::new(move || {
            if std::thread::current().id() == pump_thread {
                Ok(())
            } else {
                Err(TransportError::WrongThread)
            }
        }));
        // `run` enqueues eagerly (before its future is polled), so a
        // single-threaded test can pump first, then await.
        assert_eq!(broker.pump(), 1, "exactly one job should run");
        fut.await.map_err(Into::into)
    }

    #[tokio::test]
    async fn job_error_propagates() -> TestResult {
        let mut broker = MainThreadBroker::new();
        let handle = broker.handle();
        let fut = handle.run(Box::new(|| Err(TransportError::NotFound)));
        assert_eq!(broker.pump(), 1, "exactly one job should run");
        let r = fut.await;
        assert!(matches!(r, Err(TransportError::NotFound)), "got {r:?}");
        Ok(())
    }

    #[tokio::test]
    async fn dropped_broker_fails_pending_jobs() -> TestResult {
        let broker = MainThreadBroker::new();
        let handle = broker.handle();
        drop(broker);
        let r = handle.run(Box::new(|| Ok(()))).await;
        assert!(matches!(r, Err(TransportError::WrongThread)), "got {r:?}");
        Ok(())
    }

    #[tokio::test]
    async fn pump_with_no_jobs_is_zero() {
        let mut broker = MainThreadBroker::new();
        assert_eq!(broker.pump(), 0, "no jobs queued");
    }
}
