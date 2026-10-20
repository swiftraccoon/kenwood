//! Startup and restoration over fake control and modem hosts: the Bluetooth
//! entry path, the active control-endpoint path, and restoration state.

use std::collections::VecDeque;
use std::io;
use std::num::NonZeroU32;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use kenwood_transport::{MockTransport, Transport, TransportError};

use super::*;
use crate::protocol::mcp::{ACK, Header, regions};
use crate::radio::menu::MenuFieldSnapshot;
use crate::radio::programming::{McpJournal, PageReplacement};
use crate::radio::readiness::{SETTLE_QUIET, SettleOutcome};
use crate::radio::terminal::session::TerminalPhase;
use crate::transport::{DEFAULT_BAUD, KENWOOD_VID, TMD750_PANEL_PID};
use crate::types::{FirmwareIdentity, Page, RadioModel, RadioType, Region};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VERSION: &[u8] = b"\xE0\x0E\x00\x01MMDVM 2018";
const GET_VERSION: &[u8] = b"\xE0\x03\x00";

fn identity() -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.control-panel".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_PANEL_PID),
    }
}

fn configuration(
    gateway: u8,
    route: u8,
    subtype: u8,
) -> Result<MenuFieldSnapshot, Box<dyn std::error::Error>> {
    let mut pages: Vec<(Page, Vec<u8>)> = regions::menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .map(|page| (page, vec![0xA5; page.len()]))
        .collect();
    for (address, value) in [
        (10, 0),
        (323_593, 0),
        (329_031, 0),
        (329_037, route),
        (331_776, gateway),
        (331_778, subtype),
    ] {
        let (page, bytes) = pages
            .iter_mut()
            .find(|(page, _)| (page.address().as_u32()..page.end()).contains(&address))
            .ok_or("configuration guard page missing")?;
        let offset = usize::try_from(address - page.address().as_u32())?;
        *bytes.get_mut(offset).ok_or("guard byte missing")? = value;
    }
    Ok(MenuFieldSnapshot::from_pages(pages)?)
}

fn cat(gateway: u8) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
    mock
}

fn identity_only() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock
}

/// [`cat`] with the `ID` reply held for `hold`, as a Bluetooth connection
/// opened soon after another one closed holds its first reply.
fn held_cat(gateway: u8, hold: std::time::Duration) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect_reads(b"ID\r", []);
    mock.queue_read_delayed(b"ID TM-D750\r", hold);
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
    mock
}

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = Header::write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&Header::read_request(page), frame(page, bytes));
    mock.expect(&[ACK], &[ACK]);
}

fn program_into(
    mock: &mut MockTransport,
    snapshot: &MenuFieldSnapshot,
    plan: &TerminalPlan,
    backup: bool,
) {
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    if backup {
        for (page, bytes) in snapshot.pages() {
            read(mock, *page, bytes);
        }
    }
    for page in plan.replacements() {
        read(mock, page.page(), page.expected());
    }
    for page in plan.replacements().iter().filter(|page| !page.is_noop()) {
        mock.expect(frame(page.page(), page.replacement()), &[ACK]);
        read(mock, page.page(), page.replacement());
    }
    mock.expect(b"E", &[ACK]);
}

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    ControlOpen,
    ModemOpen,
    Close(&'static str),
    Drop(&'static str),
    Stage(ControlStage),
}

type Log = Arc<Mutex<Vec<Seen>>>;

#[derive(Debug)]
struct Conn {
    tag: &'static str,
    mock: MockTransport,
    log: Log,
}

impl Transport for Conn {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.mock.write(bytes).await
    }
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        push(&self.log, Seen::Close(self.tag));
        self.mock.close().await
    }
    fn set_baud_rate(&mut self, baud: NonZeroU32) -> Result<(), TransportError> {
        self.mock.set_baud_rate(baud)
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        push(&self.log, Seen::Drop(self.tag));
    }
}

fn push(log: &Log, event: Seen) {
    if let Ok(mut events) = log.lock() {
        events.push(event);
    }
}

struct FakeControl {
    scripts: VecDeque<(&'static str, MockTransport)>,
    log: Log,
    elapsed: std::time::Duration,
    enumerations: usize,
    /// Enumerations after this many omit the endpoint.
    absent_after: Option<usize>,
    fail_close_tag: Option<&'static str>,
}

impl FakeControl {
    fn new(scripts: impl IntoIterator<Item = (&'static str, MockTransport)>, log: &Log) -> Self {
        Self {
            scripts: scripts.into_iter().collect(),
            log: Arc::clone(log),
            elapsed: std::time::Duration::ZERO,
            enumerations: 0,
            absent_after: None,
            fail_close_tag: None,
        }
    }

    fn control_opens(&self) -> Result<usize, Box<dyn std::error::Error>> {
        Ok(self
            .log
            .lock()
            .map_err(|_| "log poisoned")?
            .iter()
            .filter(|event| **event == Seen::ControlOpen)
            .count())
    }
}

impl ControlHost for FakeControl {
    type Connection = Conn;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        self.enumerations += 1;
        if self
            .absent_after
            .is_some_and(|count| self.enumerations > count)
        {
            return Ok(Vec::new());
        }
        Ok(vec![endpoint()])
    }

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: NonZeroU32,
    ) -> Result<Conn, TransportError> {
        assert_eq!(*selected, endpoint());
        assert_eq!(baud.get(), 9600);
        push(&self.log, Seen::ControlOpen);
        let (tag, mock) = self
            .scripts
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected control open"),
            })?;
        Ok(Conn {
            tag,
            mock,
            log: Arc::clone(&self.log),
        })
    }

    fn now(&self) -> std::time::Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: std::time::Duration) {
        self.elapsed += duration;
    }

    fn stage(&mut self, stage: ControlStage) {
        push(&self.log, Seen::Stage(stage));
    }

    async fn close(&mut self, connection: Conn) -> Result<(), CloseFailure> {
        let fail = self.fail_close_tag == Some(connection.tag);
        crate::radio::readiness::close_within(connection, crate::radio::readiness::CLOSE_BUDGET)
            .await?;
        if fail {
            Err(CloseFailure::Host(
                io::Error::other("scripted control release failure").into(),
            ))
        } else {
            Ok(())
        }
    }
}

struct FakeModem {
    opens: VecDeque<(&'static str, MockTransport)>,
    log: Log,
}

impl ModemHost for FakeModem {
    type Connection = Conn;

    async fn open(&mut self, _cancelled: &AtomicBool) -> Result<Conn, ModemOpenFailure> {
        push(&self.log, Seen::ModemOpen);
        let (tag, mock) = self.opens.pop_front().ok_or_else(|| ModemOpenFailure {
            source: io::Error::other("unexpected modem open").into(),
            retry_allowed: false,
            released: true,
        })?;
        Ok(Conn {
            tag,
            mock,
            log: Arc::clone(&self.log),
        })
    }

    async fn reopen(
        &mut self,
        _deadline: tokio::time::Instant,
        cancelled: &AtomicBool,
    ) -> Result<Conn, ModemOpenFailure> {
        self.open(cancelled).await.map_err(|mut error| {
            error.retry_allowed = true;
            error
        })
    }

    async fn wait(&mut self, _duration: std::time::Duration) {}
}

#[derive(Default)]
struct RecordingJournal {
    backups: usize,
    plans: usize,
    entry_writes: usize,
    restore_writes: usize,
    checkpoints: Vec<(TerminalPhase, bool)>,
    fail_checkpoint: bool,
}

impl TerminalJournal for RecordingJournal {
    fn backup(&mut self, _identity: &Identity, _snapshot: &MenuFieldSnapshot) -> io::Result<()> {
        self.backups += 1;
        Ok(())
    }
    fn planned(&mut self, _plan: &TerminalPlan) -> io::Result<()> {
        self.plans += 1;
        Ok(())
    }
    fn before_write(&mut self, phase: TerminalPhase, _page: &PageReplacement) -> io::Result<()> {
        match phase {
            TerminalPhase::Entry => self.entry_writes += 1,
            TerminalPhase::Restore => self.restore_writes += 1,
        }
        Ok(())
    }
    fn checkpoint(
        &mut self,
        phase: TerminalPhase,
        acknowledged: bool,
        _journal: &McpJournal,
    ) -> io::Result<()> {
        self.checkpoints.push((phase, acknowledged));
        if self.fail_checkpoint {
            Err(io::Error::other("scripted checkpoint failure"))
        } else {
            Ok(())
        }
    }
}

fn lifecycle() -> TerminalLifecycle {
    TerminalLifecycle::new(endpoint(), DEFAULT_BAUD, TerminalGatewayRoute::Bluetooth)
}

#[tokio::test]
async fn bluetooth_entry_writes_over_the_modem_then_proves_and_restores() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let forward =
        TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let restore = forward.restoration()?;
    let log: Log = Arc::default();

    let mut bluetooth = cat(0);
    program_into(&mut bluetooth, &snapshot, &forward, true);
    bluetooth.expect(GET_VERSION, VERSION);

    let mut control = FakeControl::new([("preflight", cat(0))], &log);
    let mut modem = FakeModem {
        opens: VecDeque::from([("bluetooth", bluetooth)]),
        log: Arc::clone(&log),
    };
    let mut journal = RecordingJournal::default();

    let mut startup = lifecycle()
        .prepare(
            &mut control,
            &mut modem,
            &mut journal,
            &AtomicBool::new(false),
        )
        .await;
    assert!(startup.succeeded(), "{:?}", startup.error);
    assert_eq!(startup.recovery.state(), RestorationState::Owed);
    assert_eq!(journal.backups, 1);
    assert_eq!(journal.plans, 1);
    assert!(journal.entry_writes >= 1);
    assert_eq!(journal.checkpoints, [(TerminalPhase::Entry, true)]);
    let proof = startup.proof.ok_or("missing proof")?;
    assert_eq!(proof.version().description(), "MMDVM 2018");
    crate::radio::readiness::close_within(
        proof.into_transport(),
        std::time::Duration::from_secs(1),
    )
    .await?;

    let mut restore_script = cat(2);
    program_into(&mut restore_script, &snapshot, &restore, false);
    control.scripts = VecDeque::from([
        ("readiness-before", identity_only()),
        ("restore", restore_script),
        ("readiness-after", cat(0)),
    ]);
    let opens_before = control.control_opens()?;

    let report = startup
        .recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert!(report.verified(), "{report:?}");
    assert!(journal.restore_writes >= 1);
    assert_eq!(
        journal.checkpoints,
        [(TerminalPhase::Entry, true), (TerminalPhase::Restore, true)]
    );
    let restored_gateway = report
        .readiness_after
        .as_ref()
        .and_then(|readiness| readiness.attempts.last())
        .and_then(|attempt| attempt.connection.as_ref())
        .and_then(|connection| connection.gateway);
    assert_eq!(
        restored_gateway,
        Some(DvGatewayMode::Off),
        "the matching post-restoration connection read the restored Gateway"
    );
    let settle = report.settle.as_ref().ok_or("settle wait missing")?;
    assert!(
        matches!(settle.outcome, SettleOutcome::Settled),
        "the operation-panel control endpoint settles before programming: {settle:?}"
    );
    assert_eq!(settle.elapsed, SETTLE_QUIET);
    assert_eq!(
        control.control_opens()? - opens_before,
        3,
        "readiness, restoration and readiness with Gateway; no separate verification open"
    );
    let events = log.lock().map_err(|_| "log poisoned")?;
    let stages_and_drop = [
        events.contains(&Seen::Stage(ControlStage::Preflight)),
        events.contains(&Seen::Stage(ControlStage::ReadinessAfterRestore)),
        events.contains(&Seen::Drop("bluetooth")),
    ];
    drop(events);
    assert!(
        stages_and_drop.iter().all(|seen| *seen),
        "preflight, post-restoration readiness and modem drop are recorded"
    );
    Ok(())
}

#[tokio::test]
async fn a_bluetooth_modem_link_may_hold_its_first_reply_past_the_regular_deadline() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let forward =
        TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let log: Log = Arc::default();
    let hold = crate::radio::DEFAULT_TIMEOUT.saturating_add(std::time::Duration::from_millis(200));
    let mut bluetooth = held_cat(0, hold);
    program_into(&mut bluetooth, &snapshot, &forward, true);
    bluetooth.expect(GET_VERSION, VERSION);
    let mut control = FakeControl::new([("preflight", cat(0))], &log);
    let mut modem = FakeModem {
        opens: VecDeque::from([("bluetooth", bluetooth)]),
        log: Arc::clone(&log),
    };
    let mut journal = RecordingJournal::default();
    let startup = lifecycle()
        .prepare(
            &mut control,
            &mut modem,
            &mut journal,
            &AtomicBool::new(false),
        )
        .await;
    assert!(startup.succeeded(), "{:?}", startup.error);
    let proof = startup.proof.ok_or("missing proof")?;
    crate::radio::readiness::close_within(
        proof.into_transport(),
        std::time::Duration::from_secs(1),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn active_route_writes_over_control_and_leaves_the_modem_link_untouched() -> TestResult {
    let snapshot = configuration(2, 1, 0)?;
    let forward =
        TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let restore = forward.restoration()?;
    let log: Log = Arc::default();

    let mut active_entry = cat(2);
    program_into(&mut active_entry, &snapshot, &forward, true);

    let mut bluetooth = MockTransport::new();
    bluetooth.expect(GET_VERSION, VERSION);

    let mut control = FakeControl::new(
        [("preflight", cat(2)), ("active-entry", active_entry)],
        &log,
    );
    let mut modem = FakeModem {
        opens: VecDeque::from([("bluetooth", bluetooth)]),
        log: Arc::clone(&log),
    };
    let mut journal = RecordingJournal::default();

    let mut startup = lifecycle()
        .prepare(
            &mut control,
            &mut modem,
            &mut journal,
            &AtomicBool::new(false),
        )
        .await;
    assert!(startup.succeeded(), "{:?}", startup.error);
    assert_eq!(startup.recovery.state(), RestorationState::Owed);
    let (control_opens, active_entry) = {
        let events = log.lock().map_err(|_| "log poisoned")?;
        (
            events
                .iter()
                .filter(|event| **event == Seen::ControlOpen)
                .count(),
            events.contains(&Seen::Stage(ControlStage::ActiveEntry)),
        )
    };
    assert_eq!(
        control_opens, 2,
        "preflight and active entry both open control"
    );
    assert!(active_entry, "programming ran over the control endpoint");
    let settle = startup
        .settle
        .as_ref()
        .ok_or("active-entry settle missing")?;
    assert!(
        matches!(settle.outcome, SettleOutcome::Settled),
        "the operation-panel control endpoint settles before active entry: {settle:?}"
    );
    let proof = startup.proof.ok_or("missing proof")?;
    crate::radio::readiness::close_within(
        proof.into_transport(),
        std::time::Duration::from_secs(1),
    )
    .await?;

    let mut restore_script = cat(2);
    program_into(&mut restore_script, &snapshot, &restore, false);
    control.scripts = VecDeque::from([
        ("readiness-before", identity_only()),
        ("restore", restore_script),
        ("readiness-after", cat(2)),
    ]);
    let report = startup
        .recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert!(report.verified(), "{report:?}");
    Ok(())
}

#[tokio::test]
async fn an_unsettled_active_route_control_endpoint_is_never_programmed() -> TestResult {
    let log: Log = Arc::default();
    let mut control = FakeControl::new([("preflight", cat(2))], &log);
    control.absent_after = Some(0);
    let mut modem = FakeModem {
        opens: VecDeque::from([("bluetooth", MockTransport::new())]),
        log: Arc::clone(&log),
    };
    let mut journal = RecordingJournal::default();
    let startup = lifecycle()
        .prepare(
            &mut control,
            &mut modem,
            &mut journal,
            &AtomicBool::new(false),
        )
        .await;
    assert!(
        matches!(startup.error, Some(LifecycleError::ControlNotSettled)),
        "{:?}",
        startup.error
    );
    assert_eq!(startup.recovery.state(), RestorationState::NotRequired);
    assert_eq!(journal.backups, 0, "no programming session began");
    assert_eq!(control.control_opens()?, 1, "only the preflight opened");
    let settle = startup
        .settle
        .as_ref()
        .ok_or("active-entry settle missing")?;
    assert!(
        matches!(
            settle.outcome,
            SettleOutcome::Failed(ReadinessError::Unsettled { .. })
        ),
        "{settle:?}"
    );
    Ok(())
}

#[tokio::test]
async fn unsupported_identity_stops_before_opening_the_modem() -> TestResult {
    let mut wrong = MockTransport::new();
    wrong.expect(b"ID\r", b"ID TM-D750\r");
    wrong.expect(b"FV\r", b"FV 1.03\r");
    wrong.expect(b"TY\r", b"TY K,2,1\r");
    wrong.expect(b"GW\r", b"GW 0\r");
    let log: Log = Arc::default();
    let mut control = FakeControl::new([("preflight", wrong)], &log);
    let mut modem = FakeModem {
        opens: VecDeque::new(),
        log: Arc::clone(&log),
    };
    let mut journal = RecordingJournal::default();
    let startup = lifecycle()
        .prepare(
            &mut control,
            &mut modem,
            &mut journal,
            &AtomicBool::new(false),
        )
        .await;
    assert!(!startup.succeeded());
    assert!(matches!(
        startup.error,
        Some(LifecycleError::UnsupportedTarget { .. })
    ));
    assert_eq!(startup.recovery.state(), RestorationState::NotRequired);
    let events = log.lock().map_err(|_| "log poisoned")?;
    let opened_modem = events.contains(&Seen::ModemOpen);
    drop(events);
    assert!(!opened_modem, "no modem open on refusal");
    Ok(())
}

#[tokio::test]
async fn finish_without_release_blocks_restoration_and_sends_no_traffic() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let forward =
        TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let log: Log = Arc::default();
    let mut recovery = TerminalRecovery {
        control_endpoint: endpoint(),
        baud: DEFAULT_BAUD,
        owed: Some(OwedRestoration {
            identity: identity()?,
            plan: forward.restoration()?,
        }),
        state: RestorationState::Owed,
    };
    let mut control = FakeControl::new([], &log);
    let mut journal = RecordingJournal::default();
    let report = recovery
        .finish(&mut control, &mut journal, false, &AtomicBool::new(false))
        .await;
    assert_eq!(report.state, RestorationState::Blocked);
    assert!(matches!(
        report.error,
        Some(RestorationError::ModemNotReleased)
    ));
    assert!(log.lock().map_err(|_| "log poisoned")?.is_empty());
    Ok(())
}

#[tokio::test]
async fn nothing_owed_finishes_verified_without_traffic() -> TestResult {
    let log: Log = Arc::default();
    let mut recovery = TerminalRecovery {
        control_endpoint: endpoint(),
        baud: DEFAULT_BAUD,
        owed: None,
        state: RestorationState::NotRequired,
    };
    let mut control = FakeControl::new([], &log);
    let mut journal = RecordingJournal::default();
    let report = recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert!(report.verified());
    assert!(log.lock().map_err(|_| "log poisoned")?.is_empty());
    Ok(())
}

/// A recovery owing the reverse of the Bluetooth entry from Gateway Off.
fn owed_recovery()
-> Result<(MenuFieldSnapshot, TerminalPlan, TerminalRecovery), Box<dyn std::error::Error>> {
    let snapshot = configuration(0, 1, 1)?;
    let forward =
        TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let restore = forward.restoration()?;
    let recovery = TerminalRecovery {
        control_endpoint: endpoint(),
        baud: DEFAULT_BAUD,
        owed: Some(OwedRestoration {
            identity: identity()?,
            plan: restore.clone(),
        }),
        state: RestorationState::Owed,
    };
    Ok((snapshot, restore, recovery))
}

#[tokio::test]
async fn a_restored_gateway_mismatch_leaves_restoration_owed() -> TestResult {
    let (snapshot, restore, mut recovery) = owed_recovery()?;
    let log: Log = Arc::default();
    let mut restore_script = cat(2);
    program_into(&mut restore_script, &snapshot, &restore, false);
    let mut control = FakeControl::new(
        [
            ("readiness-before", identity_only()),
            ("restore", restore_script),
            ("readiness-after", cat(2)),
        ],
        &log,
    );
    let mut journal = RecordingJournal::default();
    let report = recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert_eq!(report.state, RestorationState::Owed);
    assert!(
        matches!(report.error, Some(RestorationError::GatewayNotVerified)),
        "{report:?}"
    );
    assert_eq!(report.restore, Some(RestoreOutcome::Written));
    assert_eq!(
        control.control_opens()?,
        3,
        "a mismatching Gateway ends readiness without another open"
    );
    Ok(())
}

#[tokio::test]
async fn an_unsettled_control_endpoint_leaves_restoration_owed_without_programming() -> TestResult {
    let (_snapshot, _restore, mut recovery) = owed_recovery()?;
    let log: Log = Arc::default();
    let mut control = FakeControl::new([("readiness-before", identity_only())], &log);
    control.absent_after = Some(1);
    let mut journal = RecordingJournal::default();
    let report = recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert_eq!(report.state, RestorationState::Owed);
    assert!(
        matches!(report.error, Some(RestorationError::ControlNotSettled)),
        "{report:?}"
    );
    assert!(
        report.restore.is_none(),
        "no programming entry was attempted"
    );
    assert_eq!(journal.restore_writes, 0);
    assert_eq!(
        control.control_opens()?,
        1,
        "only the readiness connection opened"
    );
    let settle = report.settle.as_ref().ok_or("settle wait missing")?;
    assert!(
        matches!(
            settle.outcome,
            SettleOutcome::Failed(ReadinessError::Unsettled { .. })
        ),
        "{settle:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_entry_checkpoint_keeps_the_reverse_plan_on_both_routes() -> TestResult {
    for gateway in [0, 2] {
        let snapshot = configuration(gateway, 1, 0)?;
        let forward =
            TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
        let reverse = forward.restoration()?;
        let log: Log = Arc::default();
        let mut programming = cat(gateway);
        program_into(&mut programming, &snapshot, &forward, true);
        let (scripts, modem_script) = if gateway == 0 {
            (vec![("preflight", cat(0))], programming)
        } else {
            (
                vec![("preflight", cat(2)), ("active-entry", programming)],
                MockTransport::new(),
            )
        };
        let mut control = FakeControl::new(scripts, &log);
        let mut modem = FakeModem {
            opens: VecDeque::from([("modem", modem_script)]),
            log: Arc::clone(&log),
        };
        let mut journal = RecordingJournal {
            fail_checkpoint: true,
            ..RecordingJournal::default()
        };
        let startup = lifecycle()
            .prepare(
                &mut control,
                &mut modem,
                &mut journal,
                &AtomicBool::new(false),
            )
            .await;
        assert_eq!(
            startup.recovery.state(),
            RestorationState::Owed,
            "{startup:?}"
        );
        assert_eq!(startup.recovery.restoration(), Some(&reverse));
        let Some(LifecycleError::Program(error)) = startup.error else {
            return Err("checkpoint failure did not remain the programming cause".into());
        };
        assert!(error.checkpoint.is_some(), "{error:?}");
        assert!(
            error.write_started,
            "the fixture must dispatch changed pages"
        );
        assert!(
            !error.journal.verified.is_empty(),
            "completed readbacks must remain available"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_failed_active_entry_close_retains_restoration_and_prevents_release() -> TestResult {
    let snapshot = configuration(2, 1, 0)?;
    let forward =
        TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let log: Log = Arc::default();
    let mut programming = cat(2);
    program_into(&mut programming, &snapshot, &forward, true);
    let mut control =
        FakeControl::new([("preflight", cat(2)), ("failed-close", programming)], &log);
    control.fail_close_tag = Some("failed-close");
    let mut modem = FakeModem {
        opens: VecDeque::from([("modem", MockTransport::new())]),
        log,
    };
    let mut journal = RecordingJournal::default();
    let startup = lifecycle()
        .prepare(
            &mut control,
            &mut modem,
            &mut journal,
            &AtomicBool::new(false),
        )
        .await;
    assert_eq!(startup.recovery.state(), RestorationState::Blocked);
    assert_eq!(
        startup.recovery.restoration(),
        Some(&forward.restoration()?)
    );
    assert!(
        matches!(startup.error, Some(LifecycleError::ControlClose(_))),
        "{startup:?}"
    );
    assert!(
        !startup.released(),
        "a retained control connection must block restoration"
    );
    Ok(())
}

#[tokio::test]
async fn verified_restoration_is_completed_once_without_repeating_traffic() -> TestResult {
    let (snapshot, restore, mut recovery) = owed_recovery()?;
    let log: Log = Arc::default();
    let mut programming = cat(2);
    program_into(&mut programming, &snapshot, &restore, false);
    let mut control = FakeControl::new(
        [
            ("readiness-before", identity_only()),
            ("restore", programming),
            ("readiness-after", cat(0)),
        ],
        &log,
    );
    let mut journal = RecordingJournal::default();
    assert!(
        recovery
            .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
            .await
            .verified()
    );
    let opens = control.control_opens()?;
    let checkpoints = journal.checkpoints.len();
    assert!(
        recovery.restoration().is_none(),
        "completed recovery must consume its obligation"
    );
    assert!(
        recovery
            .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
            .await
            .verified()
    );
    assert_eq!(
        control.control_opens()?,
        opens,
        "a second completion must not open a connection"
    );
    assert_eq!(
        journal.checkpoints.len(),
        checkpoints,
        "a second completion must not program"
    );
    Ok(())
}
