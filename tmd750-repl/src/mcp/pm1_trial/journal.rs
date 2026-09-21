//! Append-only, fsynced recovery records for the two fixed text round trips.
//!
//! One private JSON-lines file per run holds the trial scope and the original
//! page bytes needed to restore the text by hand. The first append or
//! synchronization failure blocks every later append.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kenwood_tmd750::Identity;
#[cfg(test)]
use kenwood_tmd750::memory::PmNameTrial;
use kenwood_tmd750::memory::{PmNameTrialStatus, PmNameTrialWrite};
use kenwood_tmd750::types::{PAGE_SIZE, Page};
use serde::Serialize;
use time::OffsetDateTime;

use super::super::IdentityEvidence;
use super::target::{Trial, TrialKind};

#[cfg(unix)]
const FILENAME: &str = "trial-journal.jsonl";

/// Sink whose `Ok` means the bytes and the directory entry reached storage.
///
/// Tests substitute a sink that fails on demand.
pub(super) trait DurableWrite: Write {
    /// Synchronize the file's contents and metadata.
    fn synchronize(&mut self) -> io::Result<()>;
    /// Synchronize the containing directory and its parent entry.
    fn synchronize_directory(&mut self) -> io::Result<()>;
}

/// The journal file and the directory holding its entry.
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
        "text experiments require Unix private files and directory synchronization",
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
    RenameIntent,
    RestoreIntent,
    Finished,
}

#[derive(Debug)]
struct BoundTrial {
    kind: TrialKind,
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    expected: [u8; PAGE_SIZE],
    control: Option<(Page, [u8; PAGE_SIZE])>,
}

impl BoundTrial {
    fn matches(&self, trial: &impl Trial) -> bool {
        self.kind == trial.kind()
            && &self.identity == trial.identity()
            && self.page == trial.page()
            && &self.original == trial.original_page()
            && &self.expected == trial.expected_page()
            && self.control.as_ref().map(|(page, bytes)| (*page, bytes)) == trial.control_page()
    }
}

/// One append-only journal; its first failure blocks all later appends.
#[derive(Debug)]
pub(super) struct Journal<S: DurableWrite = FileSink> {
    sink: S,
    sequence: u64,
    error: Option<StickyError>,
    stage: Stage,
    bound: Option<BoundTrial>,
}

impl Journal<FileSink> {
    /// Create the journal file in an existing private directory.
    ///
    /// The directory must be a real directory with no group or other
    /// permissions. An existing file is never opened for modification or
    /// truncated; the call fails instead.
    #[cfg(unix)]
    pub(super) fn create(directory: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "text-trial journal requires a private, non-symlink directory",
            ));
        }
        let directory = directory.canonicalize()?;
        let file = crate::capture::create_private_file(&directory.join(FILENAME))?;
        Ok(Self::new(FileSink { file, directory }))
    }

    /// Return an error: non-Unix hosts lack the private-file and directory
    /// synchronization this journal requires.
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
        }
    }

    /// Record the recovery bytes and the baseline check, before the port opens.
    ///
    /// PM1 requires `confirmed_name` to match the name read off the radio
    /// display; MY1 requires an empty captured baseline. The file and its
    /// directory are synchronized before this returns `Ok`. Returns an
    /// `io::Error` when the journal is already prepared, the trial already has
    /// a write obligation, the confirmation does not match, or any append or
    /// synchronization fails.
    pub(super) fn prepare(
        &mut self,
        trial: &impl Trial,
        backup: &Path,
        confirmed_name: &str,
    ) -> io::Result<()> {
        self.require(
            self.stage == Stage::Unprepared,
            "journal is already prepared",
        )?;
        self.require(
            trial.status() == PmNameTrialStatus::NotWritten,
            "trial already has a write obligation",
        )?;
        let confirmed = trial.validate_confirmation(confirmed_name);
        self.require(
            confirmed.is_ok(),
            "journal baseline confirmation does not match the trial",
        )?;
        self.append(
            Kind::Prepared,
            &Prepared {
                scope: Scope::new(trial),
                backup,
                operator_confirmed_name: (trial.kind() == TrialKind::Pm1Name)
                    .then_some(confirmed_name),
                baseline_confirmation: match trial.kind() {
                    TrialKind::Pm1Name => "independently observed PM1 display name",
                    TrialKind::PmOffMy1 => {
                        "empty MY1 in completed backup; not independently observed display text"
                    }
                },
                operator_approved_fixed_rename_and_restore: true,
                layout_qualification: "unqualified",
            },
        )?;
        let result = self.sink.synchronize_directory();
        self.remember(result)?;
        self.bound = Some(BoundTrial {
            kind: trial.kind(),
            identity: trial.identity().clone(),
            page: trial.page(),
            original: *trial.original_page(),
            expected: *trial.expected_page(),
            control: trial.control_page().map(|(page, bytes)| (page, *bytes)),
        });
        self.stage = Stage::Prepared;
        Ok(())
    }

    /// Append the write intent and recovery bytes, then flush and fsync them.
    ///
    /// The engine calls this after its fresh-format and whole-page checks and
    /// before it sends the W frame; the raw bytes behind those checks stay in
    /// the session capture. A rename intent must follow `prepare` and a restore
    /// intent must follow the rename intent. Returns an `io::Error` on an
    /// out-of-order call, a trial mismatch, or a failed append or
    /// synchronization.
    pub(super) fn intent(&mut self, trial: &impl Trial, write: PmNameTrialWrite) -> io::Result<()> {
        self.check_bound(trial)?;
        let (required, next, session_id, write) = match write {
            PmNameTrialWrite::Rename => {
                (Stage::Prepared, Stage::RenameIntent, 1, WriteKind::Rename)
            }
            PmNameTrialWrite::Restore => (
                Stage::RenameIntent,
                Stage::RestoreIntent,
                2,
                WriteKind::Restore,
            ),
        };
        self.require(
            self.stage == required,
            "journal write intent is out of order",
        )?;
        self.append(Kind::WriteIntent, &WriteIntentRecord {
            scope: Scope::new(trial), session_id, intent_id: session_id, write,
            memory_format: 0,
            validation_provenance: "accepted engine FreshSession; raw format fragment in session capture",
            restoration_status: Status::PossiblyChanged,
        })?;
        self.stage = next;
        Ok(())
    }

    /// Append one session's diagnostics, successful or failed.
    ///
    /// The record never clears an earlier journal failure or changes the
    /// restoration status. Rejected before `prepare` and after `finish`.
    pub(super) fn evidence(&mut self, evidence: &impl Serialize) -> io::Result<()> {
        self.require(
            !matches!(self.stage, Stage::Unprepared | Stage::Finished),
            "journal does not accept a session record at this stage",
        )?;
        self.append(Kind::SessionEvidence, evidence)
    }

    /// Append the trial's final status and close the journal.
    ///
    /// The status must match the recorded stage: `NotWritten` only after
    /// `prepare`, `PossiblyChanged` after either write intent, and
    /// `RestorationVerified` only after the restore intent.
    pub(super) fn finish(&mut self, trial: &impl Trial) -> io::Result<()> {
        self.check_bound(trial)?;
        self.require(self.stage != Stage::Finished, "journal is already finished")?;
        let consistent = match trial.status() {
            PmNameTrialStatus::NotWritten => self.stage == Stage::Prepared,
            PmNameTrialStatus::PossiblyChanged => {
                matches!(self.stage, Stage::RenameIntent | Stage::RestoreIntent)
            }
            PmNameTrialStatus::RestorationVerified => self.stage == Stage::RestoreIntent,
        };
        self.require(
            consistent,
            "final trial status disagrees with durable journal intents",
        )?;
        self.append(
            Kind::Finished,
            &Finished {
                status: Status::from(trial.status()),
                restoration_required: trial.status() == PmNameTrialStatus::PossiblyChanged,
            },
        )?;
        self.stage = Stage::Finished;
        Ok(())
    }

    fn check_bound(&mut self, trial: &impl Trial) -> io::Result<()> {
        self.require(
            self.bound
                .as_ref()
                .is_some_and(|bound| bound.matches(trial)),
            "journal trial differs from its prepared recovery record",
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
        self.sink.synchronize()
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
struct Scope<'a> {
    trial_kind: TrialKind,
    identity: IdentityEvidence,
    field: &'static str,
    page_address: u32,
    page_length: usize,
    original_page: &'a [u8],
    expected_page: &'a [u8],
    temporary_name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_page: Option<ControlPage<'a>>,
}

#[derive(Debug, Serialize)]
struct ControlPage<'a> {
    address: u32,
    length: usize,
    data: &'a [u8],
}

impl<'a> Scope<'a> {
    fn new(trial: &'a impl Trial) -> Self {
        Self {
            trial_kind: trial.kind(),
            identity: IdentityEvidence::from(trial.identity()),
            field: trial.field(),
            page_address: trial.page().address().as_u32(),
            page_length: trial.page().len(),
            original_page: trial.original_page(),
            expected_page: trial.expected_page(),
            temporary_name: trial.temporary_text(),
            control_page: trial.control_page().map(|(page, data)| ControlPage {
                address: page.address().as_u32(),
                length: page.len(),
                data,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct Prepared<'a> {
    scope: Scope<'a>,
    backup: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    operator_confirmed_name: Option<&'a str>,
    baseline_confirmation: &'static str,
    operator_approved_fixed_rename_and_restore: bool,
    layout_qualification: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum WriteKind {
    Rename,
    Restore,
}

#[derive(Debug, Serialize)]
struct WriteIntentRecord<'a> {
    scope: Scope<'a>,
    session_id: u8,
    intent_id: u8,
    write: WriteKind,
    memory_format: u8,
    validation_provenance: &'static str,
    restoration_status: Status,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    NotWritten,
    PossiblyChanged,
    RestorationVerified,
}

impl From<PmNameTrialStatus> for Status {
    fn from(status: PmNameTrialStatus) -> Self {
        match status {
            PmNameTrialStatus::NotWritten => Self::NotWritten,
            PmNameTrialStatus::PossiblyChanged => Self::PossiblyChanged,
            PmNameTrialStatus::RestorationVerified => Self::RestorationVerified,
        }
    }
}

#[derive(Debug, Serialize)]
struct Finished {
    status: Status,
    restoration_required: bool,
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
