//! Open-worker lifecycle over a fake backend: cancellation and deadline
//! handling, joining a worker that finishes late, closing a connection that
//! arrives after the failure, and which failures allow one retry.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll, Wake, Waker};

use kenwood_transport::MockTransport;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Worker = Box<
    dyn FnOnce(BluetoothOpenCancellation) -> Result<Opened<ObservedConnection>, TransportError>
        + Send,
>;

const TEST_BOUND: Duration = Duration::from_secs(2);

#[test]
fn host_retirement_requires_positive_native_evidence() -> TestResult {
    for cleanup in [
        BluetoothCloseFailure::ChannelUnconfirmed,
        BluetoothCloseFailure::ReapPending,
        BluetoothCloseFailure::ForcedTermination,
        BluetoothCloseFailure::HelperExited { code: Some(89) },
    ] {
        let failure = OpenFailure::from_error(&TransportError::BluetoothClose { failure: cleanup });
        assert!(
            !failure.host_retirement_confirmed(),
            "bare cleanup cannot prove application-owner retirement: {cleanup:?}"
        );
        assert!(!failure.retry_allowed());
        assert_eq!(
            serde_json::to_value(&failure)?.get("host_retirement_confirmed"),
            Some(&serde_json::Value::Bool(false))
        );
    }
    let reaped = OpenFailure::from_error(&TransportError::BluetoothOpenWithCleanup {
        stage: BluetoothOpenStage::RfcommDeadline,
        cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
    });
    assert!(reaped.host_retirement_confirmed());
    assert!(
        reaped.retry_allowed(),
        "existing selected retry remains admitted"
    );
    assert_eq!(
        serde_json::to_value(&reaped)?.get("host_retirement_confirmed"),
        Some(&serde_json::Value::Bool(true))
    );
    for error in [
        io::Error::other(reaped.to_string()),
        io::Error::other("unframed helper failure"),
    ] {
        assert!(!OpenFailure::from_error(&error).host_retirement_confirmed());
    }
    let mut late = reaped;
    late.close_error = Some(Failure::from_error(&io::Error::other("late close failed")));
    assert!(!late.host_retirement_confirmed());
    assert_eq!(
        serde_json::to_value(&late)?.get("host_retirement_confirmed"),
        Some(&serde_json::Value::Bool(false)),
        "serialized admission includes independent late-owner cleanup"
    );
    Ok(())
}

#[test]
fn selected_retry_uses_native_stage_and_independent_cleanup_evidence() {
    for stage in [
        BluetoothOpenStage::ContextAllocation,
        BluetoothOpenStage::SdpStart,
        BluetoothOpenStage::SdpCompletion,
        BluetoothOpenStage::SdpDeadline,
        BluetoothOpenStage::RfcommStart,
        BluetoothOpenStage::RfcommCompletion,
        BluetoothOpenStage::RfcommDeadline,
        BluetoothOpenStage::RfcommEndpoint,
    ] {
        let clean = OpenFailure::from_error(&TransportError::BluetoothOpen { stage });
        assert!(clean.retry_allowed(), "eligible opening stage {stage:?}");
        let combined = OpenFailure::from_error(&TransportError::BluetoothOpenWithCleanup {
            stage,
            cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
        });
        assert!(
            combined.retry_allowed(),
            "known reaped opening stage {stage:?}"
        );
    }
    for stage in [
        BluetoothOpenStage::StartupDeadline,
        BluetoothOpenStage::ServiceResolution,
    ] {
        let clean = OpenFailure::from_error(&TransportError::BluetoothOpen { stage });
        assert!(!clean.retry_allowed(), "ineligible opening stage {stage:?}");
        let combined = OpenFailure::from_error(&TransportError::BluetoothOpenWithCleanup {
            stage,
            cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
        });
        assert!(
            !combined.retry_allowed(),
            "cleanup cannot widen stage admission"
        );
    }
    for cleanup in [
        BluetoothCloseFailure::ForcedTermination,
        BluetoothCloseFailure::ReapPending,
        BluetoothCloseFailure::HelperExited { code: Some(89) },
    ] {
        let combined = OpenFailure::from_error(&TransportError::BluetoothOpenWithCleanup {
            stage: BluetoothOpenStage::RfcommDeadline,
            cleanup,
        });
        assert!(!combined.retry_allowed(), "ineligible cleanup {cleanup:?}");
    }
}

#[test]
fn selected_retry_cannot_be_authorized_by_text_or_unrelated_cleanup() {
    let bare_cleanup = TransportError::BluetoothClose {
        failure: BluetoothCloseFailure::ChannelUnconfirmed,
    };
    assert!(!OpenFailure::from_error(&bare_cleanup).retry_allowed());
    let eligible = TransportError::BluetoothOpen {
        stage: BluetoothOpenStage::RfcommDeadline,
    };
    assert!(!OpenFailure::from_error(&io::Error::other(eligible.to_string())).retry_allowed());
    assert!(!OpenFailure::from_error(&TransportError::BluetoothOpenInterrupted).retry_allowed());
    let mut late_owner = OpenFailure::from_error(&eligible);
    late_owner.close_error = Some(Failure::from_error(&bare_cleanup));
    assert!(
        !late_owner.retry_allowed(),
        "late owner cleanup is not opening evidence"
    );
}

#[derive(Debug, Default)]
struct Lifecycle {
    workers: AtomicUsize,
    reads: AtomicUsize,
    writes: AtomicUsize,
    closes: AtomicUsize,
    drops: AtomicUsize,
}

#[derive(Clone, Copy, Debug)]
enum CloseBehavior {
    Success,
    Failure,
    Pending,
}

#[derive(Debug)]
struct ObservedConnection {
    mock: MockTransport,
    lifecycle: Arc<Lifecycle>,
    close: CloseBehavior,
}

impl Transport for ObservedConnection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        let _previous = self.lifecycle.writes.fetch_add(1, Ordering::SeqCst);
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        let _previous = self.lifecycle.reads.fetch_add(1, Ordering::SeqCst);
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let _previous = self.lifecycle.closes.fetch_add(1, Ordering::SeqCst);
        match self.close {
            CloseBehavior::Success => self.mock.close().await,
            CloseBehavior::Failure => Err(TransportError::Disconnected(io::Error::other(
                "scripted late-open close failure",
            ))),
            CloseBehavior::Pending => std::future::pending().await,
        }
    }
}

impl Drop for ObservedConnection {
    fn drop(&mut self) {
        let _previous = self.lifecycle.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn opened(lifecycle: Arc<Lifecycle>, close: CloseBehavior) -> Opened<ObservedConnection> {
    Opened {
        connection: ObservedConnection {
            mock: MockTransport::new(),
            lifecycle,
            close,
        },
        resolved: Resolved {
            address: "00-AA-BB-CC-DD-55".to_owned(),
            rfcomm_channel: 27,
        },
    }
}

#[derive(Debug)]
struct WorkerControl {
    started: tokio::sync::oneshot::Receiver<BluetoothOpenCancellation>,
    release: mpsc::Sender<()>,
}

fn delayed_worker(lifecycle: Arc<Lifecycle>, close: CloseBehavior) -> (Worker, WorkerControl) {
    let (started_tx, started) = tokio::sync::oneshot::channel();
    let (release, release_rx) = mpsc::channel();
    let worker = Box::new(move |cancellation: BluetoothOpenCancellation| {
        let _previous = lifecycle.workers.fetch_add(1, Ordering::SeqCst);
        started_tx.send(cancellation).map_err(|_cancellation| {
            TransportError::Read(io::Error::other("test worker startup receiver disappeared"))
        })?;
        release_rx
            .recv_timeout(TEST_BOUND)
            .map_err(|error| TransportError::Read(io::Error::other(error)))?;
        Ok(opened(lifecycle, close))
    });
    (worker, WorkerControl { started, release })
}

/// A `Wake` that sends on a channel, so a test sees the wake without
/// repolling the future.
#[derive(Debug)]
struct CompletionWake(mpsc::Sender<()>);

impl Wake for CompletionWake {
    fn wake(self: Arc<Self>) {
        let _notification = self.0.send(());
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _notification = self.0.send(());
    }
}

fn completion_waker() -> (Waker, mpsc::Receiver<()>) {
    let (sender, receiver) = mpsc::channel();
    (Waker::from(Arc::new(CompletionWake(sender))), receiver)
}

fn poll_once<F: Future>(future: Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(waker))
}

fn assert_no_radio_io(lifecycle: &Lifecycle) {
    assert_eq!(
        lifecycle.reads.load(Ordering::SeqCst),
        0,
        "opening is not CAT"
    );
    assert_eq!(
        lifecycle.writes.load(Ordering::SeqCst),
        0,
        "opening sends no CAT"
    );
}

fn assert_released_once(lifecycle: &Lifecycle) {
    assert_eq!(
        lifecycle.workers.load(Ordering::SeqCst),
        1,
        "one worker only"
    );
    assert_eq!(
        lifecycle.closes.load(Ordering::SeqCst),
        1,
        "close exactly once"
    );
    assert_eq!(
        lifecycle.drops.load(Ordering::SeqCst),
        1,
        "drop the joined owner"
    );
    assert_no_radio_io(lifecycle);
}

#[tokio::test]
async fn pre_cancelled_open_never_starts_the_worker() -> TestResult {
    let cancelled = AtomicBool::new(true);
    let lifecycle = Arc::new(Lifecycle::default());
    let observed = Arc::clone(&lifecycle);
    let result = open_worker(&cancelled, TEST_BOUND, move |_cancellation| {
        let _previous = observed.workers.fetch_add(1, Ordering::SeqCst);
        Ok(opened(observed, CloseBehavior::Success))
    })
    .await;
    let failure = result.err().ok_or("pre-cancelled open was admitted")?;
    assert!(
        failure.host_retirement_confirmed(),
        "no worker was dispatched"
    );
    assert_eq!(
        failure.error.message,
        TransportError::BluetoothOpenInterrupted.to_string(),
        "retain cancellation as the primary failure"
    );
    assert!(failure.close_error.is_none(), "no owner exists to close");
    assert_eq!(
        lifecycle.workers.load(Ordering::SeqCst),
        0,
        "no worker spawn"
    );
    assert_eq!(
        lifecycle.closes.load(Ordering::SeqCst),
        0,
        "no invented close"
    );
    assert_eq!(
        lifecycle.drops.load(Ordering::SeqCst),
        0,
        "no created owner"
    );
    assert_no_radio_io(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn healthy_open_returns_endpoint_and_leaves_cleanup_to_its_owner() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let observed = Arc::clone(&lifecycle);
    let mut result = open_worker(&cancelled, TEST_BOUND, move |_cancellation| {
        let _previous = observed.workers.fetch_add(1, Ordering::SeqCst);
        Ok(opened(observed, CloseBehavior::Success))
    })
    .await?;
    assert_eq!(
        result.resolved.address, "00-AA-BB-CC-DD-55",
        "retain actual address"
    );
    assert_eq!(
        result.resolved.rfcomm_channel, 27,
        "retain actual SDP channel"
    );
    assert_eq!(
        lifecycle.closes.load(Ordering::SeqCst),
        0,
        "do not close admitted owner"
    );
    assert_eq!(
        lifecycle.drops.load(Ordering::SeqCst),
        0,
        "do not discard admitted owner"
    );
    assert!(
        close(&mut result.connection).await.is_none(),
        "explicit close succeeds"
    );
    drop(result);
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn cancellation_joins_a_running_worker_then_closes_its_late_owner() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let (worker, control) = delayed_worker(Arc::clone(&lifecycle), CloseBehavior::Success);
    let opening = open_worker(&cancelled, TEST_BOUND, worker);
    tokio::pin!(opening);
    let (waker, _wakeups) = completion_waker();
    assert!(
        poll_once(opening.as_mut(), &waker).is_pending(),
        "worker is gated"
    );
    let native_cancellation = tokio::time::timeout(TEST_BOUND, control.started).await??;
    cancelled.store(true, Ordering::SeqCst);
    tokio::time::sleep(CANCELLATION_POLL * 2).await;
    assert!(
        poll_once(opening.as_mut(), &waker).is_pending(),
        "cancellation must still join"
    );
    assert!(
        native_cancellation.is_cancelled(),
        "signal the native worker"
    );
    assert_eq!(
        lifecycle.closes.load(Ordering::SeqCst),
        0,
        "worker still owns construction"
    );
    assert_eq!(
        lifecycle.drops.load(Ordering::SeqCst),
        0,
        "do not detach or invent an owner"
    );
    control.release.send(())?;
    let failure = opening.await.err().ok_or("late owner was admitted")?;
    assert_eq!(
        failure.error.message,
        TransportError::BluetoothOpenInterrupted.to_string(),
        "retain cancellation after joining the late owner"
    );
    assert!(failure.close_error.is_none(), "late close succeeded");
    assert!(failure.host_retirement_confirmed());
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn already_completed_worker_cannot_win_over_current_cancellation() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let (worker, control) = delayed_worker(Arc::clone(&lifecycle), CloseBehavior::Success);
    let opening = open_worker(&cancelled, TEST_BOUND, worker);
    tokio::pin!(opening);
    let (waker, completed) = completion_waker();
    assert!(
        poll_once(opening.as_mut(), &waker).is_pending(),
        "register completion first"
    );
    control.release.send(())?;
    // Deliberately keep this current-thread test executor parked. Only the
    // blocking worker can produce this wake; the timer driver is not polled.
    completed.recv_timeout(TEST_BOUND)?;
    cancelled.store(true, Ordering::SeqCst);
    let failure = opening
        .await
        .err()
        .ok_or("completed worker bypassed cancellation")?;
    assert_eq!(
        failure.error.message,
        TransportError::BluetoothOpenInterrupted.to_string(),
        "ready worker completion cannot erase cancellation"
    );
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn already_completed_worker_cannot_win_after_absolute_deadline() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let (worker, control) = delayed_worker(Arc::clone(&lifecycle), CloseBehavior::Success);
    let budget = Duration::from_millis(10);
    let opening = open_worker(&cancelled, budget, worker);
    tokio::pin!(opening);
    let (waker, completed) = completion_waker();
    assert!(
        poll_once(opening.as_mut(), &waker).is_pending(),
        "start the absolute budget"
    );
    control.release.send(())?;
    completed.recv_timeout(TEST_BOUND)?;
    // Model a busy host executor: native completion is ready, but it cannot
    // be accepted until after the already-started budget has expired.
    std::thread::sleep(budget * 2);
    let failure = opening
        .await
        .err()
        .ok_or("completed worker bypassed the deadline")?;
    assert!(
        failure
            .error
            .causes
            .iter()
            .any(|cause| cause.contains("dispatch budget")),
        "retain timeout cause"
    );
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn expired_budget_signals_worker_and_rejects_its_late_success() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let observed = Arc::clone(&lifecycle);
    let cancellation_seen = Arc::new(AtomicBool::new(false));
    let worker_seen = Arc::clone(&cancellation_seen);
    let result = open_worker(&cancelled, Duration::from_millis(10), move |cancellation| {
        let _previous = observed.workers.fetch_add(1, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + TEST_BOUND;
        while !cancellation.is_cancelled() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        worker_seen.store(cancellation.is_cancelled(), Ordering::SeqCst);
        Ok(opened(observed, CloseBehavior::Success))
    })
    .await;
    let failure = result.err().ok_or("expired worker was admitted")?;
    assert!(
        cancellation_seen.load(Ordering::SeqCst),
        "timeout signals the native worker"
    );
    assert!(failure.close_error.is_none(), "late close succeeded");
    assert!(
        failure
            .error
            .causes
            .iter()
            .any(|cause| cause.contains("dispatch budget")),
        "retain timeout cause"
    );
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn late_close_failure_does_not_replace_the_cancellation_failure() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let (worker, control) = delayed_worker(Arc::clone(&lifecycle), CloseBehavior::Failure);
    let opening = open_worker(&cancelled, TEST_BOUND, worker);
    tokio::pin!(opening);
    let (waker, completed) = completion_waker();
    assert!(
        poll_once(opening.as_mut(), &waker).is_pending(),
        "worker starts pending"
    );
    control.release.send(())?;
    completed.recv_timeout(TEST_BOUND)?;
    cancelled.store(true, Ordering::SeqCst);
    let failure = opening
        .await
        .err()
        .ok_or("cancelled late owner was admitted")?;
    assert!(!failure.host_retirement_confirmed());
    assert_eq!(
        failure.error.message,
        TransportError::BluetoothOpenInterrupted.to_string(),
        "late close failure cannot replace cancellation"
    );
    let close_error = failure
        .close_error
        .ok_or("late close failure disappeared")?;
    assert!(
        close_error
            .causes
            .iter()
            .any(|cause| cause == "scripted late-open close failure"),
        "retain independent close cause"
    );
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn late_close_timeout_remains_independent_and_still_drops_owner() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let lifecycle = Arc::new(Lifecycle::default());
    let (worker, control) = delayed_worker(Arc::clone(&lifecycle), CloseBehavior::Pending);
    let opening = open_worker(&cancelled, TEST_BOUND, worker);
    tokio::pin!(opening);
    let (waker, completed) = completion_waker();
    assert!(
        poll_once(opening.as_mut(), &waker).is_pending(),
        "worker starts pending"
    );
    control.release.send(())?;
    completed.recv_timeout(TEST_BOUND)?;
    cancelled.store(true, Ordering::SeqCst);
    let failure = opening
        .await
        .err()
        .ok_or("cancelled late owner was admitted")?;
    assert_eq!(
        failure.error.message,
        TransportError::BluetoothOpenInterrupted.to_string(),
        "late close timeout cannot replace cancellation"
    );
    let close_error = failure
        .close_error
        .ok_or("bounded close timeout disappeared")?;
    assert!(
        close_error.message.contains("deadline"),
        "retain close deadline separately"
    );
    assert_released_once(&lifecycle);
    Ok(())
}

#[tokio::test]
async fn worker_error_preserves_its_cause_without_inventing_a_close() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let result = open_worker::<ObservedConnection, _>(&cancelled, TEST_BOUND, |_cancellation| {
        Err(TransportError::Open {
            path: "synthetic native endpoint".to_owned(),
            source: io::Error::other("scripted open failure"),
        })
    })
    .await;
    let failure = result.err().ok_or("worker error became success")?;
    assert!(
        failure
            .error
            .causes
            .iter()
            .any(|cause| cause == "scripted open failure"),
        "retain worker cause"
    );
    assert!(failure.close_error.is_none(), "no successful owner exists");
    Ok(())
}

#[tokio::test]
async fn expired_worker_error_cannot_authorize_a_selected_retry() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let result = open_worker::<ObservedConnection, _>(
        &cancelled,
        Duration::from_millis(10),
        |cancellation| {
            let deadline = std::time::Instant::now() + TEST_BOUND;
            while !cancellation.is_cancelled() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(TransportError::BluetoothOpenWithCleanup {
                stage: BluetoothOpenStage::RfcommDeadline,
                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
            })
        },
    )
    .await;
    let failure = result.err().ok_or("expired error was admitted")?;
    assert!(
        failure.host_retirement_confirmed(),
        "outer deadline preserves the independently reaped helper"
    );
    assert!(
        !failure.retry_allowed(),
        "outer deadline overrides native eligibility"
    );
    assert!(
        failure
            .error
            .causes
            .iter()
            .any(|cause| cause.contains("dispatch budget"))
    );
    assert!(
        failure
            .error
            .causes
            .iter()
            .any(|cause| cause.contains("RFCOMM opening deadline")
                && cause.contains("ChannelUnconfirmed"))
    );
    assert!(failure.close_error.is_none(), "no successful owner exists");
    Ok(())
}

#[tokio::test]
async fn interrupted_worker_cannot_erase_pending_host_retirement() -> TestResult {
    for cancel in [false, true] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_signal = Arc::clone(&cancelled);
        let failure = open_worker::<ObservedConnection, _>(
            &cancelled,
            Duration::from_millis(10),
            move |cancellation| {
                if cancel {
                    worker_signal.store(true, Ordering::Release);
                }
                let bound = std::time::Instant::now() + TEST_BOUND;
                while !cancellation.is_cancelled() && std::time::Instant::now() < bound {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(TransportError::BluetoothClose {
                    failure: BluetoothCloseFailure::ReapPending,
                })
            },
        )
        .await
        .err()
        .ok_or("pending helper became an owner")?;
        assert!(!failure.host_retirement_confirmed());
        assert!(!failure.retry_allowed());
        assert!(
            failure
                .error
                .causes
                .iter()
                .any(|cause| cause.contains("ReapPending"))
        );
        assert_eq!(
            serde_json::to_value(&failure)?.get("host_retirement_confirmed"),
            Some(&serde_json::Value::Bool(false))
        );
    }
    Ok(())
}

#[tokio::test]
async fn worker_join_failure_is_an_open_failure_without_a_fake_close() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let result = open_worker::<ObservedConnection, _>(&cancelled, TEST_BOUND, |_cancellation| {
        std::panic::resume_unwind(Box::new("scripted native worker unwind"))
    })
    .await;
    let failure = result.err().ok_or("worker panic became success")?;
    assert!(
        failure.error.message.contains("panicked"),
        "retain the failed join"
    );
    assert!(failure.close_error.is_none(), "no successful owner exists");
    Ok(())
}

#[test]
fn endpoint_requires_one_exact_typed_address() -> TestResult {
    let endpoint = Endpoint {
        address: "00:aa:bb:cc:dd:55".parse()?,
        helper: None,
    };
    assert_eq!(
        endpoint.address.as_str(),
        "00-AA-BB-CC-DD-55",
        "canonical address"
    );
    for invalid in [
        "TM-D750",
        "/dev/cu.TM-D750",
        "00-AA:BB-CC-DD-55",
        "00-AA-BB-CC-DD-GG",
    ] {
        assert!(
            invalid.parse::<BluetoothAddress>().is_err(),
            "reject non-address {invalid:?}"
        );
    }
    Ok(())
}
