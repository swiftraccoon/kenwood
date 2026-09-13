//! Fake-native and USB coverage for end-to-end ownership and guarded recovery.

use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;

use kenwood_tmd750::memory::TerminalGatewayRoute;
use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID};
use kenwood_transport::error::{BluetoothCloseFailure, BluetoothOpenStage};
use kenwood_transport::{MockTransport, TransportError};

use super::*;
use program::tests::{backup, cat, changes, compare, configuration, identity, read};

type TestResult = AppResult<()>;
type Log = Arc<Mutex<Vec<Seen>>>;

const VERSION: &[u8] = b"\xE0\x0E\x00\x01MMDVM 2018";

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Open(&'static str),
    Write(&'static str, Vec<u8>),
    Close(&'static str),
    Drop(&'static str),
}

fn record(log: &Log, event: Seen) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Write(io::Error::other("test log poisoned")))?
        .push(event);
    Ok(())
}

struct FakeConnection {
    name: &'static str,
    mock: MockTransport,
    log: Log,
    close_failed: bool,
    cancel_on_open: Option<Arc<AtomicBool>>,
    cancel_on_exit: Option<Arc<AtomicBool>>,
    last_write: Vec<u8>,
}

impl Transport for FakeConnection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        record(&self.log, Seen::Write(self.name, bytes.to_vec()))?;
        self.last_write = bytes.to_vec();
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.mock.read(bytes).await?;
        if self.last_write == b"E"
            && bytes.get(..count) == Some(&[6])
            && let Some(cancelled) = &self.cancel_on_exit
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        record(&self.log, Seen::Close(self.name))?;
        self.mock.assert_complete();
        if self.close_failed {
            Err(TransportError::Disconnected(io::Error::other(
                "fake close failure",
            )))
        } else {
            Ok(())
        }
    }
}

impl Drop for FakeConnection {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Seen::Drop(self.name));
        }
    }
}

struct Native {
    owners: VecDeque<FakeConnection>,
    log: Log,
}

impl native::Backend for Native {
    type Connection = FakeConnection;

    async fn open(
        &mut self,
        endpoint: &native::Endpoint,
        _service: BluetoothService,
        _cancelled: &AtomicBool,
    ) -> Result<native::Opened<FakeConnection>, native::OpenFailure> {
        let owner = self.owners.pop_front().ok_or_else(|| {
            native::OpenFailure::from_error(&io::Error::other("unexpected native open"))
        })?;
        record(&self.log, Seen::Open(owner.name))
            .map_err(|error| native::OpenFailure::from_error(&error))?;
        Ok(native::Opened {
            connection: owner,
            resolved: native::Resolved {
                address: endpoint.address.to_string(),
                rfcomm_channel: 19,
            },
        })
    }

    async fn wait(&mut self, _duration: Duration) {}
}

struct Usb {
    owners: VecDeque<FakeConnection>,
    endpoint: SerialCandidate,
    log: Log,
    elapsed: Duration,
    absent: VecDeque<bool>,
    unavailable: bool,
}

impl UsbBackend for Usb {
    type Connection = FakeConnection;

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<FakeConnection, TransportError> {
        assert_eq!(*endpoint, self.endpoint);
        assert_eq!(baud, DEFAULT_BAUD);
        let owner = self
            .owners
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: endpoint.path.clone(),
                source: io::Error::other("unexpected USB open"),
            })?;
        record(&self.log, Seen::Open(owner.name))?;
        if let Some(cancelled) = &owner.cancel_on_open {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(owner)
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        if self.unavailable || self.absent.pop_front().unwrap_or(false) {
            Ok(Vec::new())
        } else {
            Ok(vec![self.endpoint.clone()])
        }
    }

    fn now(&self) -> Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: Duration) {
        self.elapsed += duration;
    }
}

fn endpoints() -> AppResult<Endpoints> {
    Ok(Endpoints {
        bluetooth: native::Endpoint {
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

/// Reserve a no-debt recovery for runtime tests without permitting USB traffic.
pub(crate) fn runtime_recovery(parent: &Path) -> AppResult<Recovery> {
    let (recovery, _transcript) = reserve_at(
        &endpoints()?,
        Arc::new(AtomicBool::new(false)),
        Some(&parent.join("runtime-recovery")),
    )?;
    Ok(recovery)
}

struct Harness {
    _directory: tempfile::TempDir,
    endpoints: Endpoints,
    recovery: Recovery,
    original: Option<Recorder<File>>,
    native: Native,
    usb: Usb,
    log: Log,
    cancelled: Arc<AtomicBool>,
}

impl Harness {
    fn new() -> AppResult<Self> {
        let directory = tempfile::tempdir()?;
        let endpoints = endpoints()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (recovery, original) = reserve_at(
            &endpoints,
            Arc::clone(&cancelled),
            Some(&directory.path().join("startup")),
        )?;
        Ok(Self {
            _directory: directory,
            recovery,
            original: Some(original),
            native: Native {
                owners: VecDeque::new(),
                log: Arc::clone(&log),
            },
            usb: Usb {
                owners: VecDeque::new(),
                endpoint: endpoints.control.clone(),
                log: Arc::clone(&log),
                elapsed: Duration::ZERO,
                absent: VecDeque::new(),
                unavailable: false,
            },
            endpoints,
            log,
            cancelled,
        })
    }

    fn connection(&self, name: &'static str, mock: MockTransport) -> FakeConnection {
        FakeConnection {
            name,
            mock,
            log: Arc::clone(&self.log),
            close_failed: false,
            cancel_on_open: None,
            cancel_on_exit: None,
            last_write: Vec::new(),
        }
    }

    async fn prepare(&mut self) -> AppResult<ProvenModem<CaptureTransport<FakeConnection, File>>> {
        prepare_with(
            &mut self.native,
            &mut self.usb,
            &self.endpoints,
            self.original
                .take()
                .ok_or("original capture already consumed")?,
            &mut self.recovery,
            &self.cancelled,
        )
        .await
    }

    fn owe(&mut self, forward: &TerminalPlan) -> AppResult<()> {
        self.recovery.plan = Some(forward.restoration()?);
        self.recovery.report.restoration = RestorationState::Owed;
        self.recovery.journal.planned(forward)?;
        Ok(())
    }

    fn restoration(&mut self, plan: &TerminalPlan) {
        self.usb
            .owners
            .push_back(self.connection("before-restore", identity_script()));
        let mut restore = cat(2);
        restore.expect(b"0M PROGRAM\r", b"0M\r");
        compare(&mut restore, plan);
        changes(&mut restore, plan);
        restore.expect(b"E", &[6]);
        self.usb
            .owners
            .push_back(self.connection("restore", restore));
        self.usb
            .owners
            .push_back(self.connection("after-restore", identity_script()));
        let gateway = if plan.target() == kenwood_tmd750::radio::terminal::TerminalTarget::Off {
            0
        } else {
            2
        };
        self.usb
            .owners
            .push_back(self.connection("verify-restore", cat(gateway)));
    }
}

fn identity_script() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock
}

fn entry_script() -> AppResult<(MockTransport, TerminalPlan)> {
    program_script(0, 1, 1)
}

fn program_script(gateway: u8, route: u8, subtype: u8) -> AppResult<(MockTransport, TerminalPlan)> {
    let snapshot = configuration(gateway, route, subtype)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(gateway);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    changes(&mut mock, &plan);
    mock.expect(b"E", &[6]);
    Ok((mock, plan))
}

#[tokio::test]
async fn one_bluetooth_entry_reuses_owner_for_binary_proof_then_restores_exactly() -> TestResult {
    let mut harness = Harness::new()?;
    harness
        .usb
        .owners
        .push_back(harness.connection("preflight", cat(0)));
    let (mut script, plan) = entry_script()?;
    script.expect(b"\xE0\x03\x00", VERSION);
    harness
        .native
        .owners
        .push_back(harness.connection("bluetooth", script));
    let proof = harness.prepare().await?;
    assert_eq!(harness.recovery.report.restoration, RestorationState::Owed);
    assert!(harness.recovery.report.modem_proved);
    let retired = control::retire(proof.into_transport()).await;
    let released = retired.succeeded();
    harness.recovery.report.retirements.push(retired);
    harness.restoration(&plan.restoration()?);
    harness
        .recovery
        .finish_with(&mut harness.usb, released)
        .await?;
    assert_eq!(
        harness.recovery.report.restoration,
        RestorationState::Verified
    );
    assert!(harness.usb.owners.is_empty());
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    let writes: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Seen::Write("bluetooth", bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(
        writes
            .iter()
            .filter(|bytes| **bytes == b"0M PROGRAM\r")
            .count(),
        1
    );
    let exit = writes
        .iter()
        .position(|bytes| *bytes == b"E")
        .ok_or("entry exit missing")?;
    assert_eq!(
        writes.get(exit + 1..),
        Some([b"\xE0\x03\x00".as_slice()].as_slice())
    );
    let released = log
        .iter()
        .position(|event| *event == Seen::Drop("bluetooth"))
        .ok_or("modem was not dropped")?;
    let restore_open = log
        .iter()
        .position(|event| *event == Seen::Open("before-restore"))
        .ok_or("restoration readiness absent")?;
    assert!(released < restore_open);
    drop(log);
    Ok(())
}

#[tokio::test]
async fn unsupported_usb_identity_prevents_bluetooth_open_or_mcp() -> TestResult {
    let mut harness = Harness::new()?;
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.03\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", b"GW 0\r");
    harness
        .usb
        .owners
        .push_back(harness.connection("preflight", mock));
    assert!(harness.prepare().await.is_err());
    assert!(harness.recovery.report.original_opening.is_none());
    Ok(())
}

#[tokio::test]
async fn cancelled_completed_entry_waits_for_usb_before_exact_restoration() -> TestResult {
    let mut harness = Harness::new()?;
    harness
        .usb
        .owners
        .push_back(harness.connection("preflight", cat(0)));
    let (script, plan) = entry_script()?;
    let mut connection = harness.connection("bluetooth", script);
    connection.cancel_on_exit = Some(Arc::clone(&harness.cancelled));
    harness.native.owners.push_back(connection);
    assert!(harness.prepare().await.is_err());
    assert!(harness.recovery.preparation_released());
    harness.usb.absent = VecDeque::from([true, true, false]);
    harness.restoration(&plan.restoration()?);
    harness.recovery.finish_with(&mut harness.usb, true).await?;
    assert_eq!(
        harness.recovery.report.restoration,
        RestorationState::Verified
    );
    assert_eq!(harness.recovery.report.readiness.len(), 2);
    Ok(())
}

#[tokio::test]
async fn failed_terminal_cleanup_blocks_restoration_and_preserves_separate_evidence() -> TestResult
{
    let mut harness = Harness::new()?;
    harness
        .usb
        .owners
        .push_back(harness.connection("preflight", cat(0)));
    let (script, _plan) = entry_script()?;
    let mut connection = harness.connection("bluetooth", script);
    connection.cancel_on_exit = Some(Arc::clone(&harness.cancelled));
    connection.close_failed = true;
    harness.native.owners.push_back(connection);
    assert!(harness.prepare().await.is_err());
    assert!(harness.recovery.report.transition_cleanup.is_some());
    assert!(!harness.recovery.preparation_released());
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(
        harness.recovery.report.restoration,
        RestorationState::Blocked
    );
    assert!(harness.recovery.report.readiness.is_empty());
    assert!(!harness.recovery.report.restore_errors.is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_pre_restore_usb_keeps_debt_without_opening_mcp() -> TestResult {
    let mut harness = Harness::new()?;
    let (_, plan) = entry_script()?;
    harness.owe(&plan)?;
    harness.usb.unavailable = true;
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.restoration, RestorationState::Owed);
    assert!(
        harness
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .is_empty()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn failed_runtime_capture_sync_blocks_recovery_before_usb_access() -> TestResult {
    let mut harness = Harness::new()?;
    let (_, plan) = entry_script()?;
    harness.owe(&plan)?;
    let (socket, _peer) = std::os::unix::net::UnixStream::pair()?;
    let descriptor: std::os::fd::OwnedFd = socket.into();
    harness.recovery.runtime_capture = Some(File::from(descriptor));
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert!(harness.recovery.report.runtime_capture_error.is_some());
    assert_eq!(
        harness.recovery.report.restoration,
        RestorationState::Blocked
    );
    assert!(
        harness
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn retained_signal_owner_preserves_late_output_and_listener_error() -> TestResult {
    for signal in [Ok(()), Err(io::Error::other("listener unavailable"))] {
        let cancelled = AtomicBool::new(false);
        let joined = AtomicBool::new(false);
        let operation = async {
            assert!(cancelled.load(Ordering::Relaxed));
            tokio::task::yield_now().await;
            joined.store(true, Ordering::Relaxed);
            41
        };
        let failed = signal.is_err();
        let (value, error) =
            finish_on_interrupt(operation, std::future::ready(signal), &cancelled).await;
        assert_eq!(value, 41);
        assert!(joined.load(Ordering::Relaxed));
        assert_eq!(error.is_some(), failed);
    }
    Ok(())
}

#[tokio::test]
async fn late_ready_proof_after_cancellation_is_closed_before_returning_error() -> TestResult {
    let mut harness = Harness::new()?;
    let mut mock = MockTransport::new();
    mock.expect(b"\xE0\x03\x00", VERSION);
    let owner = CaptureTransport::required(
        harness.connection("late", mock),
        harness.original.take().ok_or("capture missing")?,
    );
    let proof = ProvenModem::probe(owner, Duration::from_secs(1))
        .await
        .map_err(|(_, error)| error)?;
    harness.cancelled.store(true, Ordering::Relaxed);
    let result = finish_preparation(
        Ok(proof),
        harness.recovery,
        &mut harness.usb,
        &harness.cancelled,
    )
    .await;
    assert!(result.is_err());
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    assert_eq!(log.last(), Some(&Seen::Drop("late")));
    assert!(log.contains(&Seen::Close("late")));
    drop(log);
    Ok(())
}

#[tokio::test]
async fn active_terminal_preserves_noop_and_restores_only_its_own_route_change() -> TestResult {
    for route in [1, 2] {
        let mut harness = Harness::new()?;
        harness
            .usb
            .owners
            .push_back(harness.connection("preflight", cat(2)));
        let (script, plan) = program_script(2, route, 0)?;
        harness
            .usb
            .owners
            .push_back(harness.connection("active-entry", script));
        let mut binary = MockTransport::new();
        binary.expect(b"\xE0\x03\x00", VERSION);
        harness
            .native
            .owners
            .push_back(harness.connection("bluetooth", binary));
        let proof = harness.prepare().await?;
        let changed = route != 2;
        assert_eq!(harness.recovery.journal.write_started, changed);
        assert_eq!(
            harness.recovery.report.restoration,
            if changed {
                RestorationState::Owed
            } else {
                RestorationState::NotRequired
            }
        );
        let retired = control::retire(proof.into_transport()).await;
        assert!(retired.succeeded());
        harness.recovery.report.retirements.push(retired);
        if changed {
            let inverse = plan.restoration()?;
            assert_eq!(
                inverse.target(),
                kenwood_tmd750::radio::terminal::TerminalTarget::ReflectorTerminal
            );
            assert_eq!(inverse.target_route(), plan.route());
            harness.restoration(&inverse);
        }
        harness.recovery.finish_with(&mut harness.usb, true).await?;
        assert_eq!(
            harness.recovery.report.restoration,
            if changed {
                RestorationState::Verified
            } else {
                RestorationState::NotRequired
            }
        );
        assert!(harness.usb.owners.is_empty());
        let log = harness.log.lock().map_err(|_| "test log poisoned")?;
        let writes: Vec<_> = log
            .iter()
            .filter_map(|event| match event {
                Seen::Write("bluetooth", bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(writes, [b"\xE0\x03\x00".as_slice()]);
        drop(log);
    }
    Ok(())
}

#[tokio::test]
async fn restoration_guard_conflict_closes_without_writing_or_clearing_debt() -> TestResult {
    let mut harness = Harness::new()?;
    let (_, forward) = entry_script()?;
    harness.owe(&forward)?;
    let plan = forward.restoration()?;
    harness
        .usb
        .owners
        .push_back(harness.connection("before-restore", identity_script()));
    let mut restore = cat(2);
    restore.expect(b"0M PROGRAM\r", b"0M\r");
    for (index, page) in plan.replacements().iter().enumerate() {
        let mut bytes = page.expected().to_vec();
        if index == 3 {
            *bytes.last_mut().ok_or("guard byte missing")? ^= 1;
        }
        read(&mut restore, page.page(), &bytes);
    }
    restore.expect(b"E", &[6]);
    harness
        .usb
        .owners
        .push_back(harness.connection("restore", restore));
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.restoration, RestorationState::Owed);
    assert_eq!(harness.recovery.report.readiness.len(), 1);
    assert!(
        harness
            .recovery
            .report
            .retirements
            .iter()
            .all(control::Retirement::succeeded)
    );
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    assert!(!log.iter().any(
        |event| matches!(event, Seen::Write("restore", bytes) if bytes.first() == Some(&b'W'))
    ));
    assert_eq!(log.last(), Some(&Seen::Drop("restore")));
    drop(log);
    Ok(())
}

#[tokio::test]
async fn restored_pages_do_not_clear_debt_after_failed_close_readiness_or_gateway() -> TestResult {
    #[derive(Clone, Copy, Debug)]
    enum Fault {
        Close,
        Readiness,
        Gateway,
    }
    for stage in [Fault::Close, Fault::Readiness, Fault::Gateway] {
        let mut harness = Harness::new()?;
        let (_, forward) = entry_script()?;
        harness.owe(&forward)?;
        harness.restoration(&forward.restoration()?);
        match stage {
            Fault::Close => {
                harness
                    .usb
                    .owners
                    .get_mut(1)
                    .ok_or("restore owner missing")?
                    .close_failed = true;
            }
            Fault::Readiness => {
                let mut mismatch = MockTransport::new();
                mismatch.expect(b"ID\r", b"ID TM-D750\r");
                mismatch.expect(b"FV\r", b"FV 1.03\r");
                mismatch.expect(b"TY\r", b"TY K,2,1\r");
                harness
                    .usb
                    .owners
                    .get_mut(2)
                    .ok_or("readiness owner missing")?
                    .mock = mismatch;
            }
            Fault::Gateway => {
                harness
                    .usb
                    .owners
                    .get_mut(3)
                    .ok_or("verification owner missing")?
                    .mock = cat(2);
            }
        }
        assert!(
            harness
                .recovery
                .finish_with(&mut harness.usb, true)
                .await
                .is_err(),
            "{stage:?}"
        );
        assert_eq!(
            harness.recovery.report.restoration,
            if matches!(stage, Fault::Close) {
                RestorationState::Blocked
            } else {
                RestorationState::Owed
            },
            "{stage:?}"
        );
        assert_eq!(
            harness.recovery.report.owner_released,
            Some(!matches!(stage, Fault::Close))
        );
        assert_eq!(harness.recovery.report.restore_errors.len(), 1);
        assert!(!harness.recovery.report.retirements.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn mismatched_pre_restore_identity_refuses_mcp_and_retains_debt() -> TestResult {
    let mut harness = Harness::new()?;
    let (_, plan) = entry_script()?;
    harness.owe(&plan)?;
    let mut mismatch = MockTransport::new();
    mismatch.expect(b"ID\r", b"ID TM-D750\r");
    mismatch.expect(b"FV\r", b"FV 1.03\r");
    mismatch.expect(b"TY\r", b"TY K,2,1\r");
    harness
        .usb
        .owners
        .push_back(harness.connection("before-restore", mismatch));
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.restoration, RestorationState::Owed);
    assert_eq!(harness.recovery.report.readiness.len(), 1);
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    assert!(
        !log.iter()
            .any(|event| matches!(event, Seen::Write(_, bytes) if bytes == b"0M PROGRAM\r"))
    );
    assert_eq!(log.last(), Some(&Seen::Drop("before-restore")));
    drop(log);
    Ok(())
}

#[test]
fn failed_final_report_publication_never_claims_verified_recovery() -> TestResult {
    let mut harness = Harness::new()?;
    harness.recovery.report.restoration = RestorationState::Verified;
    harness.recovery.report_file = File::open(harness.recovery.directory.join("report.json"))?;
    assert!(harness.recovery.publish().is_err());
    assert_eq!(harness.recovery.report.restoration, RestorationState::Owed);
    assert_eq!(harness.recovery.report.errors.len(), 1);
    Ok(())
}

#[test]
fn journal_and_report_publication_failures_remain_independent() -> TestResult {
    let mut harness = Harness::new()?;
    let (_, plan) = entry_script()?;
    let path = harness.recovery.directory.join("report.json");
    let mut journal = Journal::new(Recorder::named(
        File::open(&path)?,
        Arc::clone(&harness.cancelled),
        "unwritable-journal.jsonl",
    ));
    assert!(journal.planned(&plan).is_err());
    harness.recovery.journal = journal;
    harness.recovery.report_file = File::open(path)?;
    harness.recovery.report.restoration = RestorationState::Verified;
    let error = harness
        .recovery
        .publish()
        .err()
        .ok_or("publication unexpectedly succeeded")?;
    assert!(error.to_string().contains("journal synchronization:"));
    assert!(error.to_string().contains("report publication:"));
    assert_eq!(harness.recovery.report.restoration, RestorationState::Owed);
    assert_eq!(harness.recovery.report.errors.len(), 2);
    Ok(())
}

fn failed_admission_is_retained(recovery: &Recovery) -> TestResult {
    assert_eq!(recovery.report.control_admission_errors.len(), 1);
    let primary = recovery
        .report
        .control_admission_errors
        .first()
        .ok_or("admission failure missing")?;
    assert!(primary.message.contains("admission cancelled"));
    let retired = recovery
        .report
        .retirements
        .first()
        .ok_or("admission cleanup missing")?;
    assert!(retired.close_error.is_some());
    assert!(retired.capture_error.is_none());
    assert!(retired.transcript.complete);
    Ok(())
}

#[tokio::test]
async fn cancelled_usb_preflight_admission_retains_failed_close_and_sends_no_commands() -> TestResult
{
    let mut harness = Harness::new()?;
    let mut owner = harness.connection("late-preflight", MockTransport::new());
    owner.cancel_on_open = Some(Arc::clone(&harness.cancelled));
    owner.close_failed = true;
    harness.usb.owners.push_back(owner);
    let error = harness
        .prepare()
        .await
        .err()
        .ok_or("preflight unexpectedly succeeded")?;
    assert!(error.downcast_ref::<control::OpenFailure>().is_some());
    assert!(!harness.recovery.preparation_released());
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.owner_released, Some(false));
    assert!(harness.recovery.report.original_opening.is_none());
    failed_admission_is_retained(&harness.recovery)?;
    harness.recovery.publish()?;
    let report: serde_json::Value =
        serde_json::from_reader(File::open(harness.recovery.directory.join("report.json"))?)?;
    assert_eq!(
        report.pointer("/owner_released"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(
        report
            .pointer("/retirements/0/close_error/message")
            .is_some()
    );
    assert!(
        report
            .pointer("/control_admission_errors/0/message")
            .is_some()
    );
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    assert_eq!(
        *log,
        [
            Seen::Open("late-preflight"),
            Seen::Close("late-preflight"),
            Seen::Drop("late-preflight")
        ]
    );
    drop(log);
    Ok(())
}

#[tokio::test]
async fn cancelled_active_usb_admission_retains_failed_close_and_retires_bluetooth() -> TestResult {
    let mut harness = Harness::new()?;
    harness
        .usb
        .owners
        .push_back(harness.connection("preflight", cat(2)));
    harness
        .native
        .owners
        .push_back(harness.connection("bluetooth", MockTransport::new()));
    let mut owner = harness.connection("late-active", MockTransport::new());
    owner.cancel_on_open = Some(Arc::clone(&harness.cancelled));
    owner.close_failed = true;
    harness.usb.owners.push_back(owner);
    let error = harness
        .prepare()
        .await
        .err()
        .ok_or("active entry unexpectedly succeeded")?;
    assert!(error.downcast_ref::<control::OpenFailure>().is_some());
    assert!(!harness.recovery.preparation_released());
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.owner_released, Some(false));
    assert!(!harness.recovery.journal.write_started);
    failed_admission_is_retained(&harness.recovery)?;
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    assert!(
        !log.iter()
            .any(|event| matches!(event, Seen::Write(name, _) if *name != "preflight"))
    );
    assert!(log.contains(&Seen::Close("bluetooth")));
    assert_eq!(log.last(), Some(&Seen::Drop("bluetooth")));
    drop(log);
    Ok(())
}

#[tokio::test]
async fn cancelled_restoration_usb_admission_retains_failed_close_and_existing_debt() -> TestResult
{
    let mut harness = Harness::new()?;
    let (_, forward) = entry_script()?;
    harness.owe(&forward)?;
    harness.recovery.report.owner_released = Some(true);
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut owner = harness.connection("late-restore", MockTransport::new());
    owner.cancel_on_open = Some(Arc::clone(&cancelled));
    owner.close_failed = true;
    harness.usb.owners.push_back(owner);
    let error = harness
        .recovery
        .restore_pages(&mut harness.usb, &forward.restoration()?, &cancelled)
        .await
        .err()
        .ok_or("restoration admission unexpectedly succeeded")?;
    assert!(error.downcast_ref::<control::OpenFailure>().is_some());
    assert!(!harness.recovery.preparation_released());
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.owner_released, Some(false));
    assert_eq!(
        harness.recovery.report.restoration,
        RestorationState::Blocked
    );
    assert!(harness.recovery.report.readiness.is_empty());
    failed_admission_is_retained(&harness.recovery)?;
    let log = harness.log.lock().map_err(|_| "test log poisoned")?;
    assert_eq!(
        *log,
        [
            Seen::Open("late-restore"),
            Seen::Close("late-restore"),
            Seen::Drop("late-restore")
        ]
    );
    drop(log);
    Ok(())
}

#[tokio::test]
async fn failed_readiness_close_cannot_leave_owner_release_true() -> TestResult {
    for before_restore in [true, false] {
        let mut harness = Harness::new()?;
        let (_, forward) = entry_script()?;
        harness.owe(&forward)?;
        if before_restore {
            let mut owner = harness.connection("before-restore", identity_script());
            owner.close_failed = true;
            harness.usb.owners.push_back(owner);
        } else {
            harness.restoration(&forward.restoration()?);
            harness
                .usb
                .owners
                .get_mut(2)
                .ok_or("readiness owner missing")?
                .close_failed = true;
        }
        assert!(
            harness
                .recovery
                .finish_with(&mut harness.usb, true)
                .await
                .is_err()
        );
        assert_eq!(harness.recovery.report.owner_released, Some(false));
        assert_eq!(
            harness.recovery.report.restoration,
            RestorationState::Blocked
        );
        assert!(harness.recovery.report.control.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn incomplete_readiness_capture_cannot_claim_owner_release() -> TestResult {
    let mut harness = Harness::new()?;
    let (_, forward) = entry_script()?;
    harness.owe(&forward)?;
    harness.recovery.before_restore_readiness = Some(Recorder::named(
        File::open(
            harness
                .recovery
                .directory
                .join("before-restore-readiness.jsonl"),
        )?,
        Arc::clone(&harness.cancelled),
        "before-restore-readiness.jsonl",
    ));
    assert!(
        harness
            .recovery
            .finish_with(&mut harness.usb, true)
            .await
            .is_err()
    );
    assert_eq!(harness.recovery.report.owner_released, Some(false));
    assert_eq!(
        harness.recovery.report.restoration,
        RestorationState::Blocked
    );
    assert!(
        harness
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .is_empty()
    );
    Ok(())
}

#[test]
fn native_history_requires_host_retirement_not_remote_channel_confirmation() -> TestResult {
    for (error, released) in [
        (
            TransportError::BluetoothOpenWithCleanup {
                stage: BluetoothOpenStage::RfcommCompletion,
                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
            },
            true,
        ),
        (
            TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ChannelUnconfirmed,
            },
            false,
        ),
        (
            TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ReapPending,
            },
            false,
        ),
    ] {
        let mut harness = Harness::new()?;
        let transcript = harness
            .original
            .as_ref()
            .ok_or("opening capture missing")?
            .summary();
        harness.recovery.report.original_opening = Some(opening::History {
            attempts: vec![opening::Attempt {
                number: 1,
                started: true,
                resolved: None,
                error: Some(native::OpenFailure::from_error(&error)),
                interruption: None,
            }],
            retry_error: None,
            capture_error: None,
            transcript,
        });
        assert_eq!(harness.recovery.preparation_released(), released, "{error}");
    }
    Ok(())
}
