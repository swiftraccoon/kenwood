//! Bounded, validated MCP cache snapshots with temporary-file replacement.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use kenwood_thd75::memory::{MemoryError, MemoryImage};
use kenwood_thd75::protocol::programming::TOTAL_SIZE;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A checked cache image and its available wall-clock file age.
#[derive(Debug)]
pub(crate) struct CachedImage {
    pub(crate) image: MemoryImage,
    pub(crate) age: Option<Duration>,
}

/// Cache I/O or image validation failure.
#[derive(Debug)]
pub(crate) enum CacheError {
    /// Opening, metadata inspection or bounded reading failed.
    Io(io::Error),
    /// The retained bytes do not form the model's fixed image.
    Image(MemoryError),
    /// Metadata proves the file exceeds the complete image length.
    TooLarge { actual: u64 },
    /// The platform cannot represent the selected model byte bound.
    SizeRepresentation,
    /// The destination has no filename component.
    MissingFileName,
    /// The name counter is exhausted or 32 exclusive naming attempts collided.
    NameExhausted,
    /// Writing/replacement failed; a separate temporary cleanup may also fail.
    Write {
        source: io::Error,
        cleanup: Option<io::Error>,
    },
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Image(error) => error.fmt(formatter),
            Self::TooLarge { actual } => {
                write!(formatter, "cache has {actual} bytes; expected {TOTAL_SIZE}")
            }
            Self::SizeRepresentation => {
                formatter.write_str("model image length is unrepresentable")
            }
            Self::MissingFileName => formatter.write_str("cache destination has no filename"),
            Self::NameExhausted => {
                formatter.write_str("exclusive cache temporary-name attempts exhausted")
            }
            Self::Write { source, cleanup } => {
                source.fmt(formatter)?;
                if let Some(cleanup) = cleanup {
                    write!(formatter, "; temporary cleanup also failed: {cleanup}")?;
                }
                Ok(())
            }
        }
    }
}
impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Write { source: error, .. } => Some(error),
            Self::Image(error) => Some(error),
            Self::TooLarge { .. }
            | Self::SizeRepresentation
            | Self::MissingFileName
            | Self::NameExhausted => None,
        }
    }
}

/// Read at most one complete image plus a growth-detection byte before parsing.
///
/// Missing files return `None`; malformed, oversized and unreadable files are
/// errors. Modification time in the future or unavailable metadata age is `None`.
pub(crate) fn load(path: &Path) -> Result<Option<CachedImage>, CacheError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CacheError::Io(error)),
    };
    let metadata = file.metadata().map_err(CacheError::Io)?;
    let maximum = u64::try_from(TOTAL_SIZE).map_err(|_error| CacheError::SizeRepresentation)?;
    if metadata.len() > maximum {
        return Err(CacheError::TooLarge {
            actual: metadata.len(),
        });
    }
    let image = read_image(file, maximum)?;
    let age = metadata
        .modified()
        .ok()
        .and_then(|time| SystemTime::now().duration_since(time).ok());
    Ok(Some(CachedImage { image, age }))
}

fn read_image(reader: impl io::Read, maximum: u64) -> Result<MemoryImage, CacheError> {
    let read_bound = maximum
        .checked_add(1)
        .ok_or(CacheError::SizeRepresentation)?;
    let mut bytes = Vec::new();
    let _read = reader
        .take(read_bound)
        .read_to_end(&mut bytes)
        .map_err(CacheError::Io)?;
    MemoryImage::from_raw(bytes).map_err(CacheError::Image)
}

/// Write a validated image to an exclusive temporary file, sync it and rename.
///
/// The previous cache remains until replacement. File and cleanup failures are
/// reported independently. Containing directories are not synced; this is a
/// rebuildable cache, without a power-loss persistence contract.
pub(crate) fn save(path: &Path, image: &MemoryImage) -> Result<(), CacheError> {
    save_before_replace(path, image, || Ok(()))
}

fn save_before_replace(
    path: &Path,
    image: &MemoryImage,
    before_replace: impl FnOnce() -> io::Result<()>,
) -> Result<(), CacheError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(CacheError::Io)?;
    let (temporary, mut file) = reserve_temporary(path, parent)?;
    let result = (|| {
        file.write_all(image.as_raw())?;
        file.sync_all()?;
        drop(file);
        before_replace()?;
        fs::rename(&temporary, path)
    })();
    if let Err(source) = result {
        let cleanup = match fs::remove_file(&temporary) {
            Ok(()) => None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => Some(error),
        };
        return Err(CacheError::Write { source, cleanup });
    }
    Ok(())
}

fn reserve_temporary(path: &Path, parent: &Path) -> Result<(PathBuf, File), CacheError> {
    let name = path.file_name().ok_or(CacheError::MissingFileName)?;
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_exhausted| CacheError::NameExhausted)?;
        let mut temporary_name = name.to_os_string();
        temporary_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let temporary = parent.join(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(CacheError::Io(error)),
        }
    }
    Err(CacheError::NameExhausted)
}

#[cfg(test)]
mod tests {
    use super::{CacheError, TEMP_SEQUENCE, load, read_image, save, save_before_replace};
    use kenwood_thd75::memory::MemoryImage;
    use kenwood_thd75::protocol::programming::TOTAL_SIZE;
    use std::io::{Cursor, Read};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    struct CountingRead {
        input: Cursor<Vec<u8>>,
        count: Arc<AtomicUsize>,
    }
    impl Read for CountingRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = self.input.read(buffer)?;
            let _previous = self.count.fetch_add(count, Ordering::AcqRel);
            Ok(count)
        }
    }

    #[test]
    fn growing_cache_read_stops_after_one_detection_byte() -> Result<(), Box<dyn std::error::Error>>
    {
        let count = Arc::new(AtomicUsize::new(0));
        let input = CountingRead {
            input: Cursor::new(vec![0; TOTAL_SIZE * 2]),
            count: Arc::clone(&count),
        };
        assert!(
            read_image(input, u64::try_from(TOTAL_SIZE)?).is_err(),
            "growing image was silently truncated"
        );
        assert_eq!(
            count.load(Ordering::Acquire),
            TOTAL_SIZE + 1,
            "cache growth escaped its read bound"
        );
        Ok(())
    }

    #[derive(Debug)]
    struct TestDirectory(std::path::PathBuf);
    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            for _ in 0..32 {
                let sequence = TEMP_SEQUENCE
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                        value.checked_add(1)
                    })
                    .map_err(|_error| std::io::Error::other("fixture names exhausted"))?;
                let path = std::env::temp_dir()
                    .join(format!("thd75-tui-cache-{}-{sequence}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Ok(Self(path)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(std::io::Error::other(
                "fixture directory collisions exhausted",
            ))
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _cleanup = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bounded_read_rejects_oversized_and_malformed_cache() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = TestDirectory::new()?;
        let path = directory.0.join("mcp.bin");
        assert!(load(&path)?.is_none(), "missing cache was not absent");
        std::fs::write(&path, [0; 3])?;
        assert!(matches!(load(&path), Err(CacheError::Image(_))));
        let file = std::fs::File::create(&path)?;
        file.set_len(
            u64::try_from(TOTAL_SIZE)?
                .checked_add(1)
                .ok_or("length overflow")?,
        )?;
        assert!(matches!(load(&path), Err(CacheError::TooLarge { .. })));
        Ok(())
    }

    #[test]
    fn failed_replacement_preserves_previous_validated_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.0.join("mcp.bin");
        let original = MemoryImage::from_raw(vec![0x11; TOTAL_SIZE])?;
        let replacement = MemoryImage::from_raw(vec![0x22; TOTAL_SIZE])?;
        save(&path, &original)?;
        let result = save_before_replace(&path, &replacement, || {
            Err(std::io::Error::other("injected replacement failure"))
        });
        assert!(matches!(result, Err(CacheError::Write { .. })));
        assert_eq!(
            load(&path)?.ok_or("cache disappeared")?.image.as_raw(),
            original.as_raw()
        );
        save(&path, &replacement)?;
        assert_eq!(
            load(&path)?.ok_or("cache disappeared")?.image.as_raw(),
            replacement.as_raw()
        );
        Ok(())
    }
}
