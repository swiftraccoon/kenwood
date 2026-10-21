// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Bounded snapshot loading and atomic file publication.

use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Maximum encoded settings/history snapshot accepted on read and write.
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// Snapshot size, encoding, read or atomic publication failure.
#[derive(Debug)]
pub(crate) enum SnapshotError {
    TooLarge,
    Io(io::Error),
    Encoding(std::string::FromUtf8Error),
    Save(SaveError),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes"),
            Self::Io(error) => write!(f, "read snapshot: {error}"),
            Self::Encoding(error) => write!(f, "snapshot is not UTF-8: {error}"),
            Self::Save(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge => None,
            Self::Io(error) => Some(error),
            Self::Encoding(error) => Some(error),
            Self::Save(error) => Some(error),
        }
    }
}

/// Read at most 1 MiB plus one sentinel byte, rejecting oversized input.
pub(crate) fn read_snapshot(path: &Path) -> Result<String, SnapshotError> {
    let file = File::open(path).map_err(SnapshotError::Io)?;
    let mut bytes = Vec::new();
    let _read = file
        .take((MAX_SNAPSHOT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(SnapshotError::Io)?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    String::from_utf8(bytes).map_err(SnapshotError::Encoding)
}

/// Reject an oversized snapshot before creating any publication files.
pub(crate) fn save_snapshot(path: &Path, bytes: &[u8]) -> Result<SaveOutcome, SnapshotError> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::TooLarge);
    }
    save_atomic(path, bytes).map_err(SnapshotError::Save)
}

/// Completion reached by a successful snapshot write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveOutcome {
    /// File bytes and the containing directory were synced.
    #[cfg(unix)]
    FileAndDirectorySynced,
    /// File bytes were synced and renamed; directory syncing is unavailable.
    #[cfg(not(unix))]
    FileSynced,
}

/// Failed publication step; publishing may precede a directory-sync failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveStage {
    PrepareDirectory,
    Reserve,
    Write,
    SyncFile,
    Publish,
    #[cfg(unix)]
    SyncDirectory,
}

/// Original publication failure and any independent temporary-file cleanup error.
#[derive(Debug)]
pub(crate) struct SaveError {
    path: PathBuf,
    stage: SaveStage,
    source: io::Error,
    cleanup: Option<io::Error>,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "saving {} failed during {:?}: {}",
            self.path.display(),
            self.stage,
            self.source
        )?;
        if let Some(cleanup) = &self.cleanup {
            write!(f, "; temporary-file cleanup failed: {cleanup}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Sync a complete temporary file before replacing the destination name.
///
/// Failed writes retain the previous destination. Unix success also syncs the
/// containing directory; other platforms report `SaveOutcome::FileSynced`.
/// Directory-sync failure reports an error after the replacement has occurred.
pub(crate) fn save_atomic(path: &Path, contents: &[u8]) -> Result<SaveOutcome, SaveError> {
    save_with(path, |file| file.write_all(contents))
}

/// Stream a complete file into an exclusive temporary, then sync and replace.
/// The producer runs once; its original failure and cleanup failure are retained.
pub(crate) fn save_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<SaveOutcome, SaveError> {
    let failure = |stage, source| SaveError {
        path: path.to_owned(),
        stage,
        source,
        cleanup: None,
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|source| failure(SaveStage::PrepareDirectory, source))?;
    let (temporary, mut file) =
        reserve(parent).map_err(|source| failure(SaveStage::Reserve, source))?;
    let operation: Result<SaveOutcome, SaveError> = (|| {
        write(&mut file).map_err(|source| failure(SaveStage::Write, source))?;
        file.sync_all()
            .map_err(|source| failure(SaveStage::SyncFile, source))?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|source| failure(SaveStage::Publish, source))?;
        #[cfg(unix)]
        {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| failure(SaveStage::SyncDirectory, source))?;
            Ok(SaveOutcome::FileAndDirectorySynced)
        }
        #[cfg(not(unix))]
        {
            Ok(SaveOutcome::FileSynced)
        }
    })();
    operation.map_err(|mut error| {
        if let Err(cleanup) = std::fs::remove_file(&temporary)
            && cleanup.kind() != io::ErrorKind::NotFound
        {
            error.cleanup = Some(cleanup);
        }
        error
    })
}

fn reserve(parent: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..128 {
        let ticket = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".sextant-{}-{ticket}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary snapshot names are occupied",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_snapshots_are_rejected_before_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "sextant-snapshot-limit-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        let path = root.join("snapshot");
        std::fs::write(&path, b"previous")?;
        let result = save_snapshot(&path, &vec![b'x'; MAX_SNAPSHOT_BYTES + 1]);
        assert!(
            matches!(result, Err(SnapshotError::TooLarge)),
            "oversized write was admitted: {result:?}"
        );
        assert_eq!(
            std::fs::read(&path)?,
            b"previous",
            "rejected write replaced the previous snapshot"
        );
        File::create(&path)?.set_len((MAX_SNAPSHOT_BYTES + 1) as u64)?;
        let result = read_snapshot(&path);
        assert!(
            matches!(result, Err(SnapshotError::TooLarge)),
            "oversized read was admitted"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_partial_write_preserves_the_previous_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "sextant-persistence-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        let path = root.join("settings.toml");
        std::fs::write(&path, b"previous")?;
        let result = save_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected storage failure",
            ))
        });
        assert!(
            matches!(
                result,
                Err(SaveError {
                    stage: SaveStage::Write,
                    ..
                })
            ),
            "{result:?}"
        );
        assert_eq!(std::fs::read(&path)?, b"previous");
        let _completion = save_atomic(&path, b"replacement")?;
        assert_eq!(std::fs::read(&path)?, b"replacement");
        assert_eq!(
            std::fs::read_dir(&root)?.count(),
            1,
            "temporary snapshot leaked"
        );
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}
