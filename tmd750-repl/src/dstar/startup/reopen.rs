//! Reopen the same Bluetooth address on its original RFCOMM channel, inside
//! the Terminal transition window.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use kenwood_transport::bluetooth::{BluetoothService, RfcommChannel};
use tokio::time::Instant;

use crate::capture::{CaptureTransport, Failure, Recorder, create_private_file};
use crate::native::{self, Endpoint, OpenFailure, Opened, opening};

use super::super::transition;
use super::control;

#[cfg(test)]
mod tests;

struct Window<'a, B> {
    backend: &'a mut B,
    deadline: Instant,
}

impl<B: native::Backend> native::Backend for Window<'_, B> {
    type Connection = B::Connection;
    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
    ) -> Result<Opened<Self::Connection>, OpenFailure> {
        if Instant::now() >= self.deadline {
            return Err(OpenFailure::before_open(&std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Terminal opening window expired",
            )));
        }
        let result = self
            .backend
            .open_until(endpoint, service, cancelled, self.deadline)
            .await;
        if Instant::now() < self.deadline {
            return result;
        }
        let mut error = OpenFailure::from_error(&std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Terminal opening completed after its deadline",
        ));
        match result {
            Ok(mut owner) => {
                error.record_close(native::close(&mut owner.connection).await);
            }
            Err(cause) => error.retain_open_failure(cause),
        }
        Err(error)
    }
    async fn wait(&mut self, duration: Duration) {
        self.backend
            .wait(duration.min(self.deadline.saturating_duration_since(Instant::now())))
            .await;
    }
}

pub(super) struct Reopen<'a, B> {
    pub(super) backend: &'a mut B,
    pub(super) endpoint: &'a Endpoint,
    pub(super) channel: RfcommChannel,
    pub(super) directory: &'a Path,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) openings: &'a mut Vec<opening::History>,
    pub(super) retirements: &'a mut Vec<control::Retirement>,
}

impl<B: native::Backend> transition::Backend for Reopen<'_, B> {
    type Connection = CaptureTransport<B::Connection, File>;
    async fn reopen(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self::Connection, transition::ReopenFailure> {
        let file = create_private_file(
            &self
                .directory
                .join(format!("reopen-{}.jsonl", self.openings.len() + 1)),
        )
        .map_err(|error| transition::ReopenFailure {
            error: Failure::from_error(&error),
            retry_allowed: false,
        })?;
        let recorder = Recorder::named(file, Arc::clone(&self.cancelled), "bluetooth-reopen");
        let selected = opening::open_selected(
            &mut Window {
                backend: self.backend,
                deadline,
            },
            self.endpoint,
            BluetoothService::FixedChannel(self.channel),
            recorder,
            cancelled,
        )
        .await;
        let retry_allowed = selected.history.capture_error.is_none()
            && selected.history.retry_error.is_none()
            && selected.history.transcript.complete
            && selected.history.attempts.last().is_some_and(|attempt| {
                attempt.started
                    && attempt.interruption.is_none()
                    && attempt
                        .error
                        .as_ref()
                        .is_some_and(OpenFailure::retry_allowed)
            });
        self.openings.push(selected.history);
        selected
            .opened
            .map(|owner| owner.transport)
            .ok_or_else(|| transition::ReopenFailure {
                error: Failure {
                    message: "pinned Bluetooth opening failed; all attempts are retained in the startup report"
                        .to_owned(),
                    causes: Vec::new(),
                },
                retry_allowed,
            })
    }
    async fn wait(&mut self, duration: Duration, cancelled: &AtomicBool) -> Result<(), Failure> {
        super::program::check_cancelled(cancelled).map_err(|error| Failure::from_error(&error))?;
        self.backend.wait(duration).await;
        super::program::check_cancelled(cancelled).map_err(|error| Failure::from_error(&error))
    }

    async fn retire(&mut self, owner: Self::Connection) -> Result<(), Failure> {
        let retirement = control::retire(owner).await;
        let result = if retirement.succeeded() {
            Ok(())
        } else {
            Err(Failure {
                message: "closing the Bluetooth connection failed; its close and capture errors are in the startup report"
                    .to_owned(),
                causes: [
                    retirement.close_error.as_ref(),
                    retirement.capture_error.as_ref(),
                ]
                .into_iter()
                .flatten()
                .map(ToString::to_string)
                .collect(),
            })
        };
        self.retirements.push(retirement);
        result
    }
}
