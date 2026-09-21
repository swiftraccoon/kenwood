//! Append-only, fsynced JSON-lines record of one text update.
//!
//! Holds the update scope, the source backup path and the original page bytes
//! needed to restore the text by hand. The first append or synchronization
//! failure blocks every later append, including the success marker.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::Identity;
use kenwood_tmd750::types::{PAGE_SIZE, Page};
use serde::Serialize;

use super::super::IdentityEvidence;
use super::UpdateStatus;
use super::target::{Update, UpdateKind};
use crate::capture::Recorder;

#[cfg(unix)]
const FILENAME: &str = "update-journal.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Unprepared,
    Prepared,
    WriteIntent,
    Finished,
}

#[derive(Debug)]
struct BoundUpdate {
    kind: UpdateKind,
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
    control: Option<(Page, [u8; PAGE_SIZE])>,
}

impl BoundUpdate {
    fn matches(&self, update: &impl Update) -> bool {
        self.kind == update.kind()
            && &self.identity == update.identity()
            && self.page == update.page()
            && &self.original == update.original_page()
            && &self.desired == update.desired_page()
            && self.control.as_ref().map(|(page, data)| (*page, data)) == update.control_page()
    }
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

/// One journal file, written through the recorder the session captures use.
///
/// The scope and the original recovery bytes never change after `prepare`. A
/// failed append, synchronization or consistency check blocks every later
/// append, including the success marker.
#[derive(Debug)]
pub(super) struct UpdateJournal {
    recorder: Recorder<File>,
    directory: PathBuf,
    capture_failed: Arc<AtomicBool>,
    error: Option<StickyError>,
    stage: Stage,
    bound: Option<BoundUpdate>,
    #[cfg(all(test, unix))]
    fail_evidence: bool,
    #[cfg(all(test, unix))]
    fail_raw_sync: bool,
}

impl UpdateJournal {
    /// Create the journal file in an existing private directory.
    ///
    /// A symlinked directory is rejected and a pre-existing journal is never
    /// opened. `prepare` must succeed before the port is opened.
    #[cfg(unix)]
    pub(super) fn create(directory: &Path, capture_failed: Arc<AtomicBool>) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "text update journal requires a private, non-symlink directory",
            ));
        }
        let directory = directory.canonicalize()?;
        let file = crate::capture::create_private_file(&directory.join(FILENAME))?;
        Ok(Self {
            recorder: Recorder::named(file, Arc::clone(&capture_failed), FILENAME),
            directory,
            capture_failed,
            error: None,
            stage: Stage::Unprepared,
            bound: None,
            #[cfg(all(test, unix))]
            fail_evidence: false,
            #[cfg(all(test, unix))]
            fail_raw_sync: false,
        })
    }

    /// Return an error: non-Unix hosts lack the private-file and directory
    /// synchronization this journal requires.
    #[cfg(not(unix))]
    pub(super) fn create(_directory: &Path, _capture_failed: Arc<AtomicBool>) -> io::Result<Self> {
        Err(unsupported_platform())
    }

    /// Append the update scope, both complete pages and the backup path.
    ///
    /// Callers reach this only behind the required `--apply` flag. The file,
    /// its directory and the parent directory are synchronized before this
    /// returns `Ok`. Returns an `io::Error` when the journal is already
    /// prepared, the update is not `NotWritten`, or any append or
    /// synchronization fails.
    pub(super) fn prepare(&mut self, update: &impl Update, backup: &Path) -> io::Result<()> {
        self.require(
            self.stage == Stage::Unprepared,
            "journal is already prepared",
        )?;
        self.require(
            update.status() == UpdateStatus::NotWritten,
            "update already has a possible write",
        )?;
        self.append(
            Kind::Prepared,
            &Prepared {
                scope: Scope::from(update),
                backup,
                operator_approved_apply: true,
                automatic_restore: false,
                qualification: update.kind().qualification(),
            },
        )?;
        let result = synchronize_directory(&self.directory);
        self.remember(result)?;
        self.bound = Some(BoundUpdate {
            kind: update.kind(),
            identity: update.identity().clone(),
            page: update.page(),
            original: *update.original_page(),
            desired: *update.desired_page(),
            control: update.control_page().map(|(page, data)| (page, *data)),
        });
        self.stage = Stage::Prepared;
        Ok(())
    }

    /// Append and fsync the single write intent, before the W frame is sent.
    ///
    /// The engine calls this after it has matched a fresh identity, memory
    /// format zero and the exact whole-page before-image; the raw bytes behind
    /// those checks stay in the session capture. A second intent returns an
    /// `io::Error`.
    pub(super) fn intent(&mut self, update: &impl Update) -> io::Result<()> {
        self.intent_with_sync(update, Recorder::synchronize)
    }

    fn intent_with_sync(
        &mut self,
        update: &impl Update,
        synchronize: impl FnOnce(&mut Recorder<File>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.check_bound(update)?;
        self.require(
            self.stage == Stage::Prepared,
            "journal write intent is out of order",
        )?;
        self.require(
            update.status() == UpdateStatus::NotWritten,
            "journal write intent follows a possible write",
        )?;
        self.append_with_sync(
            Kind::WriteIntent,
            &WriteIntent {
                scope: Scope::from(update),
                session_id: 1,
                intent_id: 1,
                memory_format: 0,
                validation_provenance: match update.kind() {
                    UpdateKind::Pm1Name => "accepted engine FreshSession; raw format fragment and whole page in session capture",
                    UpdateKind::PmOffMy1 => "accepted guarded engine FreshSession; raw Gateway, format, full control and target pages synchronized in session capture before this intent",
                },
                status: UpdateStatus::PossiblyChanged,
            },
            synchronize,
        )?;
        self.stage = Stage::WriteIntent;
        Ok(())
    }

    /// Append one session's diagnostics; the update status is left unchanged.
    pub(super) fn evidence(&mut self, evidence: &impl Serialize) -> io::Result<()> {
        self.require(
            matches!(self.stage, Stage::Prepared | Stage::WriteIntent),
            "journal does not accept a session record at this stage",
        )?;
        #[cfg(all(test, unix))]
        if self.fail_evidence {
            return self.remember(Err(io::Error::other(
                "injected session record synchronization failure",
            )));
        }
        self.append(Kind::SessionEvidence, evidence)
    }

    /// Make the next `evidence` call fail, to exercise the sticky-failure path.
    #[cfg(all(test, unix))]
    pub(super) const fn fail_evidence_for_test(&mut self) {
        self.fail_evidence = true;
    }

    /// Make the next pre-write transcript synchronization fail once.
    #[cfg(all(test, unix))]
    pub(super) const fn fail_raw_sync_for_test(&mut self) {
        self.fail_raw_sync = true;
    }

    #[cfg(all(test, unix))]
    pub(super) fn take_raw_sync_failure_for_test(&mut self) -> bool {
        std::mem::take(&mut self.fail_raw_sync)
    }

    /// Append the update's final status and close the journal.
    ///
    /// The status must match the recorded stage: `NotWritten` only after
    /// `prepare`, the other two only after the write intent. Nothing is
    /// restored automatically, and an immediate readback alone never yields
    /// `VerifiedAcrossSessions`.
    pub(super) fn finish(&mut self, update: &impl Update) -> io::Result<()> {
        self.check_bound(update)?;
        let consistent = match update.status() {
            UpdateStatus::NotWritten => self.stage == Stage::Prepared,
            UpdateStatus::PossiblyChanged | UpdateStatus::VerifiedAcrossSessions => {
                self.stage == Stage::WriteIntent
            }
        };
        self.require(
            consistent,
            "final update status disagrees with durable journal intent",
        )?;
        self.append(
            Kind::Finished,
            &Finished {
                scope: Scope::from(update),
                status: update.status(),
                manual_recovery_may_be_required: update.status() == UpdateStatus::PossiblyChanged,
                automatic_restore: false,
            },
        )?;
        self.stage = Stage::Finished;
        Ok(())
    }

    fn check_bound(&mut self, update: &impl Update) -> io::Result<()> {
        self.require(
            self.bound
                .as_ref()
                .is_some_and(|bound| bound.matches(update)),
            "journal update differs from its prepared recovery record",
        )
    }

    /// `Ok` while the journal is usable; an error after any failed append or
    /// synchronization, which stops the workflow from opening a connection.
    pub(super) fn ensure_complete(&self) -> io::Result<()> {
        self.healthy()
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
        self.error.as_ref().map_or_else(
            || self.recorder.ensure_complete(),
            |error| Err(error.error()),
        )
    }

    fn remember(&mut self, result: io::Result<()>) -> io::Result<()> {
        if let Err(error) = result {
            if self.error.is_none() {
                self.error = Some(StickyError {
                    kind: error.kind(),
                    message: error.to_string(),
                });
            }
            self.capture_failed.store(true, Ordering::Relaxed);
        }
        self.healthy()
    }

    fn append(&mut self, kind: Kind, evidence: &impl Serialize) -> io::Result<()> {
        self.append_with_sync(kind, evidence, Recorder::synchronize)
    }

    fn append_with_sync(
        &mut self,
        kind: Kind,
        evidence: &impl Serialize,
        synchronize: impl FnOnce(&mut Recorder<File>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.healthy()?;
        self.recorder.record(Record {
            format_version: 1,
            kind,
            evidence,
        });
        let result = synchronize(&mut self.recorder);
        self.remember(result)
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
        "text updates require Unix private files and directory synchronization",
    )
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
    kind: Kind,
    evidence: &'a T,
}

#[derive(Debug, Serialize)]
struct Scope<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    target_kind: Option<UpdateKind>,
    identity: IdentityEvidence,
    field: &'static str,
    page_address: u32,
    page_length: usize,
    original_page: &'a [u8],
    desired_page: &'a [u8],
    current_name: &'a str,
    desired_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_page: Option<ControlPage<'a>>,
}

#[derive(Debug, Serialize)]
struct ControlPage<'a> {
    address: u32,
    length: usize,
    data: &'a [u8],
}

impl<'a, U: Update> From<&'a U> for Scope<'a> {
    fn from(update: &'a U) -> Self {
        Self {
            target_kind: (update.kind() == UpdateKind::PmOffMy1).then_some(update.kind()),
            identity: IdentityEvidence::from(update.identity()),
            field: update.field(),
            page_address: update.page().address().as_u32(),
            page_length: update.page().len(),
            original_page: update.original_page(),
            desired_page: update.desired_page(),
            current_name: update.current_text(),
            desired_name: update.desired_text(),
            control_page: update.control_page().map(|(page, data)| ControlPage {
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
    operator_approved_apply: bool,
    automatic_restore: bool,
    qualification: &'static str,
}

#[derive(Debug, Serialize)]
struct WriteIntent<'a> {
    scope: Scope<'a>,
    session_id: u8,
    intent_id: u8,
    memory_format: u8,
    validation_provenance: &'static str,
    status: UpdateStatus,
}

#[derive(Debug, Serialize)]
struct Finished<'a> {
    scope: Scope<'a>,
    status: UpdateStatus,
    manual_recovery_may_be_required: bool,
    automatic_restore: bool,
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "my1_journal_tests.rs"]
mod my1_tests;
