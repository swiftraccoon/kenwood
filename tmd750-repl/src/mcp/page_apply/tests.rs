//! The page-plan workflow's pre-write synchronization seam.

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[test]
fn pre_write_synchronization_failure_is_sticky() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut synchronization =
        RawSynchronization::new(create_private_file(&directory.path().join("raw.jsonl"))?);
    synchronization.fail_next = true;
    assert!(
        synchronization.synchronize().is_err(),
        "injected synchronization failure must be observable"
    );
    synchronization.file.sync_all()?;
    assert!(
        synchronization.synchronize().is_err(),
        "a later successful OS sync must not erase the first failure"
    );
    Ok(())
}
