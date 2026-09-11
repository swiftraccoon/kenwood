//! Exclusive, synchronized recovery records for the fixed PM1 experiment.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kenwood_tmd750::Identity;
use kenwood_tmd750::memory::{PmNameTrial, PmNameTrialStatus, PmNameTrialWrite};
use kenwood_tmd750::types::{PAGE_SIZE, Page};
use serde::Serialize;
use time::OffsetDateTime;

use super::super::IdentityEvidence;

#[cfg(unix)]
const FILENAME: &str = "trial-journal.jsonl";

/// Injected storage boundary; success must mean both content and metadata sync.
pub(super) trait DurableWrite: Write {
    fn synchronize(&mut self) -> io::Result<()>;
    fn synchronize_directory(&mut self) -> io::Result<()>;
}

/// Exclusive file plus the directory whose entry must survive before any W.
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
        "the PM1 experiment requires Unix private files and directory synchronization",
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
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    expected: [u8; PAGE_SIZE],
}

impl BoundTrial {
    fn matches(&self, trial: &PmNameTrial) -> bool {
        &self.identity == trial.identity()
            && self.page == trial.page()
            && &self.original == trial.original_page()
            && &self.expected == trial.expected_page()
    }
}

/// A single append-only journal; the first failure poisons all later appends.
#[derive(Debug)]
pub(super) struct Journal<S: DurableWrite = FileSink> {
    sink: S,
    sequence: u64,
    error: Option<StickyError>,
    stage: Stage,
    bound: Option<BoundTrial>,
}

impl Journal<FileSink> {
    /// Reserve the fixed private filename in a newly reserved private directory.
    /// Existing files are never opened for modification or truncated.
    #[cfg(unix)]
    pub(super) fn create(directory: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "PM1 journal requires a private, non-symlink directory",
            ));
        }
        let directory = directory.canonicalize()?;
        let file = super::super::capture::create_private_file(&directory.join(FILENAME))?;
        Ok(Self::new(FileSink { file, directory }))
    }

    /// Refuse platforms without this runner's private-file durability contract.
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

    /// Bind recovery bytes and separately confirmed display text before opening
    /// the radio. The caller must have separately obtained operator approval.
    /// Both file and directory entries are synchronized before success.
    pub(super) fn prepare(
        &mut self,
        trial: &PmNameTrial,
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
        let confirmed = PmNameTrial::prepare_unqualified_offline(
            trial.identity(),
            trial.original_page(),
            confirmed_name,
        );
        self.require(
            confirmed.is_ok(),
            "journal display confirmation does not match the trial",
        )?;
        self.append(
            Kind::Prepared,
            &Prepared {
                scope: Scope::from(trial),
                backup,
                operator_confirmed_name: confirmed_name,
                operator_approved_fixed_rename_and_restore: true,
                layout_qualification: "unqualified",
            },
        )?;
        let result = self.sink.synchronize_directory();
        self.remember(result)?;
        self.bound = Some(BoundTrial {
            identity: trial.identity().clone(),
            page: trial.page(),
            original: *trial.original_page(),
            expected: *trial.expected_page(),
        });
        self.stage = Stage::Prepared;
        Ok(())
    }

    /// Record the fixed backend intent and complete recovery bytes, then flush
    /// and synchronize before the backend may represent or dispatch any W.
    /// The callback follows the engine's fresh format and full-page validation;
    /// raw observations belong to the separately retained session capture.
    pub(super) fn intent(
        &mut self,
        trial: &PmNameTrial,
        write: PmNameTrialWrite,
    ) -> io::Result<()> {
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
            scope: Scope::from(trial), session_id, intent_id: session_id, write,
            memory_format: 0,
            validation_provenance: "accepted engine FreshSession; raw format fragment in session capture",
            restoration_status: Status::PossiblyChanged,
        })?;
        self.stage = next;
        Ok(())
    }

    /// Synchronize complete successful or failed session diagnostics. Evidence
    /// never clears an earlier journal failure or changes restoration status.
    pub(super) fn evidence(&mut self, evidence: &impl Serialize) -> io::Result<()> {
        self.require(
            !matches!(self.stage, Stage::Unprepared | Stage::Finished),
            "journal does not accept session evidence at this stage",
        )?;
        self.append(Kind::SessionEvidence, evidence)
    }

    /// Record the engine's final conservative status without inferring success
    /// from CAT, process completion, or an immediate original-page readback.
    pub(super) fn finish(&mut self, trial: &PmNameTrial) -> io::Result<()> {
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

    fn check_bound(&mut self, trial: &PmNameTrial) -> io::Result<()> {
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
    identity: IdentityEvidence,
    field: &'static str,
    page_address: u32,
    page_length: usize,
    original_page: &'a [u8],
    expected_page: &'a [u8],
    temporary_name: &'static str,
}

impl<'a> From<&'a PmNameTrial> for Scope<'a> {
    fn from(trial: &'a PmNameTrial) -> Self {
        Self {
            identity: IdentityEvidence::from(trial.identity()),
            field: "pm.PmName1",
            page_address: trial.page().address().as_u32(),
            page_length: trial.page().len(),
            original_page: trial.original_page(),
            expected_page: trial.expected_page(),
            temporary_name: PmNameTrial::TEMPORARY_NAME,
        }
    }
}

#[derive(Debug, Serialize)]
struct Prepared<'a> {
    scope: Scope<'a>,
    backup: &'a Path,
    operator_confirmed_name: &'a str,
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
