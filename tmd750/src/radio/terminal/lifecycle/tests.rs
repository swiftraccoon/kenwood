//! Startup and restoration over fake control and modem hosts: the Bluetooth
//! entry path, the active control-endpoint path, and restoration state.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use kenwood_transport::{MockTransport, Transport, TransportError};

use super::*;
use crate::protocol::mcp::{ACK, read_request, regions, write_request};
use crate::radio::menu::MenuFieldSnapshot;
use crate::radio::programming::{McpJournal, PageReplacement};
use crate::radio::terminal::session::TerminalPhase;
use crate::transport::{KENWOOD_VID, TMD750_PANEL_PID};
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

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&read_request(page), &frame(page, bytes));
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
        mock.expect(&frame(page.page(), page.replacement()), &[ACK]);
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
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
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
}

impl ControlHost for FakeControl {
    type Connection = Conn;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        Ok(vec![endpoint()])
    }

    fn open(&mut self, selected: &SerialCandidate, baud: u32) -> Result<Conn, TransportError> {
        assert_eq!(*selected, endpoint());
        assert_eq!(baud, 9600);
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
        Ok(())
    }
}

fn lifecycle() -> TerminalLifecycle {
    TerminalLifecycle::new(endpoint(), 9600, TerminalGatewayRoute::Bluetooth)
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

    let mut control = FakeControl {
        scripts: VecDeque::from([("preflight", cat(0))]),
        log: Arc::clone(&log),
        elapsed: std::time::Duration::ZERO,
    };
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
    assert_eq!(proof.version().description, "MMDVM 2018");
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
        ("readiness-after", identity_only()),
        ("verify", cat(0)),
    ]);

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
    let events = log.lock().map_err(|_| "log poisoned")?;
    let stages_and_drop = [
        events.contains(&Seen::Stage(ControlStage::Preflight)),
        events.contains(&Seen::Stage(ControlStage::Verification)),
        events.contains(&Seen::Drop("bluetooth")),
    ];
    drop(events);
    assert!(
        stages_and_drop.iter().all(|seen| *seen),
        "preflight, verification and modem drop are recorded"
    );
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

    let mut control = FakeControl {
        scripts: VecDeque::from([("preflight", cat(2)), ("active-entry", active_entry)]),
        log: Arc::clone(&log),
        elapsed: std::time::Duration::ZERO,
    };
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
        ("readiness-after", identity_only()),
        ("verify", cat(2)),
    ]);
    let report = startup
        .recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert!(report.verified(), "{report:?}");
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
    let mut control = FakeControl {
        scripts: VecDeque::from([("preflight", wrong)]),
        log: Arc::clone(&log),
        elapsed: std::time::Duration::ZERO,
    };
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
        baud: 9600,
        owed: Some(OwedRestoration {
            identity: identity()?,
            plan: forward.restoration()?,
        }),
        state: RestorationState::Owed,
    };
    let mut control = FakeControl {
        scripts: VecDeque::new(),
        log: Arc::clone(&log),
        elapsed: std::time::Duration::ZERO,
    };
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
        baud: 9600,
        owed: None,
        state: RestorationState::NotRequired,
    };
    let mut control = FakeControl {
        scripts: VecDeque::new(),
        log: Arc::clone(&log),
        elapsed: std::time::Duration::ZERO,
    };
    let mut journal = RecordingJournal::default();
    let report = recovery
        .finish(&mut control, &mut journal, true, &AtomicBool::new(false))
        .await;
    assert!(report.verified());
    assert!(log.lock().map_err(|_| "log poisoned")?.is_empty());
    Ok(())
}
