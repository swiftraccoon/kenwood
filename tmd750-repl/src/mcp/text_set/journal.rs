//! Durable, immutable-scope evidence for one operator-approved PM1 update.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::Identity;
use kenwood_tmd750::memory::{Pm1NameUpdate, Pm1NameUpdateStatus};
use kenwood_tmd750::types::{PAGE_SIZE, Page};
use serde::Serialize;

use super::super::IdentityEvidence;
use super::super::capture::Recorder;
use super::UpdateStatus;

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
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
}

impl BoundUpdate {
    fn matches(&self, update: &Pm1NameUpdate) -> bool {
        &self.identity == update.identity()
            && self.page == update.page()
            && &self.original == update.original_page()
            && &self.desired == update.desired_page()
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

/// One exclusive journal using the same durable recorder as session captures.
///
/// Scope and original recovery bytes never change. A failed append, sync, or
/// consistency check prevents every later append, including a success marker.
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
}

impl UpdateJournal {
    /// Reserve a private file without following a directory symlink or opening
    /// any pre-existing journal. Preparation must succeed before opening USB.
    #[cfg(unix)]
    pub(super) fn create(directory: &Path, capture_failed: Arc<AtomicBool>) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "PM1 update journal requires a private, non-symlink directory",
            ));
        }
        let directory = directory.canonicalize()?;
        let file = super::super::capture::create_private_file(&directory.join(FILENAME))?;
        Ok(Self {
            recorder: Recorder::named(file, Arc::clone(&capture_failed), FILENAME),
            directory,
            capture_failed,
            error: None,
            stage: Stage::Unprepared,
            bound: None,
            #[cfg(all(test, unix))]
            fail_evidence: false,
        })
    }

    /// Refuse hosts without this command's private-file durability contract.
    #[cfg(not(unix))]
    pub(super) fn create(_directory: &Path, _capture_failed: Arc<AtomicBool>) -> io::Result<Self> {
        Err(unsupported_platform())
    }

    /// Preserve the approved update, both complete pages, and backup provenance.
    ///
    /// The caller must require explicit `--apply` approval before calling this
    /// method. Success includes file, directory, and parent-directory sync.
    pub(super) fn prepare(&mut self, update: &Pm1NameUpdate, backup: &Path) -> io::Result<()> {
        self.require(
            self.stage == Stage::Unprepared,
            "journal is already prepared",
        )?;
        self.require(
            update.status() == Pm1NameUpdateStatus::NotWritten,
            "update already has a possible write",
        )?;
        self.append(
            Kind::Prepared,
            &Prepared {
                scope: Scope::from(update),
                backup,
                operator_approved_apply: true,
                automatic_restore: false,
                qualification: "PM1 name only; TM-D750 firmware 1.02 and type K,2,1",
            },
        )?;
        let result = synchronize_directory(&self.directory);
        self.remember(result)?;
        self.bound = Some(BoundUpdate {
            identity: update.identity().clone(),
            page: update.page(),
            original: *update.original_page(),
            desired: *update.desired_page(),
        });
        self.stage = Stage::Prepared;
        Ok(())
    }

    /// Synchronize the sole write intent before the backend may dispatch W.
    ///
    /// The backend calls this only after the engine has accepted a fresh
    /// identity, format byte zero, and exact whole-page before-image. That
    /// validation is attributed explicitly; its raw bytes remain in capture.
    pub(super) fn intent(&mut self, update: &Pm1NameUpdate) -> io::Result<()> {
        self.intent_with_sync(update, Recorder::synchronize)
    }

    fn intent_with_sync(
        &mut self,
        update: &Pm1NameUpdate,
        synchronize: impl FnOnce(&mut Recorder<File>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.check_bound(update)?;
        self.require(
            self.stage == Stage::Prepared,
            "journal write intent is out of order",
        )?;
        self.require(
            update.status() == Pm1NameUpdateStatus::NotWritten,
            "journal write intent follows a possible write",
        )?;
        self.append_with_sync(
            Kind::WriteIntent,
            &WriteIntent {
                scope: Scope::from(update),
                session_id: 1,
                intent_id: 1,
                memory_format: 0,
                validation_provenance: "accepted engine FreshSession; raw format fragment and whole page in session capture",
                status: UpdateStatus::PossiblyChanged,
            },
            synchronize,
        )?;
        self.stage = Stage::WriteIntent;
        Ok(())
    }

    /// Retain and synchronize session diagnostics without changing write risk.
    pub(super) fn evidence(&mut self, evidence: &impl Serialize) -> io::Result<()> {
        self.require(
            matches!(self.stage, Stage::Prepared | Stage::WriteIntent),
            "journal does not accept session evidence at this stage",
        )?;
        #[cfg(all(test, unix))]
        if self.fail_evidence {
            return self.remember(Err(io::Error::other(
                "injected session evidence synchronization failure",
            )));
        }
        self.append(Kind::SessionEvidence, evidence)
    }

    /// Inject failure at the workflow's first post-session evidence boundary.
    #[cfg(all(test, unix))]
    pub(super) const fn fail_evidence_for_test(&mut self) {
        self.fail_evidence = true;
    }

    /// Record the engine's conservative result without automatically restoring
    /// or treating immediate readback as verification across separate sessions.
    pub(super) fn finish(&mut self, update: &Pm1NameUpdate) -> io::Result<()> {
        self.check_bound(update)?;
        let consistent = match update.status() {
            Pm1NameUpdateStatus::NotWritten => self.stage == Stage::Prepared,
            Pm1NameUpdateStatus::PossiblyChanged | Pm1NameUpdateStatus::VerifiedAcrossSessions => {
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
                status: UpdateStatus::from(update.status()),
                manual_recovery_may_be_required: update.status()
                    == Pm1NameUpdateStatus::PossiblyChanged,
                automatic_restore: false,
            },
        )?;
        self.stage = Stage::Finished;
        Ok(())
    }

    fn check_bound(&mut self, update: &Pm1NameUpdate) -> io::Result<()> {
        self.require(
            self.bound
                .as_ref()
                .is_some_and(|bound| bound.matches(update)),
            "journal update differs from its prepared recovery record",
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
        "PM1 updates require Unix private files and directory synchronization",
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
    identity: IdentityEvidence,
    field: &'static str,
    page_address: u32,
    page_length: usize,
    original_page: &'a [u8],
    desired_page: &'a [u8],
    current_name: &'a str,
    desired_name: &'a str,
}

impl<'a> From<&'a Pm1NameUpdate> for Scope<'a> {
    fn from(update: &'a Pm1NameUpdate) -> Self {
        Self {
            identity: IdentityEvidence::from(update.identity()),
            field: "pm.PmName1",
            page_address: update.page().address().as_u32(),
            page_length: update.page().len(),
            original_page: update.original_page(),
            desired_page: update.desired_page(),
            current_name: update.current_name().as_str(),
            desired_name: update.desired_name().as_str(),
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
