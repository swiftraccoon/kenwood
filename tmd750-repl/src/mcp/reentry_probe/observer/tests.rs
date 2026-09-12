//! Deterministic passive-command fixtures; no registry or radio queries.

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;

use tokio::sync::Notify;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const TEST_TIMING: Timing = Timing {
    post_sample_wait: Duration::from_secs(60),
    command_timeout: Duration::from_millis(50),
};

enum Step {
    Output(CommandOutput),
    Error,
    Pending(Arc<AtomicBool>),
    Released(oneshot::Receiver<()>),
}

struct SourceFixture {
    steps: VecDeque<Step>,
    calls: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}

impl SourceFixture {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            calls: Arc::new(AtomicUsize::new(0)),
            changed: Arc::new(Notify::new()),
        }
    }
}

struct DropProof(Arc<AtomicBool>);

impl Drop for DropProof {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Source for SourceFixture {
    async fn sample(&mut self) -> io::Result<CommandOutput> {
        let _previous = self.calls.fetch_add(1, Ordering::Relaxed);
        self.changed.notify_one();
        match self.steps.pop_front() {
            Some(Step::Output(output)) => Ok(output),
            Some(Step::Error) => Err(io::Error::other("scripted registry command failure")),
            Some(Step::Pending(dropped)) => {
                let _proof = DropProof(dropped);
                std::future::pending().await
            }
            Some(Step::Released(release)) => {
                release.await.map_err(io::Error::other)?;
                Ok(successful_output())
            }
            None => Err(io::Error::other("unexpected extra passive command")),
        }
    }
}

fn successful_output() -> CommandOutput {
    CommandOutput {
        exit_code: Some(0),
        stdout: b"+-o IOSerialBSDClient <class IOSerialBSDClient, id 0x1000f395f>\n\xff".to_vec(),
        stderr: Vec::new(),
    }
}

fn recorder(
    file: &tempfile::NamedTempFile,
    cancelled: &Arc<AtomicBool>,
) -> Result<Recorder<File>, TestError> {
    Ok(Recorder::named(
        file.reopen()?,
        Arc::clone(cancelled),
        "serial-registry.jsonl",
    ))
}

#[derive(serde::Deserialize)]
struct CapturedRecord {
    sequence: u64,
    utc_unix_nanoseconds: String,
    elapsed_microseconds: u128,
    event: serde_json::Value,
}

fn records(path: &std::path::Path) -> Result<Vec<CapturedRecord>, TestError> {
    let content = std::fs::read_to_string(path)?;
    assert!(
        content.ends_with('\n'),
        "every captured event must be a complete record"
    );
    let records = content
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<CapturedRecord>, _>>()?;
    for (index, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence,
            u64::try_from(index)?,
            "capture sequence must be contiguous"
        );
        let timestamp = record.utc_unix_nanoseconds.parse::<i128>()?;
        assert!(
            timestamp > 0,
            "each observation requires an actual UTC timestamp"
        );
        let _timestamp = time::OffsetDateTime::from_unix_timestamp_nanos(timestamp)?;
    }
    assert!(records.windows(2).all(|pair| {
        matches!(pair, [before, after] if before.elapsed_microseconds <= after.elapsed_microseconds)
    }), "monotonic timestamps must retain observation order");
    Ok(records)
}

fn assert_failed(summary: &Summary, expected: FailureStage) {
    assert!(
        !summary.succeeded(),
        "failed observation cannot qualify the experiment"
    );
    assert!(
        matches!(&summary.outcome, Outcome::Failed { stage, .. } if *stage == expected),
        "preserve the failed passive-observation boundary: {summary:?}"
    );
}

async fn require_start_failure(result: Result<Observer, Summary>) -> Result<Summary, TestError> {
    match result {
        Err(summary) => Ok(summary),
        Ok(observer) => {
            let _finished = observer.stop().await;
            Err("failed initial evidence unexpectedly admitted a worker".into())
        }
    }
}

async fn wait_for_calls(calls: &AtomicUsize, changed: &Notify, expected: usize) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), async {
        while calls.load(Ordering::Relaxed) < expected {
            changed.notified().await;
        }
    })
    .await?;
    Ok(())
}

async fn wait_for_worker_retirement(worker: &JoinHandle<Summary>) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !worker.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn initial_sample_is_complete_before_admission_and_stop_preserves_raw_bytes() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([Step::Output(successful_output())]);
    let calls = Arc::clone(&source.calls);
    let observer = Observer::start_with(
        recorder(&file, &cancelled)?,
        Arc::clone(&cancelled),
        source,
        TEST_TIMING,
    )
    .await
    .map_err(|summary| format!("initial observation failed: {summary:?}"))?;
    let initial = records(file.path())?;
    let [requested, completed] = initial.as_slice() else {
        return Err("both initial observation records must exist before admission".into());
    };
    assert_eq!(
        requested
            .event
            .get("kind")
            .and_then(serde_json::Value::as_str),
        Some("sample_requested")
    );
    assert_eq!(
        requested.event.get("program"),
        Some(&serde_json::json!(PROGRAM))
    );
    assert_eq!(
        requested.event.get("arguments"),
        Some(&serde_json::json!(ARGUMENTS))
    );
    let raw_output: Vec<u8> = serde_json::from_value(
        completed
            .event
            .pointer("/output/stdout")
            .ok_or("raw output missing")?
            .clone(),
    )?;
    assert_eq!(
        raw_output,
        successful_output().stdout,
        "including invalid UTF-8 and registry IDs"
    );
    let summary = observer.stop().await;
    assert!(summary.succeeded(), "{summary:?}");
    assert_eq!(summary.successful_samples, 1);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "stop must interrupt the post-sample wait"
    );
    assert!(
        !cancelled.load(Ordering::Relaxed),
        "normal stop is not workflow cancellation"
    );
    assert_eq!(
        records(file.path())?.len(),
        3,
        "initial request/result and final stop event"
    );
    Ok(())
}

#[tokio::test]
async fn a_successful_empty_listing_is_retained_without_invented_device_state() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([Step::Output(CommandOutput {
        exit_code: Some(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    })]);
    let observer = Observer::start_with(
        recorder(&file, &cancelled)?,
        Arc::clone(&cancelled),
        source,
        TEST_TIMING,
    )
    .await
    .map_err(|summary| format!("empty successful sample was rejected: {summary:?}"))?;
    let summary = observer.stop().await;
    assert!(summary.succeeded(), "{summary:?}");
    assert!(
        !cancelled.load(Ordering::Relaxed),
        "successful empty output must not cancel the workflow"
    );
    let captured = records(file.path())?;
    assert_eq!(
        captured
            .get(1)
            .ok_or("completed empty sample missing")?
            .event
            .pointer("/output/stdout"),
        Some(&serde_json::json!([])),
        "preserve the successful raw output instead of inventing a readiness state"
    );
    Ok(())
}

#[tokio::test]
async fn nonzero_and_missing_exit_codes_remain_failures_with_complete_raw_output() -> TestResult {
    for exit_code in [Some(7), None] {
        let file = tempfile::NamedTempFile::new()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let source = SourceFixture::new([Step::Output(CommandOutput {
            exit_code,
            stdout: Vec::new(),
            stderr: vec![0xFF, b'!'],
        })]);
        let calls = Arc::clone(&source.calls);
        let summary = require_start_failure(
            Observer::start_with(
                recorder(&file, &cancelled)?,
                Arc::clone(&cancelled),
                source,
                TEST_TIMING,
            )
            .await,
        )
        .await?;
        assert_failed(&summary, FailureStage::CommandExit);
        assert_eq!(summary.successful_samples, 0);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(
            cancelled.load(Ordering::Relaxed),
            "unsuccessful command exit must cancel future workflow operations"
        );
        assert!(
            summary.transcript.complete && summary.final_capture,
            "a recorded command failure still requires a complete final capture summary"
        );
        let captured = records(file.path())?;
        let failed = captured.get(1).ok_or("failure event missing")?;
        assert_eq!(
            failed.event.pointer("/output/exit_code"),
            Some(&serde_json::json!(exit_code))
        );
        assert_eq!(
            failed.event.pointer("/output/stderr"),
            Some(&serde_json::json!([255, 33]))
        );
        assert_eq!(
            failed.event.get("kind").and_then(serde_json::Value::as_str),
            Some("sample_failed")
        );
    }
    Ok(())
}

#[tokio::test]
async fn initial_capture_failure_prevents_the_passive_command_and_worker() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([]);
    let calls = Arc::clone(&source.calls);
    let original = Recorder::named(
        File::open(file.path())?,
        Arc::clone(&cancelled),
        "readonly.jsonl",
    );
    let summary = require_start_failure(
        Observer::start_with(original, Arc::clone(&cancelled), source, TEST_TIMING).await,
    )
    .await?;
    assert_failed(&summary, FailureStage::Capture);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(
        !summary.transcript.complete,
        "a read-only recorder must not claim a complete transcript"
    );
    assert!(
        summary.synchronization_error.is_some(),
        "the final synchronization check must retain the capture failure"
    );
    assert!(
        cancelled.load(Ordering::Relaxed),
        "initial capture failure must cancel future workflow operations"
    );
    Ok(())
}

#[tokio::test]
async fn background_command_failure_cancels_future_work_without_an_observer_retry() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([Step::Output(successful_output()), Step::Error]);
    let calls = Arc::clone(&source.calls);
    let changed = Arc::clone(&source.changed);
    let observer = Observer::start_with(
        recorder(&file, &cancelled)?,
        Arc::clone(&cancelled),
        source,
        Timing {
            post_sample_wait: Duration::ZERO,
            ..TEST_TIMING
        },
    )
    .await
    .map_err(|summary| format!("initial observation failed: {summary:?}"))?;
    wait_for_calls(&calls, &changed, 2).await?;
    let summary = observer.stop().await;
    assert_failed(&summary, FailureStage::Command);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(summary.successful_samples, 1);
    assert!(
        cancelled.load(Ordering::Relaxed),
        "background command failure must cancel future workflow operations"
    );
    assert!(
        summary.final_capture && summary.transcript.complete,
        "the joined worker must preserve a complete capture of the command failure"
    );
    let Outcome::Failed { error, .. } = &summary.outcome else {
        return Err("command failure missing".into());
    };
    assert_eq!(error.message, "scripted registry command failure");
    assert_eq!(records(file.path())?.len(), 4);
    Ok(())
}

#[tokio::test]
async fn timeout_drops_only_the_passive_future_and_never_means_absence() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([Step::Pending(Arc::clone(&dropped))]);
    let calls = Arc::clone(&source.calls);
    let summary = require_start_failure(
        Observer::start_with(
            recorder(&file, &cancelled)?,
            Arc::clone(&cancelled),
            source,
            Timing {
                command_timeout: Duration::from_millis(1),
                ..TEST_TIMING
            },
        )
        .await,
    )
    .await?;
    assert_failed(&summary, FailureStage::CommandTimeout);
    assert!(
        dropped.load(Ordering::Relaxed),
        "the command timeout must drop the pending passive future"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(summary.successful_samples, 0);
    assert!(
        cancelled.load(Ordering::Relaxed),
        "observation timeout must cancel future workflow operations"
    );
    let captured = records(file.path())?;
    let failed = captured.get(1).ok_or("timeout event missing")?;
    assert_eq!(failed.event.get("output"), Some(&serde_json::Value::Null));
    Ok(())
}

#[tokio::test]
async fn stop_joins_an_inflight_sample_before_final_capture_synchronization() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let (release, released) = oneshot::channel();
    let source = SourceFixture::new([Step::Output(successful_output()), Step::Released(released)]);
    let calls = Arc::clone(&source.calls);
    let changed = Arc::clone(&source.changed);
    let observer = Observer::start_with(
        recorder(&file, &cancelled)?,
        Arc::clone(&cancelled),
        source,
        Timing {
            post_sample_wait: Duration::ZERO,
            command_timeout: Duration::from_secs(1),
        },
    )
    .await
    .map_err(|summary| format!("initial observation failed: {summary:?}"))?;
    wait_for_calls(&calls, &changed, 2).await?;
    let stopping = tokio::spawn(observer.stop());
    tokio::task::yield_now().await;
    assert!(
        !stopping.is_finished(),
        "stop must join the current sample, not detach it"
    );
    release
        .send(())
        .map_err(|()| "passive sample was abandoned")?;
    let summary = stopping.await?;
    assert!(summary.succeeded(), "{summary:?}");
    assert_eq!(summary.successful_samples, 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert!(
        !cancelled.load(Ordering::Relaxed),
        "joining an in-flight successful sample is a normal stop, not cancellation"
    );
    assert_eq!(records(file.path())?.len(), 5);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn abort_before_first_poll_cancels_before_join_and_leaves_summary_unqualified() -> TestResult
{
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([Step::Output(successful_output())]);
    let calls = Arc::clone(&source.calls);
    let observer = Observer::start_with(
        recorder(&file, &cancelled)?,
        Arc::clone(&cancelled),
        source,
        TEST_TIMING,
    )
    .await
    .map_err(|summary| format!("initial observation failed: {summary:?}"))?;
    // No yield follows spawn on this single-thread runtime before the abort.
    observer.worker.abort();
    wait_for_worker_retirement(&observer.worker).await?;
    assert!(
        cancelled.load(Ordering::Relaxed),
        "abort before the first worker poll must cancel without waiting for stop or join"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the aborted worker must not issue a background passive command"
    );
    let summary = observer.stop().await;
    assert_failed(&summary, FailureStage::Worker);
    assert!(
        !summary.final_capture,
        "worker loss cannot qualify the initial snapshot as a final capture summary"
    );
    assert!(
        !summary.transcript.complete,
        "worker loss leaves the final capture extent unverified"
    );
    assert!(
        cancelled.load(Ordering::Relaxed),
        "a failed worker join must cancel future workflow operations"
    );
    Ok(())
}

#[tokio::test]
async fn abort_during_a_sample_cancels_before_join_and_drops_the_pending_future() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let source = SourceFixture::new([
        Step::Output(successful_output()),
        Step::Pending(Arc::clone(&dropped)),
    ]);
    let calls = Arc::clone(&source.calls);
    let changed = Arc::clone(&source.changed);
    let observer = Observer::start_with(
        recorder(&file, &cancelled)?,
        Arc::clone(&cancelled),
        source,
        Timing {
            post_sample_wait: Duration::ZERO,
            command_timeout: Duration::from_secs(1),
        },
    )
    .await
    .map_err(|summary| format!("initial observation failed: {summary:?}"))?;
    wait_for_calls(&calls, &changed, 2).await?;
    observer.worker.abort();
    wait_for_worker_retirement(&observer.worker).await?;
    assert!(
        cancelled.load(Ordering::Relaxed),
        "loss of an active observer must cancel without waiting for stop or join"
    );
    assert!(
        dropped.load(Ordering::Relaxed),
        "retiring the aborted worker must drop its pending passive command"
    );
    let summary = observer.stop().await;
    assert_failed(&summary, FailureStage::Worker);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "worker loss must not retry the interrupted passive command"
    );
    assert!(
        !summary.final_capture && !summary.transcript.complete,
        "worker loss cannot promote an initial snapshot into complete final evidence"
    );
    Ok(())
}

#[tokio::test]
async fn prior_cancellation_refuses_start_without_a_passive_command() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(true));
    let source = SourceFixture::new([]);
    let calls = Arc::clone(&source.calls);
    let summary = require_start_failure(
        Observer::start_with(
            recorder(&file, &cancelled)?,
            Arc::clone(&cancelled),
            source,
            TEST_TIMING,
        )
        .await,
    )
    .await?;
    assert_failed(&summary, FailureStage::CancelledBeforeStart);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(records(file.path())?.len(), 1);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn system_command_is_fixed_and_kills_on_future_drop_without_spawning() {
    let command = command();
    assert_eq!(
        command.as_std().get_program(),
        std::ffi::OsStr::new(PROGRAM)
    );
    assert_eq!(
        command.as_std().get_args().collect::<Vec<_>>(),
        ARGUMENTS.map(std::ffi::OsStr::new)
    );
    assert!(
        command.get_kill_on_drop(),
        "dropping a timed-out passive subprocess must request its termination"
    );
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn unsupported_platform_returns_a_recorded_failure_without_a_worker() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let summary = require_start_failure(
        Observer::start(recorder(&file, &cancelled)?, Arc::clone(&cancelled)).await,
    )
    .await?;
    assert_failed(&summary, FailureStage::UnsupportedPlatform);
    assert_eq!(summary.requested_samples, 0);
    assert!(
        cancelled.load(Ordering::Relaxed),
        "unsupported passive observation must cancel the workflow before radio admission"
    );
    Ok(())
}
