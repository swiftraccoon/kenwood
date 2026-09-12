//! Private, synchronized evidence for one fixed Terminal-to-Off experiment.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kenwood_tmd750::Identity;
use kenwood_tmd750::memory::{
    TerminalExitTrial, TerminalExitTrialError, TerminalExitTrialSession, TerminalExitTrialStatus,
};
use kenwood_tmd750::types::{PAGE_SIZE, Page};
use serde::Serialize;
use time::OffsetDateTime;

use super::super::IdentityEvidence;

#[cfg(unix)]
const FILENAME: &str = "terminal-exit-journal.jsonl";

/// Injectable durability boundary; success includes content and metadata sync.
pub(super) trait DurableWrite: Write {
    /// Synchronize the complete file contents and metadata.
    fn synchronize(&mut self) -> io::Result<()>;
    /// Synchronize the containing directory and its parent entry.
    fn synchronize_directory(&mut self) -> io::Result<()>;
}

/// Exclusive file and canonical private directory retained for synchronization.
#[derive(Debug)]
pub(super) struct FileSink {
    file: File,
    directory: PathBuf,
}

impl Write for FileSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl DurableWrite for FileSink {
    fn synchronize(&mut self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn synchronize_directory(&mut self) -> io::Result<()> {
        synchronize_directory(&self.directory)
    }
}

#[cfg(unix)]
fn synchronize_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()?;
    let parent = directory.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal directory has no parent",
        )
    })?;
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn synchronize_directory(_directory: &Path) -> io::Result<()> {
    Err(unsupported_platform())
}

#[cfg(not(unix))]
fn unsupported_platform() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Terminal exit trials require Unix private files and directory synchronization",
    )
}

#[derive(Debug)]
struct StickyError {
    kind: io::ErrorKind,
    message: String,
}

impl StickyError {
    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Unprepared,
    Prepared,
    Intent,
    Finished,
}

#[derive(Debug)]
struct BoundTrial {
    identity: Identity,
    target: Page,
    off: [u8; PAGE_SIZE],
    expected_active: [u8; PAGE_SIZE],
    control: (Page, [u8; PAGE_SIZE]),
    routing: (Page, [u8; PAGE_SIZE]),
}

impl BoundTrial {
    fn new(trial: &TerminalExitTrial) -> Self {
        Self {
            identity: trial.identity().clone(),
            target: trial.page(),
            off: *trial.off_page(),
            expected_active: *trial.expected_active_page(),
            control: (trial.control_page_spec(), *trial.control_page()),
            routing: (trial.routing_page_spec(), *trial.routing_page()),
        }
    }

    fn matches(&self, trial: &TerminalExitTrial) -> bool {
        &self.identity == trial.identity()
            && self.target == trial.page()
            && &self.off == trial.off_page()
            && &self.expected_active == trial.expected_active_page()
            && self.control.0 == trial.control_page_spec()
            && &self.control.1 == trial.control_page()
            && self.routing.0 == trial.routing_page_spec()
            && &self.routing.1 == trial.routing_page()
    }
}

/// One append-only fixed-scope journal, permanently poisoned by its first error.
/// No operation rewrites a record, changes a bound plan, or authorizes RF.
#[derive(Debug)]
pub(super) struct Journal<S: DurableWrite = FileSink> {
    sink: S,
    sequence: u64,
    error: Option<StickyError>,
    stage: Stage,
    bound: Option<BoundTrial>,
    attempts: u8,
}

impl Journal<FileSink> {
    /// Reserve a new mode-0600 file inside an existing mode-0700 real directory.
    /// Reject a final-component directory symlink when checked; existing files
    /// are never overwritten. This does not establish a race-free path lookup.
    #[cfg(unix)]
    pub(super) fn create(directory: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Terminal exit journal requires a mode-0700, non-symlink directory",
            ));
        }
        let directory = directory.canonicalize()?;
        let file = super::super::capture::create_private_file(&directory.join(FILENAME))?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Terminal exit journal requires a mode-0600 file",
            ));
        }
        Ok(Self::new(FileSink { file, directory }))
    }

    /// Refuse platforms without the required private-file durability contract.
    #[cfg(not(unix))]
    pub(super) fn create(_directory: &Path) -> io::Result<Self> {
        Err(unsupported_platform())
    }
}

impl<S: DurableWrite> Journal<S> {
    #[cfg(any(unix, test))]
    const fn new(sink: S) -> Self {
        Self {
            sink,
            sequence: 0,
            error: None,
            stage: Stage::Unprepared,
            bound: None,
            attempts: 0,
        }
    }

    /// Bind all immutable captured bytes and the synthetic active expectation.
    /// The caller establishes baseline provenance and exact-scope approval;
    /// preparation is neither an observed active page nor hardware permission.
    pub(super) fn prepare(&mut self, trial: &TerminalExitTrial, backup: &Path) -> io::Result<()> {
        self.require(
            self.stage == Stage::Unprepared,
            "journal is already prepared",
        )?;
        self.require(
            trial.status() == TerminalExitTrialStatus::NotWritten
                && trial.next_session() == Ok(TerminalExitTrialSession::Apply),
            "journal requires an untouched Terminal exit trial",
        )?;
        self.append(Kind::Prepared, &Prepared {
            scope: Scope::new(trial),
            backup,
            active_page_provenance: "synthetic expectation derived from the complete captured Off page; not an observed active-state baseline",
            approval_provenance: "caller must separately establish approval for this exact single Terminal-to-Off trial",
            layout_qualification: "unqualified",
        })?;
        self.bound = Some(BoundTrial::new(trial));
        self.stage = Stage::Prepared;
        Ok(())
    }

    /// Synchronize the sole intent after accepted fresh format and exact target,
    /// control, and routing comparisons, before pure intent acceptance or W.
    /// Raw read evidence remains in the separate session capture; this callback
    /// attests that comparison rather than independently authenticating it.
    pub(super) fn intent(&mut self, trial: &TerminalExitTrial) -> io::Result<()> {
        self.check_bound(trial)?;
        self.require(
            self.stage == Stage::Prepared && self.attempts == 0,
            "journal permits one intent before any session evidence",
        )?;
        self.require(
            trial.status() == TerminalExitTrialStatus::NotWritten
                && !trial.is_halted()
                && trial.next_session() == Err(TerminalExitTrialError::UnexpectedEvent),
            "journal intent requires the accepted fresh apply comparison boundary",
        )?;
        self.append(Kind::WriteIntent, &WriteIntent {
            scope: Scope::new(trial),
            session_id: 1,
            intent_id: 1,
            memory_format: 0,
            pre_entry_gateway: 2,
            desired_gateway: 0,
            active_page_provenance: "caller-attested complete fresh active page matched the immutable expectation; actual raw reads remain in the session capture",
            status: Status::PossiblyChanged,
        })?;
        self.stage = Stage::Intent;
        Ok(())
    }

    /// Synchronize one completed or failed attempt's diagnostics. The ordinal
    /// records append order, not independently proven session or device identity.
    /// At most two attempts are retained; without an intent only the first
    /// failed attempt is permitted, and no later intent may follow it.
    pub(super) fn evidence(&mut self, evidence: &impl Serialize) -> io::Result<()> {
        self.require(
            (self.stage == Stage::Prepared && self.attempts == 0)
                || (self.stage == Stage::Intent && self.attempts < 2),
            "journal session evidence is out of order or exceeds the two-session bound",
        )?;
        let attempt = self.attempts + 1;
        self.append(
            Kind::SessionEvidence,
            &SessionEvidence {
                attempt,
                details: evidence,
            },
        )?;
        self.attempts = attempt;
        Ok(())
    }

    /// Record the bound engine's terminal outcome without inferring success
    /// from process exit or immediate readback. Failures must first halt the
    /// engine. Verified Off requires the intent and both evidence records.
    pub(super) fn finish(&mut self, trial: &TerminalExitTrial) -> io::Result<()> {
        self.check_bound(trial)?;
        self.require(self.stage != Stage::Finished, "journal is already finished")?;
        self.require(
            trial.next_session() == Err(TerminalExitTrialError::TerminalState),
            "journal finish requires a halted or completed engine",
        )?;
        let consistent = match trial.status() {
            TerminalExitTrialStatus::NotWritten => {
                self.stage == Stage::Prepared && self.attempts <= 1
            }
            TerminalExitTrialStatus::PossiblyChanged => {
                self.stage == Stage::Intent && (1..=2).contains(&self.attempts)
            }
            TerminalExitTrialStatus::OffVerifiedAcrossSessions => {
                self.stage == Stage::Intent && self.attempts == 2
            }
        };
        self.require(
            consistent,
            "final engine status disagrees with durable intent or attempt evidence",
        )?;
        self.append(
            Kind::Finished,
            &Finished {
                status: Status::from(trial.status()),
                further_recovery_assessment_required: trial.status()
                    == TerminalExitTrialStatus::PossiblyChanged,
                attempted_sessions: self.attempts,
            },
        )?;
        self.stage = Stage::Finished;
        Ok(())
    }

    fn check_bound(&mut self, trial: &TerminalExitTrial) -> io::Result<()> {
        self.require(
            self.bound
                .as_ref()
                .is_some_and(|bound| bound.matches(trial)),
            "journal trial differs from its immutable prepared scope",
        )
    }

    fn require(&mut self, condition: bool, message: &'static str) -> io::Result<()> {
        self.healthy()?;
        self.remember(if condition {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidInput, message))
        })
    }

    fn healthy(&self) -> io::Result<()> {
        self.error
            .as_ref()
            .map_or(Ok(()), |error| Err(error.error()))
    }

    fn remember(&mut self, result: io::Result<()>) -> io::Result<()> {
        if let Err(error) = result
            && self.error.is_none()
        {
            self.error = Some(StickyError {
                kind: error.kind(),
                message: error.to_string(),
            });
        }
        self.healthy()
    }

    fn append(&mut self, kind: Kind, evidence: &impl Serialize) -> io::Result<()> {
        self.healthy()?;
        let result = self.write_record(kind, evidence);
        self.remember(result)?;
        self.sequence += 1;
        Ok(())
    }

    fn write_record(&mut self, kind: Kind, evidence: &impl Serialize) -> io::Result<()> {
        let record = Record {
            format_version: 1,
            sequence: self.sequence,
            utc_unix_nanoseconds: OffsetDateTime::now_utc().unix_timestamp_nanos().to_string(),
            kind,
            evidence,
        };
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        self.sink.write_all(&bytes)?;
        self.sink.flush()?;
        self.sink.synchronize()?;
        self.sink.synchronize_directory()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Prepared,
    WriteIntent,
    SessionEvidence,
    Finished,
}

#[derive(Debug, Serialize)]
struct Record<'a, T> {
    format_version: u8,
    sequence: u64,
    utc_unix_nanoseconds: String,
    kind: Kind,
    evidence: &'a T,
}

#[derive(Debug, Serialize)]
struct PageEvidence<'a> {
    address: u32,
    length: usize,
    data: &'a [u8],
}

impl<'a> PageEvidence<'a> {
    const fn new(page: Page, data: &'a [u8]) -> Self {
        Self {
            address: page.address().as_u32(),
            length: page.len(),
            data,
        }
    }
}

#[derive(Debug, Serialize)]
struct Scope<'a> {
    trial_kind: &'static str,
    identity: IdentityEvidence,
    field: &'static str,
    off_page: PageEvidence<'a>,
    expected_active_page: PageEvidence<'a>,
    control_page: PageEvidence<'a>,
    routing_page: PageEvidence<'a>,
    invariants: &'static str,
}

impl<'a> Scope<'a> {
    fn new(trial: &'a TerminalExitTrial) -> Self {
        Self {
            trial_kind: "terminal_to_off_trial",
            identity: IdentityEvidence::from(trial.identity()),
            field: "dv.DvGatewayModeDvGateway",
            off_page: PageEvidence::new(trial.page(), trial.off_page()),
            expected_active_page: PageEvidence::new(trial.page(), trial.expected_active_page()),
            control_page: PageEvidence::new(trial.control_page_spec(), trial.control_page()),
            routing_page: PageEvidence::new(trial.routing_page_spec(), trial.routing_page()),
            invariants: "one Terminal-to-Off byte change; no RF, routing, PM, callsign, or unrelated-byte change",
        }
    }
}

#[derive(Debug, Serialize)]
struct Prepared<'a> {
    scope: Scope<'a>,
    backup: &'a Path,
    active_page_provenance: &'static str,
    approval_provenance: &'static str,
    layout_qualification: &'static str,
}

#[derive(Debug, Serialize)]
struct WriteIntent<'a> {
    scope: Scope<'a>,
    session_id: u8,
    intent_id: u8,
    memory_format: u8,
    pre_entry_gateway: u8,
    desired_gateway: u8,
    active_page_provenance: &'static str,
    status: Status,
}

#[derive(Debug, Serialize)]
struct SessionEvidence<'a, T> {
    attempt: u8,
    details: &'a T,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    NotWritten,
    PossiblyChanged,
    OffVerifiedAcrossSessions,
}

impl From<TerminalExitTrialStatus> for Status {
    fn from(status: TerminalExitTrialStatus) -> Self {
        match status {
            TerminalExitTrialStatus::NotWritten => Self::NotWritten,
            TerminalExitTrialStatus::PossiblyChanged => Self::PossiblyChanged,
            TerminalExitTrialStatus::OffVerifiedAcrossSessions => Self::OffVerifiedAcrossSessions,
        }
    }
}

#[derive(Debug, Serialize)]
struct Finished {
    status: Status,
    further_recovery_assessment_required: bool,
    attempted_sessions: u8,
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
