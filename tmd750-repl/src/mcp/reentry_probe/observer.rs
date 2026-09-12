//! Passive, timestamped macOS registry observations, never radio protocol I/O.
//!
//! The output describes host registry objects, not physical radio identity or
//! firmware readiness. Sampling can miss short detachments. Each command has
//! its own deadline; a timed-out child is killed on drop, but its termination
//! and reaping are not independently established by that observation.

use std::fs::File;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::mcp::Failure;
use crate::mcp::capture::{Recorder, TranscriptSummary};

const PROGRAM: &str = "/usr/sbin/ioreg";
const ARGUMENTS: [&str; 6] = ["-r", "-c", "IOSerialBSDClient", "-l", "-w", "0"];

#[derive(Clone, Copy, Debug)]
struct Timing {
    post_sample_wait: Duration,
    command_timeout: Duration,
}

const TIMING: Timing = Timing {
    post_sample_wait: Duration::from_millis(50),
    command_timeout: Duration::from_secs(2),
};

/// Exact command output; nonzero or missing exit codes never mean absence.
#[derive(Debug, Serialize)]
struct CommandOutput {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Replace only the passive command when testing; there is no radio dependency.
trait Source: Send + 'static {
    fn sample(&mut self) -> impl Future<Output = io::Result<CommandOutput>> + Send;
}

#[derive(Debug)]
struct SystemSource;

#[cfg(target_os = "macos")]
fn command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(PROGRAM);
    let _configured = command
        .args(ARGUMENTS)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}

impl Source for SystemSource {
    async fn sample(&mut self) -> io::Result<CommandOutput> {
        #[cfg(target_os = "macos")]
        {
            let output = command().output().await?;
            Ok(CommandOutput {
                exit_code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "passive serial-registry observation requires macOS",
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureStage {
    UnsupportedPlatform,
    CancelledBeforeStart,
    Command,
    CommandTimeout,
    CommandExit,
    Capture,
    Worker,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    /// Initial evidence only, retained if the worker cannot return its summary.
    Running,
    /// The stop request was consumed and the final transcript synchronized.
    Stopped,
    Failed {
        stage: FailureStage,
        error: Box<Failure>,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event<'a> {
    SampleRequested {
        sample: u64,
        program: &'static str,
        arguments: &'static [&'static str],
    },
    SampleCompleted {
        sample: u64,
        output: &'a CommandOutput,
    },
    SampleFailed {
        sample: u64,
        stage: FailureStage,
        error: &'a Failure,
        output: Option<&'a CommandOutput>,
    },
    StartRefused {
        stage: FailureStage,
        error: &'a Failure,
    },
    StopRequested,
}

/// Completion evidence for the passive observer, separate from radio success.
#[derive(Debug, Serialize)]
pub(super) struct Summary {
    transcript: Box<TranscriptSummary>,
    /// Counts refer only to the initial snapshot if the worker fails to join.
    requested_samples: u64,
    successful_samples: u64,
    /// False means final capture state could not be recovered from the worker.
    #[serde(rename = "final_capture_summary")]
    final_capture: bool,
    outcome: Outcome,
    synchronization_error: Option<Box<Failure>>,
}

impl Summary {
    /// A stopped observer must retain at least one successful durable sample.
    pub(super) fn succeeded(&self) -> bool {
        self.final_capture
            && self.transcript.complete
            && self.successful_samples > 0
            && self.synchronization_error.is_none()
            && matches!(self.outcome, Outcome::Stopped)
    }
}

struct State {
    recorder: Recorder<File>,
    cancelled: Arc<AtomicBool>,
    requested_samples: u64,
    successful_samples: u64,
    outcome: Outcome,
    synchronization_error: Option<Failure>,
}

impl State {
    const fn new(recorder: Recorder<File>, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            recorder,
            cancelled,
            requested_samples: 0,
            successful_samples: 0,
            outcome: Outcome::Running,
            synchronization_error: None,
        }
    }

    fn fail(&mut self, stage: FailureStage, error: Failure) {
        if !matches!(self.outcome, Outcome::Failed { .. }) {
            self.outcome = Outcome::Failed {
                stage,
                error: Box::new(error),
            };
        }
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn capture_ready(&mut self) -> bool {
        if let Err(error) = self.recorder.ensure_complete() {
            self.fail(FailureStage::Capture, Failure::from_error(&error));
            return false;
        }
        true
    }

    fn synchronize(&mut self) -> bool {
        if let Err(error) = self.recorder.synchronize() {
            let failure = Failure::from_error(&error);
            if self.synchronization_error.is_none() {
                self.synchronization_error = Some(failure.clone());
            }
            self.fail(FailureStage::Capture, failure);
            return false;
        }
        true
    }

    fn snapshot(&self) -> Summary {
        Summary {
            transcript: Box::new(self.recorder.summary()),
            requested_samples: self.requested_samples,
            successful_samples: self.successful_samples,
            final_capture: false,
            outcome: Outcome::Running,
            synchronization_error: None,
        }
    }

    fn finish(mut self) -> Summary {
        let _synchronized = self.synchronize();
        Summary {
            transcript: Box::new(self.recorder.summary()),
            requested_samples: self.requested_samples,
            successful_samples: self.successful_samples,
            final_capture: true,
            outcome: self.outcome,
            synchronization_error: self.synchronization_error.map(Box::new),
        }
    }

    fn refuse(mut self, stage: FailureStage, message: &str) -> Summary {
        let error = Failure::from_error(&io::Error::other(message.to_owned()));
        self.recorder.record(Event::StartRefused {
            stage,
            error: &error,
        });
        self.fail(stage, error);
        self.finish()
    }

    fn sample_failed(
        &mut self,
        sample: u64,
        stage: FailureStage,
        error: Failure,
        output: Option<&CommandOutput>,
    ) {
        self.recorder.record(Event::SampleFailed {
            sample,
            stage,
            error: &error,
            output,
        });
        self.fail(stage, error);
        let _captured = self.capture_ready();
    }

    async fn sample(&mut self, source: &mut impl Source, timeout: Duration) -> bool {
        let sample = self.requested_samples;
        self.recorder.record(Event::SampleRequested {
            sample,
            program: PROGRAM,
            arguments: &ARGUMENTS,
        });
        if !self.capture_ready() {
            return false;
        }
        self.requested_samples += 1;
        let output = match tokio::time::timeout(timeout, source.sample()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                self.sample_failed(
                    sample,
                    FailureStage::Command,
                    Failure::from_error(&error),
                    None,
                );
                return false;
            }
            Err(error) => {
                self.sample_failed(
                    sample,
                    FailureStage::CommandTimeout,
                    Failure::from_error(&error),
                    None,
                );
                return false;
            }
        };
        if output.exit_code != Some(0) {
            let error = io::Error::other(format!(
                "passive registry command failed with exit code {:?}",
                output.exit_code
            ));
            self.sample_failed(
                sample,
                FailureStage::CommandExit,
                Failure::from_error(&error),
                Some(&output),
            );
            return false;
        }
        self.recorder.record(Event::SampleCompleted {
            sample,
            output: &output,
        });
        if !self.capture_ready() {
            return false;
        }
        self.successful_samples += 1;
        true
    }
}

/// Fail closed on worker loss, including an abort before its first poll.
struct WorkerGuard {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

/// One independent observer; stopping it never cancels a radio exchange.
#[derive(Debug)]
pub(super) struct Observer {
    stop: oneshot::Sender<()>,
    worker: JoinHandle<Summary>,
    initial_summary: Summary,
    cancelled: Arc<AtomicBool>,
}

impl Observer {
    /// Require a complete synchronized first observation before returning.
    /// Non-macOS hosts are rejected without starting a worker or a subprocess.
    pub(super) async fn start(
        recorder: Recorder<File>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, Summary> {
        if !cfg!(target_os = "macos") {
            return Err(State::new(recorder, cancelled).refuse(
                FailureStage::UnsupportedPlatform,
                "passive serial-registry observation requires macOS",
            ));
        }
        Self::start_with(recorder, cancelled, SystemSource, TIMING).await
    }

    async fn start_with(
        recorder: Recorder<File>,
        cancelled: Arc<AtomicBool>,
        mut source: impl Source,
        timing: Timing,
    ) -> Result<Self, Summary> {
        let mut state = State::new(recorder, Arc::clone(&cancelled));
        if cancelled.load(Ordering::Relaxed) {
            return Err(state.refuse(
                FailureStage::CancelledBeforeStart,
                "passive observer start was cancelled",
            ));
        }
        if !state.sample(&mut source, timing.command_timeout).await || !state.synchronize() {
            return Err(state.finish());
        }
        let initial_summary = state.snapshot();
        let (stop, stopped) = oneshot::channel();
        let guard = WorkerGuard {
            cancelled: Arc::clone(&cancelled),
            armed: true,
        };
        let worker = tokio::spawn(observe(state, source, timing, stopped, guard));
        Ok(Self {
            stop,
            worker,
            initial_summary,
            cancelled,
        })
    }

    /// Finish the current bounded passive sample, join, and synchronize evidence.
    pub(super) async fn stop(self) -> Summary {
        let _requested = self.stop.send(());
        match self.worker.await {
            Ok(summary) => summary,
            Err(error) => {
                self.cancelled.store(true, Ordering::Relaxed);
                let mut summary = self.initial_summary;
                summary.transcript.complete = false;
                summary.outcome = Outcome::Failed {
                    stage: FailureStage::Worker,
                    error: Box::new(Failure::from_error(&error)),
                };
                summary
            }
        }
    }
}

async fn observe(
    mut state: State,
    mut source: impl Source,
    timing: Timing,
    mut stopped: oneshot::Receiver<()>,
    mut guard: WorkerGuard,
) -> Summary {
    loop {
        tokio::select! {
            biased;
            _ = &mut stopped => {
                state.recorder.record(Event::StopRequested);
                state.outcome = Outcome::Stopped;
                break;
            }
            () = tokio::time::sleep(timing.post_sample_wait) => {}
        }
        if !state.sample(&mut source, timing.command_timeout).await {
            break;
        }
    }
    let summary = state.finish();
    guard.armed = false;
    summary
}

#[cfg(test)]
mod tests;
