//! Prepared output files and sequential atomic publication with explicit progress.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::error::{ExtractError, Result};

struct PreparedOutput {
    destination: PathBuf,
    temporary: NamedTempFile,
}

fn prepare(path: &Path, content: &str) -> Result<PreparedOutput> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| ExtractError::io("create output directory", parent, error))?;
    let destination = parent
        .canonicalize()
        .map_err(|error| ExtractError::io("resolve output directory", parent, error))?
        .join(path.file_name().ok_or_else(|| ExtractError::InvalidInput {
            message: format!("output path needs a file name: {}", path.display()),
        })?);
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(ExtractError::InvalidInput {
                message: format!("output must be a regular file: {}", destination.display()),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ExtractError::io("inspect output", &destination, error)),
    }
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| ExtractError::io("create temporary output", parent, error))?;
    temporary
        .write_all(content.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| ExtractError::io("prepare output", &destination, error))?;
    Ok(PreparedOutput {
        destination,
        temporary,
    })
}

/// Prepare every file before atomically replacing any individual destination.
///
/// Replacements occur in input order. Failure reports the destinations already
/// replaced; no individual file is truncated. Parent directories may be created
/// during preparation. Directory-entry durability and a multi-file transaction
/// are outside this completion boundary.
pub(crate) fn publish(outputs: &[(&Path, &str)]) -> Result<()> {
    let prepared = outputs
        .iter()
        .map(|(path, content)| prepare(path, content))
        .collect::<Result<Vec<_>>>()?;
    let mut destinations = std::collections::BTreeSet::new();
    for output in &prepared {
        if !destinations.insert(&output.destination) {
            return Err(ExtractError::InvalidInput {
                message: format!(
                    "duplicate output destination: {}",
                    output.destination.display()
                ),
            });
        }
    }
    publish_prepared(prepared, |_| Ok(()))
}

fn publish_prepared(
    prepared: Vec<PreparedOutput>,
    mut before_replace: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let mut published = Vec::new();
    for output in prepared {
        let replacement = before_replace(&output.destination).and_then(|()| {
            output
                .temporary
                .persist(&output.destination)
                .map(drop)
                .map_err(|error| error.error)
        });
        if let Err(source) = replacement {
            return Err(ExtractError::Publication {
                published,
                pending: output.destination,
                source,
            });
        }
        published.push(output.destination);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn preparation_failure_preserves_both_existing_outputs() -> TestResult {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("manifest.json");
        let second = directory.path().join("registry.rs");
        std::fs::write(&first, "old manifest")?;
        std::fs::write(&second, "old registry")?;
        let result = publish(&[(&first, "new manifest"), (directory.path(), "new registry")]);
        assert!(
            matches!(result, Err(ExtractError::InvalidInput { .. })),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(&first)?, "old manifest");
        assert_eq!(std::fs::read_to_string(&second)?, "old registry");
        Ok(())
    }

    #[test]
    fn replacement_failure_retains_source_and_exact_publication_progress() -> TestResult {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("manifest.json");
        let second = directory.path().join("registry.rs");
        std::fs::write(&first, "old manifest")?;
        std::fs::write(&second, "old registry")?;
        let prepared = vec![
            prepare(&first, "new manifest")?,
            prepare(&second, "new registry")?,
        ];
        let second_destination = second.canonicalize()?;
        let result = publish_prepared(prepared, |path| {
            if path == second_destination {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture replacement denied",
                ))
            } else {
                Ok(())
            }
        });
        let error = result.err().ok_or("publication unexpectedly succeeded")?;
        assert!(
            std::error::Error::source(&error).is_some(),
            "I/O source lost"
        );
        let ExtractError::Publication {
            published,
            pending,
            source,
        } = error
        else {
            return Err("wrong publication error".into());
        };
        assert_eq!(published, [first.canonicalize()?]);
        assert_eq!(pending, second.canonicalize()?);
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&first)?, "new manifest");
        assert_eq!(std::fs::read_to_string(&second)?, "old registry");
        Ok(())
    }
}
