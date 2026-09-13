//! Captured, bounded native selection with an append-only attempt history.

use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_transport::Transport;
use kenwood_transport::bluetooth::{BluetoothService, RfcommChannel};
use serde::Serialize;
use tokio::time::Instant;

use crate::capture::{CaptureTransport, Failure, Recorder, TranscriptSummary};

use super::{Backend, Endpoint, OPEN_BUDGET, OpenFailure, Opened, Resolved};

#[cfg(test)]
mod tests;

pub(crate) const MAX_ATTEMPTS: u8 = 2;
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(1);

/// A validated endpoint and its exclusively owned, required capture.
pub(crate) struct Captured<T> {
    pub(crate) transport: CaptureTransport<T, File>,
    pub(crate) resolved: Resolved,
    pub(crate) channel: RfcommChannel,
}

/// An intent is distinct from a backend dispatch that actually started.
#[derive(Debug, Serialize)]
pub(crate) struct Attempt {
    pub(crate) number: u8,
    /// False when capture or cancellation refused the recorded intent.
    pub(crate) started: bool,
    /// Returned metadata, including a rejected endpoint when one was observed.
    pub(crate) resolved: Option<Resolved>,
    pub(crate) error: Option<OpenFailure>,
    /// An outer deadline or cancellation cannot erase a native opening error.
    pub(crate) interruption: Option<Failure>,
}

impl Attempt {
    fn record_failure(&mut self, error: OpenFailure, cancelled: &AtomicBool, deadline: Instant) {
        self.interruption = interrupted(cancelled, deadline)
            .err()
            .as_ref()
            .map(|error| Failure::from_error(error));
        self.error = Some(error);
    }

    fn retry_allowed(&self) -> bool {
        self.started
            && self.interruption.is_none()
            && self
                .error
                .as_ref()
                .is_some_and(|error| error.retry_allowed() && error.close_error.is_none())
    }
}

/// Earlier failures remain visible even when a later selected open succeeds.
#[derive(Debug, Serialize)]
pub(crate) struct History {
    pub(crate) attempts: Vec<Attempt>,
    pub(crate) retry_error: Option<Failure>,
    pub(crate) capture_error: Option<Failure>,
    /// Opening-phase prefix at failure or successful owner handoff. Subsequent
    /// protocol observations continue in the same file under the admitted owner.
    pub(crate) transcript: TranscriptSummary,
}

impl History {
    /// Opening acceptance is independent of any later CAT observation.
    pub(crate) fn succeeded(&self) -> bool {
        self.retry_error.is_none()
            && self.capture_error.is_none()
            && self.transcript.complete
            && self.attempts.last().is_some_and(|attempt| {
                attempt.started
                    && attempt.resolved.is_some()
                    && attempt.error.is_none()
                    && attempt.interruption.is_none()
            })
    }
}

/// Failed selection has no transport owner; its evidence is still retained.
pub(crate) struct Selection<T> {
    pub(crate) opened: Option<Captured<T>>,
    pub(crate) history: History,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event<'a> {
    #[serde(rename = "native_open_requested")]
    Requested {
        attempt: u8,
        address: &'a str,
        service: &'static str,
        fixed_channel: Option<u8>,
        open_budget_milliseconds: u128,
    },
    #[serde(rename = "native_open_completed")]
    Completed { attempt: u8, resolved: &'a Resolved },
    #[serde(rename = "native_open_failed")]
    Failed { attempt: u8, error: &'a OpenFailure },
    #[serde(rename = "native_open_retry_wait")]
    RetryWait {
        next_attempt: u8,
        milliseconds: u128,
    },
    #[serde(rename = "native_open_retry_wait_completed")]
    RetryWaitCompleted { next_attempt: u8 },
}

enum Owner<T> {
    Admitted(Captured<T>),
    Retired(Recorder<File>),
}

fn interrupted(cancelled: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native selection cancelled",
        ))
    } else if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native selection opening deadline elapsed",
        ))
    } else {
        Ok(())
    }
}

fn validate_endpoint(
    endpoint: &Endpoint,
    service: BluetoothService,
    resolved: &Resolved,
) -> io::Result<RfcommChannel> {
    let channel = RfcommChannel::new(resolved.rfcomm_channel)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if resolved.address != endpoint.address.as_str()
        || matches!(service, BluetoothService::FixedChannel(expected) if expected != channel)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native helper resolved a different address or requested fixed channel",
        ));
    }
    Ok(channel)
}

async fn admit<T: Transport>(
    opened: Opened<T>,
    recorder: Recorder<File>,
    endpoint: &Endpoint,
    service: BluetoothService,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> (Owner<T>, Option<OpenFailure>) {
    let Opened {
        connection,
        resolved,
    } = opened;
    let mut transport = CaptureTransport::required(connection, recorder);
    let admitted = validate_endpoint(endpoint, service, &resolved).and_then(|channel| {
        interrupted(cancelled, deadline)?;
        transport.synchronize()?;
        interrupted(cancelled, deadline)?;
        Ok(channel)
    });
    match admitted {
        Ok(channel) => (
            Owner::Admitted(Captured {
                transport,
                resolved,
                channel,
            }),
            None,
        ),
        Err(error) => {
            let mut failure = OpenFailure::from_error(&error);
            failure.record_close(super::close(&mut transport).await);
            (Owner::Retired(transport.into_recorder()), Some(failure))
        }
    }
}

async fn attempt<B: Backend>(
    backend: &mut B,
    endpoint: &Endpoint,
    service: BluetoothService,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
    number: u8,
) -> (Owner<B::Connection>, Attempt) {
    let mut evidence = Attempt {
        number,
        started: false,
        resolved: None,
        error: None,
        interruption: None,
    };
    let deadline = Instant::now() + OPEN_BUDGET;
    let (service_name, fixed_channel) = match service {
        BluetoothService::SerialPort => ("serial_port_0x1101", None),
        BluetoothService::FixedChannel(channel) => ("fixed_channel", Some(channel.get())),
    };
    recorder.record(Event::Requested {
        attempt: number,
        address: endpoint.address.as_str(),
        service: service_name,
        fixed_channel,
        open_budget_milliseconds: OPEN_BUDGET.as_millis(),
    });
    if let Err(error) = recorder
        .synchronize()
        .and_then(|()| interrupted(cancelled, deadline))
    {
        evidence.error = Some(OpenFailure::before_open(&error));
        return (Owner::Retired(recorder), evidence);
    }
    evidence.started = true;
    // The backend joins every started worker, including interrupted opens.
    match backend.open(endpoint, service, cancelled).await {
        Ok(opened) => {
            evidence.resolved = Some(opened.resolved.clone());
            recorder.record(Event::Completed {
                attempt: number,
                resolved: &opened.resolved,
            });
            let (owner, error) =
                admit(opened, recorder, endpoint, service, cancelled, deadline).await;
            evidence.error = error;
            (owner, evidence)
        }
        Err(error) => {
            evidence.record_failure(error, cancelled, deadline);
            (Owner::Retired(recorder), evidence)
        }
    }
}

async fn retry_wait(
    backend: &mut impl Backend,
    recorder: &mut Recorder<File>,
    cancelled: &AtomicBool,
    next_attempt: u8,
) -> io::Result<()> {
    recorder.record(Event::RetryWait {
        next_attempt,
        milliseconds: RETRY_DELAY.as_millis(),
    });
    recorder.synchronize()?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native retry wait cancelled",
        ));
    }
    backend.wait(RETRY_DELAY).await;
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native retry wait cancelled",
        ));
    }
    recorder.record(Event::RetryWaitCompleted { next_attempt });
    recorder.synchronize()
}

/// Open one exact selection, with at most one explicitly eligible retry.
///
/// No CAT, MCP, or mode command is sent here. The same recorder survives every
/// failed attempt. An acquired but inadmissible owner is closed and dropped;
/// capture failure, cancellation, or independent cleanup failure forbids retry.
pub(crate) async fn open_selected<B: Backend>(
    backend: &mut B,
    endpoint: &Endpoint,
    service: BluetoothService,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> Selection<B::Connection> {
    let mut history = History {
        attempts: Vec::new(),
        retry_error: None,
        capture_error: None,
        transcript: recorder.summary(),
    };
    for number in 1..=MAX_ATTEMPTS {
        let (owner, evidence) =
            attempt(backend, endpoint, service, recorder, cancelled, number).await;
        let retry = evidence.retry_allowed();
        match owner {
            Owner::Admitted(opened) => {
                history.transcript = opened.transport.transcript_summary();
                history.attempts.push(evidence);
                return Selection {
                    opened: Some(opened),
                    history,
                };
            }
            Owner::Retired(mut recovered) => {
                if let Some(error) = &evidence.error {
                    recovered.record(Event::Failed {
                        attempt: number,
                        error,
                    });
                }
                history.capture_error = recovered
                    .synchronize()
                    .err()
                    .as_ref()
                    .map(|error| Failure::from_error(error));
                history.attempts.push(evidence);
                recorder = recovered;
            }
        }
        if number == MAX_ATTEMPTS
            || !retry
            || history.capture_error.is_some()
            || cancelled.load(Ordering::Relaxed)
        {
            break;
        }
        if let Err(error) = retry_wait(backend, &mut recorder, cancelled, number + 1).await {
            history.retry_error = Some(Failure::from_error(&error));
            history.capture_error = recorder
                .synchronize()
                .err()
                .as_ref()
                .map(|error| Failure::from_error(error));
            break;
        }
    }
    history.transcript = recorder.summary();
    Selection {
        opened: None,
        history,
    }
}
