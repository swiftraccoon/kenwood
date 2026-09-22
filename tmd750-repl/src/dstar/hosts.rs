//! The REPL's implementations of the library Terminal-lifecycle host traits.
//!
//! `kenwood_tmd750::TerminalLifecycle` owns the protocol; these types wire it
//! to this crate's capture transcripts, native Bluetooth backend, and recovery
//! journal. `ControlHostImpl` opens the USB CAT endpoint with a per-stage
//! recorder, `ModemHostImpl` opens and reopens the Bluetooth link, and the
//! recovery `Journal` records the durable backup, plan, intents and checkpoints.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use kenwood_tmd750::radio::readiness::{CloseFailure, ControlHost, ControlStage};
use kenwood_tmd750::radio::terminal::TerminalPlan;
use kenwood_tmd750::radio::terminal::session::{TerminalJournal, TerminalPhase};
use kenwood_tmd750::radio::terminal::transition::{ModemHost, ModemOpenFailure};
use kenwood_tmd750::transport::{SerialCandidate, open_serial};
use kenwood_tmd750::{Identity, MenuFieldSnapshot, PageReplacement};
use kenwood_transport::bluetooth::{BluetoothService, RfcommChannel};
use serde::Serialize;
use tokio::time::Instant as TokioInstant;

use crate::capture::{CaptureTransport, Failure, Recorder, TranscriptSummary, create_private_file};
use crate::connection::Connection;
use crate::native::{self, Endpoint, opening};

use super::startup::journal::{Journal, Phase};

/// A single control-endpoint open, for the startup report.
#[derive(Debug, Serialize)]
pub(super) struct ControlOpen {
    stage: &'static str,
    transcript: TranscriptSummary,
    close_error: Option<Failure>,
}

/// Serial control endpoint with per-stage capture, wired to [`ControlHost`].
pub(super) struct ControlHostImpl {
    directory: PathBuf,
    cancelled: Arc<AtomicBool>,
    started: Instant,
    stage: ControlStage,
    opens: u32,
    records: Vec<ControlOpen>,
}

impl ControlHostImpl {
    pub(super) fn new(directory: &Path, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            directory: directory.to_owned(),
            cancelled,
            started: Instant::now(),
            stage: ControlStage::Preflight,
            opens: 0,
            records: Vec::new(),
        }
    }

    /// Every control open this host performed, for the report.
    pub(super) fn records(&self) -> &[ControlOpen] {
        &self.records
    }

    /// Whether every control connection this host opened was closed cleanly.
    pub(super) fn released(&self) -> bool {
        self.records
            .iter()
            .all(|record| record.close_error.is_none() && record.transcript.complete)
    }
}

const fn stage_label(stage: ControlStage) -> &'static str {
    match stage {
        ControlStage::Preflight => "control-preflight",
        ControlStage::ActiveEntry => "control-active-entry",
        ControlStage::ReadinessBeforeRestore => "control-readiness-before",
        ControlStage::Restore => "control-restore",
        ControlStage::ReadinessAfterRestore => "control-readiness-after",
        ControlStage::Verification => "control-verification",
        _ => "control",
    }
}

impl ControlHost for ControlHostImpl {
    type Connection = CaptureTransport<Connection, File>;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, kenwood_transport::TransportError> {
        kenwood_tmd750::transport::discover_serial()
    }

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, kenwood_transport::TransportError> {
        let label = stage_label(self.stage);
        self.opens += 1;
        let path = self.directory.join(format!("{label}-{}.jsonl", self.opens));
        let file = create_private_file(&path).map_err(kenwood_transport::TransportError::Read)?;
        let recorder = Recorder::named(file, Arc::clone(&self.cancelled), label);
        let serial = open_serial(&endpoint.path, baud)?;
        Ok(CaptureTransport::required(
            Connection::Serial(serial),
            recorder,
        ))
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    async fn wait(&mut self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    async fn close(&mut self, mut connection: Self::Connection) -> Result<(), CloseFailure> {
        let close = native::close(&mut connection).await;
        let mut recorder = connection.into_recorder();
        let sync = recorder
            .synchronize()
            .err()
            .map(|error| Failure::from_error(&error));
        let close_error = close.or(sync);
        self.records.push(ControlOpen {
            stage: stage_label(self.stage),
            transcript: recorder.summary(),
            close_error: close_error.clone(),
        });
        close_error.map_or(Ok(()), |failure| {
            Err(CloseFailure::Host(failure.to_string().into()))
        })
    }

    fn stage(&mut self, stage: ControlStage) {
        self.stage = stage;
    }
}

/// Bluetooth modem link with capture, wired to [`ModemHost`].
pub(super) struct ModemHostImpl {
    backend: native::SystemBackend,
    endpoint: Endpoint,
    directory: PathBuf,
    cancelled: Arc<AtomicBool>,
    channel: Option<RfcommChannel>,
    opens: u32,
    openings: Vec<opening::History>,
    retirements: Vec<TranscriptSummary>,
}

impl ModemHostImpl {
    pub(super) fn new(endpoint: Endpoint, directory: &Path, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            backend: native::SystemBackend,
            endpoint,
            directory: directory.to_owned(),
            cancelled,
            channel: None,
            opens: 0,
            openings: Vec::new(),
            retirements: Vec::new(),
        }
    }

    /// Every Bluetooth opening attempt this host made, for the report.
    pub(super) fn openings(&self) -> &[opening::History] {
        &self.openings
    }

    fn recorder(&mut self, label: &'static str) -> Result<Recorder<File>, ModemOpenFailure> {
        self.opens += 1;
        let path = self.directory.join(format!("modem-{}.jsonl", self.opens));
        let file = create_private_file(&path).map_err(|error| ModemOpenFailure {
            source: Box::new(error),
            retry_allowed: false,
            released: true,
        })?;
        Ok(Recorder::named(file, Arc::clone(&self.cancelled), label))
    }

    async fn open_service(
        &mut self,
        service: BluetoothService,
        cancelled: &AtomicBool,
    ) -> Result<CaptureTransport<Connection, File>, ModemOpenFailure> {
        let recorder = self.recorder("modem")?;
        let selected = opening::open_selected(
            &mut self.backend,
            &self.endpoint,
            service,
            recorder,
            cancelled,
        )
        .await;
        let history = selected.history;
        let released = history.attempts.last().is_none_or(|attempt| {
            attempt
                .error
                .as_ref()
                .is_none_or(native::OpenFailure::host_retirement_confirmed)
        });
        let retry_allowed = history_retry_allowed(&history);
        self.openings.push(history);
        match selected.opened {
            Some(owner) => {
                self.channel = Some(owner.channel);
                Ok(owner.transport)
            }
            None => Err(ModemOpenFailure {
                source: "Bluetooth opening failed; the attempts are in the startup report".into(),
                retry_allowed,
                released,
            }),
        }
    }
}

fn history_retry_allowed(history: &opening::History) -> bool {
    history.capture_error.is_none()
        && history.retry_error.is_none()
        && history.transcript.complete
        && history.attempts.last().is_some_and(|attempt| {
            attempt.started
                && attempt.interruption.is_none()
                && attempt
                    .error
                    .as_ref()
                    .is_some_and(native::OpenFailure::retry_allowed)
        })
}

/// Bounds one native open to a deadline by forwarding it to `open_until`.
struct Deadline<'a> {
    backend: &'a mut native::SystemBackend,
    deadline: TokioInstant,
}

impl native::Backend for Deadline<'_> {
    type Connection = Connection;

    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
    ) -> Result<native::Opened<Connection>, native::OpenFailure> {
        self.backend
            .open_until(endpoint, service, cancelled, self.deadline)
            .await
    }

    async fn wait(&mut self, duration: Duration) {
        self.backend
            .wait(duration.min(self.deadline.saturating_duration_since(TokioInstant::now())))
            .await;
    }
}

impl ModemHost for ModemHostImpl {
    type Connection = CaptureTransport<Connection, File>;

    async fn open(&mut self, cancelled: &AtomicBool) -> Result<Self::Connection, ModemOpenFailure> {
        self.open_service(BluetoothService::SerialPort, cancelled)
            .await
    }

    async fn reopen(
        &mut self,
        deadline: TokioInstant,
        cancelled: &AtomicBool,
    ) -> Result<Self::Connection, ModemOpenFailure> {
        let Some(channel) = self.channel else {
            return Err(ModemOpenFailure {
                source: "no RFCOMM channel from the first open to reopen".into(),
                retry_allowed: false,
                released: true,
            });
        };
        let recorder = self.recorder("modem-reopen")?;
        let selected = opening::open_selected(
            &mut Deadline {
                backend: &mut self.backend,
                deadline,
            },
            &self.endpoint,
            BluetoothService::FixedChannel(channel),
            recorder,
            cancelled,
        )
        .await;
        let history = selected.history;
        let retry_allowed = history_retry_allowed(&history);
        let released = history.attempts.last().is_none_or(|attempt| {
            attempt
                .error
                .as_ref()
                .is_none_or(native::OpenFailure::host_retirement_confirmed)
        });
        self.openings.push(history);
        selected
            .opened
            .map(|owner| owner.transport)
            .ok_or(ModemOpenFailure {
                source: "pinned Bluetooth reopen failed; the attempts are in the startup report"
                    .into(),
                retry_allowed,
                released,
            })
    }

    async fn wait(&mut self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    async fn retire(&mut self, connection: Self::Connection) -> Result<(), CloseFailure> {
        let mut connection = connection;
        let close = native::close(&mut connection).await;
        let mut recorder = connection.into_recorder();
        let sync = recorder
            .synchronize()
            .err()
            .map(|error| Failure::from_error(&error));
        self.retirements.push(recorder.summary());
        close.or(sync).map_or(Ok(()), |failure| {
            Err(CloseFailure::Host(failure.to_string().into()))
        })
    }
}

impl TerminalJournal for Journal {
    fn backup(&mut self, identity: &Identity, snapshot: &MenuFieldSnapshot) -> std::io::Result<()> {
        self.record_backup(identity, snapshot)
    }

    fn planned(&mut self, plan: &TerminalPlan) -> std::io::Result<()> {
        self.record_planned(plan)
    }

    fn before_write(
        &mut self,
        phase: TerminalPhase,
        page: &PageReplacement,
    ) -> std::io::Result<()> {
        self.record_before_write(map_phase(phase), page)
    }

    fn checkpoint(
        &mut self,
        phase: TerminalPhase,
        acknowledged_exit: bool,
        journal: &kenwood_tmd750::radio::programming::McpJournal,
    ) -> std::io::Result<()> {
        self.record_checkpoint(map_phase(phase), acknowledged_exit, journal)
    }
}

const fn map_phase(phase: TerminalPhase) -> Phase {
    match phase {
        TerminalPhase::Entry => Phase::Entry,
        TerminalPhase::Restore => Phase::Restore,
    }
}
