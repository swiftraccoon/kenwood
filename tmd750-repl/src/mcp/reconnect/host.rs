//! [`ControlHost`] implementations that record the library readiness engine's
//! host calls in a capture transcript.
//!
//! Every enumeration, wait and open is written to the verification transcript
//! before it runs, as the lifecycle events of [`LifecycleEvent`]. A transcript
//! that is no longer complete refuses the next enumeration or open with
//! [`TransportError::Write`] and is reported by [`SettleHost::capture_refused`]
//! or [`RecordingHost::capture_refused`], so the caller names the `capture`
//! stage instead of the step the refusal interrupted.

use std::fs::File;
use std::io;
use std::time::Duration;

use kenwood_tmd750::radio::readiness::{CLOSE_BUDGET, CloseFailure, ControlHost};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_transport::{Transport, TransportError};

use super::{Backend, LifecycleEvent, milliseconds, observed_candidates};
use crate::capture::{CaptureTransport, Failure, Recorder};

/// Record one enumeration of `pinned` in `recorder` around the backend call.
///
/// Sets `capture_refused` and returns [`TransportError::Write`] when the
/// transcript is incomplete before or after the call.
fn record_enumeration(
    backend: &mut impl Backend,
    pinned: &SerialCandidate,
    recorder: &mut Recorder<File>,
    capture_refused: &mut bool,
) -> Result<Vec<SerialCandidate>, TransportError> {
    recorder.record(LifecycleEvent::EnumerationRequested);
    require_complete(recorder, capture_refused)?;
    match backend.enumerate() {
        Ok(candidates) => {
            recorder.record(LifecycleEvent::EnumerationCompleted {
                candidates: &observed_candidates(pinned, &candidates),
            });
            require_complete(recorder, capture_refused)?;
            Ok(candidates)
        }
        Err(error) => {
            recorder.record(LifecycleEvent::EnumerationFailed {
                error: Failure::from_error(&error),
            });
            Err(error)
        }
    }
}

/// Record a wait of `duration` in `recorder` around the backend wait.
async fn record_wait(
    backend: &mut impl Backend,
    recorder: &mut Recorder<File>,
    duration: Duration,
) {
    recorder.record(LifecycleEvent::WaitRequested {
        milliseconds: milliseconds(duration),
    });
    backend.wait(duration).await;
    recorder.record(LifecycleEvent::WaitCompleted);
}

fn require_complete(
    recorder: &Recorder<File>,
    capture_refused: &mut bool,
) -> Result<(), TransportError> {
    recorder.ensure_complete().map_err(|error| {
        *capture_refused = true;
        TransportError::Write(error)
    })
}

/// A host for the library's settle wait over a session transcript the caller
/// keeps.
///
/// The settle wait enumerates and waits only; [`ControlHost::open`] is refused.
pub(super) struct SettleHost<'a, B> {
    backend: &'a mut B,
    pinned: &'a SerialCandidate,
    recorder: &'a mut Recorder<File>,
    capture_refused: bool,
}

impl<'a, B> SettleHost<'a, B> {
    /// Record the settle wait of `pinned` in `recorder`.
    pub(super) const fn new(
        backend: &'a mut B,
        pinned: &'a SerialCandidate,
        recorder: &'a mut Recorder<File>,
    ) -> Self {
        Self {
            backend,
            pinned,
            recorder,
            capture_refused: false,
        }
    }

    /// Whether an incomplete transcript refused an enumeration.
    pub(super) const fn capture_refused(&self) -> bool {
        self.capture_refused
    }
}

impl<B: Backend> ControlHost for SettleHost<'_, B> {
    type Connection = B::Connection;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        record_enumeration(
            self.backend,
            self.pinned,
            self.recorder,
            &mut self.capture_refused,
        )
    }

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        _baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        Err(TransportError::Open {
            path: endpoint.path.clone(),
            source: io::Error::other("a settle wait opens no connection"),
        })
    }

    fn now(&self) -> Duration {
        self.backend.now()
    }

    async fn wait(&mut self, duration: Duration) {
        record_wait(self.backend, self.recorder, duration).await;
    }
}

/// A host for the library's readiness check that owns the verification
/// transcript.
///
/// An opened connection carries the transcript inside its
/// [`CaptureTransport`] until [`ControlHost::close`] hands it back; the close
/// waits [`CLOSE_BUDGET`], drops the connection and synchronizes the
/// transcript, and a synchronization failure is the close's
/// [`CloseFailure::Host`].
pub(super) struct RecordingHost<'a, B> {
    backend: &'a mut B,
    pinned: SerialCandidate,
    recorder: Option<Recorder<File>>,
    capture_refused: bool,
}

impl<'a, B> RecordingHost<'a, B> {
    /// Record the readiness check of `pinned` in `recorder`.
    pub(super) fn new(
        backend: &'a mut B,
        pinned: &SerialCandidate,
        recorder: Recorder<File>,
    ) -> Self {
        Self {
            backend,
            pinned: pinned.clone(),
            recorder: Some(recorder),
            capture_refused: false,
        }
    }

    /// Whether an incomplete transcript refused an enumeration or an open.
    pub(super) const fn capture_refused(&self) -> bool {
        self.capture_refused
    }

    /// The transcript, or `None` while a connection still holds it.
    pub(super) fn into_recorder(self) -> Option<Recorder<File>> {
        self.recorder
    }
}

/// The error for a host call while an open connection holds the transcript.
fn transcript_held() -> TransportError {
    TransportError::Write(io::Error::other(
        "the transcript is held by an open connection",
    ))
}

impl<B: Backend> ControlHost for RecordingHost<'_, B> {
    type Connection = CaptureTransport<B::Connection, File>;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        let recorder = self.recorder.as_mut().ok_or_else(transcript_held)?;
        record_enumeration(
            self.backend,
            &self.pinned,
            recorder,
            &mut self.capture_refused,
        )
    }

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        let recorder = self.recorder.as_mut().ok_or_else(transcript_held)?;
        recorder.record(LifecycleEvent::OpenRequested {
            path: &endpoint.path,
            baud,
        });
        require_complete(recorder, &mut self.capture_refused)?;
        match self.backend.open(endpoint, baud) {
            Ok(connection) => {
                recorder.record(LifecycleEvent::OpenCompleted);
                let recorder = self.recorder.take().ok_or_else(transcript_held)?;
                Ok(CaptureTransport::required(connection, recorder))
            }
            Err(error) => {
                recorder.record(LifecycleEvent::OpenFailed {
                    error: Failure::from_error(&error),
                });
                Err(error)
            }
        }
    }

    fn now(&self) -> Duration {
        self.backend.now()
    }

    async fn wait(&mut self, duration: Duration) {
        match self.recorder.as_mut() {
            Some(recorder) => record_wait(self.backend, recorder, duration).await,
            None => self.backend.wait(duration).await,
        }
    }

    async fn close(&mut self, mut connection: Self::Connection) -> Result<(), CloseFailure> {
        let closed = match tokio::time::timeout(CLOSE_BUDGET, connection.close()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(CloseFailure::Transport(error)),
            Err(_elapsed) => Err(CloseFailure::Timeout {
                budget: CLOSE_BUDGET,
            }),
        };
        let mut recorder = connection.into_recorder();
        let synchronized = recorder.synchronize();
        self.recorder = Some(recorder);
        closed?;
        synchronized.map_err(|error| CloseFailure::Host(Box::new(error)))
    }
}
