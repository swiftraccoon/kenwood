//! Coordinator-level tests: directory reservation, the report labels, and a
//! nothing-owed recovery. The lifecycle safety behavior is covered by the
//! library's own tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kenwood_tmd750::TerminalRecovery;
use kenwood_tmd750::transport::{KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID};
use serde_json::Value;

use super::*;
use crate::native::Endpoint;

type TestResult = AppResult<()>;

fn endpoints() -> AppResult<Endpoints> {
    Ok(Endpoints {
        bluetooth: Endpoint {
            address: "AA-BB-CC-DD-EE-01".parse()?,
            helper: None,
        },
        control: SerialCandidate {
            path: "/dev/cu.fake-radio".to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(TMD750_MAIN_PID),
        },
    })
}

/// A recovery that owes nothing, whose report lives under `runtime-recovery`.
///
/// Runtime tests use it to exercise close and publication without touching a
/// radio for restoration.
pub(crate) fn runtime_recovery(parent: &Path) -> AppResult<Recovery> {
    let endpoints = endpoints()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let directory = parent.join("runtime-recovery");
    let (directory, report_file) = reserve_at(
        &endpoints,
        Arc::clone(&cancelled),
        Some(&directory),
        // Only directory durability is synthetic; recovery file I/O stays real.
        |_| Ok(()),
    )?;
    let control = ControlHostImpl::new(&directory, Arc::clone(&cancelled));
    let journal = Journal::create(&directory, Arc::clone(&cancelled))?;
    Ok(Recovery {
        directory,
        report_file,
        cancelled,
        control,
        journal,
        terminal: TerminalRecovery::nothing_owed(endpoints.control, DEFAULT_BAUD),
        bluetooth_address: endpoints.bluetooth.address.to_string(),
        control_path: "/dev/cu.fake-radio".to_owned(),
        modem_proved: true,
        preflight: None,
        entry: None,
        transition: TransitionSummary::default(),
        modem_openings: Value::Null,
        readiness: Vec::new(),
        owner_released: None,
        errors: Vec::new(),
    })
}

#[test]
fn reservation_creates_the_directory_and_report_then_syncs_the_parent() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let directory = temporary.path().join("startup");
    let mut synchronized = Vec::new();
    let (created, _report) = reserve_at(
        &endpoints()?,
        Arc::new(AtomicBool::new(false)),
        Some(&directory),
        |path| {
            synchronized.push(path.to_path_buf());
            Ok(())
        },
    )?;
    assert_eq!(created, directory);
    assert!(directory.join("report.json").is_file());
    assert_eq!(synchronized, [directory, temporary.path().to_path_buf()]);
    Ok(())
}

#[test]
fn directory_synchronization_normalizes_an_empty_relative_parent() -> TestResult {
    for (directory, parent) in [("startup", "."), ("captures/startup", "captures")] {
        let mut synchronized = Vec::new();
        synchronize_directories(Path::new(directory), |path| {
            synchronized.push(path.to_path_buf());
            Ok(())
        })?;
        assert_eq!(
            synchronized,
            [PathBuf::from(directory), PathBuf::from(parent)]
        );
    }
    Ok(())
}

#[tokio::test]
async fn nothing_owed_recovery_finishes_verified_and_publishes_owner_released() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let mut recovery = runtime_recovery(temporary.path())?;
    recovery.record_runtime_failure("injected runtime failure");
    recovery.finish(true).await.map_err(CommandError)?;
    let report: Value = serde_json::from_reader(File::open(
        temporary.path().join("runtime-recovery/report.json"),
    )?)?;
    assert_eq!(report.pointer("/owner_released"), Some(&Value::Bool(true)));
    assert_eq!(
        report.pointer("/restoration"),
        Some(&Value::from("verified"))
    );
    assert_eq!(
        report.pointer("/errors/0/message"),
        Some(&Value::from("injected runtime failure"))
    );
    Ok(())
}

#[tokio::test]
async fn nothing_owed_false_release_verifies_but_reports_owner_not_released() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let recovery = runtime_recovery(temporary.path())?;
    // Nothing is owed, so there is nothing to restore even when the modem was
    // not confirmed released; the report still records the release state.
    recovery.finish(false).await.map_err(CommandError)?;
    let report: Value = serde_json::from_reader(File::open(
        temporary.path().join("runtime-recovery/report.json"),
    )?)?;
    assert_eq!(report.pointer("/owner_released"), Some(&Value::Bool(false)));
    assert_eq!(
        report.pointer("/restoration"),
        Some(&Value::from("verified"))
    );
    Ok(())
}

#[test]
fn restoration_labels_cover_every_state() {
    use kenwood_tmd750::radio::terminal::lifecycle::RestorationState;
    assert_eq!(
        restoration_label(RestorationState::NotRequired),
        "not_required"
    );
    assert_eq!(restoration_label(RestorationState::Owed), "owed");
    assert_eq!(restoration_label(RestorationState::Verified), "verified");
    assert_eq!(restoration_label(RestorationState::Blocked), "blocked");
}
