//! One entry and one restoration pass over a mock connection: the exact wire
//! schedule, that each write is preceded by its recorded intent, and the
//! journal records and outcome each pass produces.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kenwood_transport::{MockTransport, TransportError};

use super::*;
use crate::protocol::mcp::{ACK, read_request, write_request};
use crate::types::{Address, FirmwareIdentity, Page, RadioModel, RadioType, Region};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity() -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

/// A backup snapshot filled with a sentinel byte, with the guard bytes set.
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

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&read_request(page), &frame(page, bytes));
    mock.expect(&[ACK], &[ACK]);
}

fn backup(mock: &mut MockTransport, snapshot: &MenuFieldSnapshot) {
    for (page, bytes) in snapshot.pages() {
        read(mock, *page, bytes);
    }
}

fn compare(mock: &mut MockTransport, plan: &TerminalPlan) {
    for page in plan.replacements() {
        read(mock, page.page(), page.expected());
    }
}

fn changes(mock: &mut MockTransport, plan: &TerminalPlan) {
    for page in plan.replacements().iter().filter(|page| !page.is_noop()) {
        mock.expect(&frame(page.page(), page.replacement()), &[ACK]);
        read(mock, page.page(), page.replacement());
    }
}

/// One entry against a snapshot, with the whole wire schedule scripted.
fn entry_wire(
    gateway: u8,
    route: u8,
    subtype: u8,
) -> Result<(MockTransport, TerminalPlan), Box<dyn std::error::Error>> {
    let snapshot = configuration(gateway, route, subtype)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(gateway);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    changes(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    Ok((mock, plan))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Backup(usize),
    Planned,
    Intent {
        phase: TerminalPhase,
        address: u32,
    },
    Write(Vec<u8>),
    Checkpoint {
        acknowledged: bool,
        verified: Vec<u32>,
    },
}

type Log = Arc<Mutex<Vec<Event>>>;

/// Records write frames onto the shared log so intents and writes interleave.
struct LoggingTransport {
    mock: MockTransport,
    log: Log,
}

impl Transport for LoggingTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if bytes.first() == Some(&b'W') {
            self.log
                .lock()
                .map_err(|_| TransportError::Write(io::Error::other("log poisoned")))?
                .push(Event::Write(bytes.to_vec()));
        }
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.mock.set_baud_rate(baud)
    }
}

/// When a journal record fails, which one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum JournalFault {
    #[default]
    None,
    Backup,
    Planned,
    BeforeWrite,
    Checkpoint,
}

struct LogJournal {
    log: Log,
    fault: JournalFault,
    write_started: bool,
    exit_acknowledged: bool,
}

impl LogJournal {
    fn new(log: &Log) -> Self {
        Self {
            log: Arc::clone(log),
            fault: JournalFault::None,
            write_started: false,
            exit_acknowledged: false,
        }
    }

    fn push(&self, event: Event) -> io::Result<()> {
        self.log
            .lock()
            .map_err(|_| io::Error::other("log poisoned"))?
            .push(event);
        Ok(())
    }

    fn refuse(&self, fault: JournalFault) -> io::Result<()> {
        if self.fault == fault {
            Err(io::Error::other("scripted journal failure"))
        } else {
            Ok(())
        }
    }
}

impl TerminalJournal for LogJournal {
    fn backup(&mut self, _identity: &Identity, snapshot: &MenuFieldSnapshot) -> io::Result<()> {
        self.refuse(JournalFault::Backup)?;
        self.push(Event::Backup(snapshot.pages().len()))
    }

    fn planned(&mut self, _plan: &TerminalPlan) -> io::Result<()> {
        self.refuse(JournalFault::Planned)?;
        self.push(Event::Planned)
    }

    fn before_write(&mut self, phase: TerminalPhase, page: &PageReplacement) -> io::Result<()> {
        self.refuse(JournalFault::BeforeWrite)?;
        self.write_started = true;
        self.push(Event::Intent {
            phase,
            address: page.page().address().as_u32(),
        })
    }

    fn checkpoint(
        &mut self,
        _phase: TerminalPhase,
        acknowledged_exit: bool,
        journal: &McpJournal,
    ) -> io::Result<()> {
        self.exit_acknowledged = acknowledged_exit;
        self.refuse(JournalFault::Checkpoint)?;
        self.push(Event::Checkpoint {
            acknowledged: acknowledged_exit,
            verified: journal
                .verified
                .iter()
                .map(|page| page.address().as_u32())
                .collect(),
        })
    }
}

async fn run(
    mock: MockTransport,
    gateway: DvGatewayMode,
    request: TerminalRequest<'_>,
    fault: JournalFault,
    cancelled: &AtomicBool,
) -> Result<
    (
        Result<TerminalProgramReport, Box<TerminalProgramError>>,
        Vec<Event>,
        LogJournal,
    ),
    Box<dyn std::error::Error>,
> {
    let expected = identity()?;
    let log: Log = Arc::default();
    let mut journal = LogJournal::new(&log);
    journal.fault = fault;
    let mut radio = Radio::new(LoggingTransport {
        mock,
        log: Arc::clone(&log),
    });
    let result = program_terminal(
        &mut radio,
        &expected,
        gateway,
        request,
        &mut journal,
        cancelled,
    )
    .await;
    let transport = radio.into_transport();
    transport.mock.assert_complete();
    let events = log.lock().map_err(|_| "log poisoned")?.clone();
    Ok((result, events, journal))
}

/// Assert every recorded intent immediately precedes its page write, and count
/// the intents.
fn intents_precede_writes(events: &[Event]) -> Result<usize, Box<dyn std::error::Error>> {
    let mut intents = 0;
    let mut index = 0;
    while let Some(event) = events.get(index) {
        if let Event::Intent { address, .. } = event {
            intents += 1;
            let next = events
                .get(index + 1)
                .ok_or("intent without a following write")?;
            let Event::Write(frame) = next else {
                return Err("intent must be immediately followed by its write".into());
            };
            let payload = frame
                .len()
                .checked_sub(5)
                .ok_or("write frame shorter than its header")?;
            let request = write_request(Page::new(Address::new(*address)?, payload)?);
            assert_eq!(
                frame.get(..5),
                Some(request.as_slice()),
                "intent names the written page"
            );
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(intents)
}

#[tokio::test]
async fn entry_backs_up_plans_writes_and_exits_with_intents_before_writes() -> TestResult {
    let (mock, plan) = entry_wire(0, 1, 1)?;
    let changed = plan
        .replacements()
        .iter()
        .filter(|page| !page.is_noop())
        .count();
    let (result, events, journal) = run(
        mock,
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    let report = result.map_err(|error| error.to_string())?;
    assert_eq!(report.plan, plan);
    assert!(report.write_started);
    assert!(report.exit_acknowledged);
    assert!(journal.exit_acknowledged);
    assert_eq!(intents_precede_writes(&events)?, changed);
    assert_eq!(events.first(), Some(&Event::Backup(plan_backup_pages())));
    assert_eq!(events.get(1), Some(&Event::Planned));
    let verified: Vec<u32> = plan
        .replacements()
        .iter()
        .filter(|page| !page.is_noop())
        .map(|page| page.page().address().as_u32())
        .collect();
    assert_eq!(
        events.last(),
        Some(&Event::Checkpoint {
            acknowledged: true,
            verified,
        })
    );
    Ok(())
}

fn plan_backup_pages() -> usize {
    regions::menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .count()
}

#[tokio::test]
async fn cancellation_before_entry_writes_nothing_and_records_no_checkpoint() -> TestResult {
    let (result, events, _journal) = run(
        MockTransport::new(),
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(true),
    )
    .await?;
    let error = result.err().ok_or("cancellation must fail")?;
    assert!(matches!(
        error.operation,
        Some(TerminalOperationError::Cancelled)
    ));
    assert!(error.exit.is_none() && error.checkpoint.is_none());
    assert!(events.is_empty(), "no MCP entry, so no records");
    Ok(())
}

/// Sets the cancellation flag as soon as the first intent is recorded, so the
/// bounded transaction must still finish and exit.
struct CancelJournal {
    inner: LogJournal,
    cancelled: Arc<AtomicBool>,
}

impl TerminalJournal for CancelJournal {
    fn backup(&mut self, identity: &Identity, snapshot: &MenuFieldSnapshot) -> io::Result<()> {
        self.inner.backup(identity, snapshot)
    }
    fn planned(&mut self, plan: &TerminalPlan) -> io::Result<()> {
        self.inner.planned(plan)
    }
    fn before_write(&mut self, phase: TerminalPhase, page: &PageReplacement) -> io::Result<()> {
        let result = self.inner.before_write(phase, page);
        self.cancelled.store(true, Ordering::Relaxed);
        result
    }
    fn checkpoint(
        &mut self,
        phase: TerminalPhase,
        acknowledged: bool,
        journal: &McpJournal,
    ) -> io::Result<()> {
        self.inner.checkpoint(phase, acknowledged, journal)
    }
}

#[tokio::test]
async fn cancellation_after_the_first_write_completes_the_transaction_and_exit() -> TestResult {
    let expected = identity()?;
    let (mock, plan) = entry_wire(0, 1, 1)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let log: Log = Arc::default();
    let mut journal = LogJournal::new(&log);
    journal.log = Arc::clone(&log);
    let mut wrapper = CancelJournal {
        inner: journal,
        cancelled: Arc::clone(&cancelled),
    };
    let mut radio = Radio::new(LoggingTransport {
        mock,
        log: Arc::clone(&log),
    });
    let report = program_terminal(
        &mut radio,
        &expected,
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        &mut wrapper,
        &cancelled,
    )
    .await
    .map_err(|error| error.to_string())?;
    radio.into_transport().mock.assert_complete();
    assert_eq!(report.plan, plan);
    assert!(
        report.exit_acknowledged,
        "the bounded transaction still exits"
    );
    assert!(wrapper.inner.exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_retains_intent_and_forbids_exit() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let changed = plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("changed page missing")?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    mock.expect_eof(&frame(changed.page(), changed.replacement()));
    let (result, events, journal) = run(
        mock,
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    let error = result.err().ok_or("a missing ACK must fail")?;
    assert!(error.operation.is_some());
    assert!(error.exit.is_some(), "an uncertain session sends no exit");
    assert!(error.checkpoint.is_none());
    assert!(journal.write_started);
    assert!(!journal.exit_acknowledged);
    // The intent for the changed page was recorded before its write frame.
    assert_eq!(intents_precede_writes(&events)?, 1);
    assert!(matches!(
        events.last(),
        Some(Event::Checkpoint {
            acknowledged: false,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn fresh_guard_conflict_after_backup_refuses_all_writes_but_still_exits() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    for (index, page) in plan.replacements().iter().enumerate() {
        let mut bytes = page.expected().to_vec();
        if index == 3 {
            *bytes.last_mut().ok_or("target guard missing")? ^= 1;
        }
        read(&mut mock, page.page(), &bytes);
    }
    mock.expect(b"E", &[ACK]);
    let (result, events, journal) = run(
        mock,
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    let error = result.err().ok_or("a guard conflict must fail")?;
    assert!(error.operation.is_some());
    assert!(error.exit.is_none(), "a clean compare boundary still exits");
    assert!(!journal.write_started);
    assert_eq!(intents_precede_writes(&events)?, 0);
    assert!(matches!(
        events.last(),
        Some(Event::Checkpoint {
            acknowledged: true,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn already_satisfied_terminal_backs_up_and_compares_without_writes() -> TestResult {
    let snapshot = configuration(2, 2, 0)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(2);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    let (result, _events, journal) = run(
        mock,
        DvGatewayMode::Terminal,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    assert!(result.is_ok());
    assert!(!journal.write_started);
    assert!(journal.exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn changed_gateway_refuses_programming_entry() -> TestResult {
    let (result, events, _journal) = run(
        cat(2),
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    let error = result.err().ok_or("a changed Gateway must fail")?;
    assert!(matches!(
        error.operation,
        Some(TerminalOperationError::Gateway { .. })
    ));
    assert!(events.is_empty(), "no MCP entry, so no records");
    Ok(())
}

#[tokio::test]
async fn changed_firmware_refuses_before_entry() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.03\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    let (result, events, _journal) = run(
        mock,
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    assert!(matches!(
        result.err().and_then(|error| error.operation),
        Some(TerminalOperationError::Identity { .. })
    ));
    assert!(events.is_empty());
    Ok(())
}

#[tokio::test]
async fn restoration_compares_the_exact_inverse_without_a_second_backup() -> TestResult {
    let forward = TerminalPlan::for_route(
        &identity()?,
        &configuration(0, 1, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?;
    let restore = forward.restoration()?;
    let mut mock = cat(2);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    compare(&mut mock, &restore);
    changes(&mut mock, &restore);
    mock.expect(b"E", &[ACK]);
    let (result, events, journal) = run(
        mock,
        DvGatewayMode::Terminal,
        TerminalRequest::Restore { plan: &restore },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    let report = result.map_err(|error| error.to_string())?;
    assert_eq!(report.plan, restore);
    assert!(journal.exit_acknowledged);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Backup(_) | Event::Planned)),
        "restoration neither backs up nor plans"
    );
    let restore_intents = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::Intent {
                    phase: TerminalPhase::Restore,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        restore_intents,
        restore
            .replacements()
            .iter()
            .filter(|page| !page.is_noop())
            .count()
    );
    Ok(())
}

#[tokio::test]
async fn inconsistent_restoration_admission_fails_before_identity() -> TestResult {
    let restore = TerminalPlan::for_route(
        &identity()?,
        &configuration(0, 1, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?
    .restoration()?;
    // Gateway Off does not match the restoration plan's before-Terminal.
    let (result, events, _journal) = run(
        MockTransport::new(),
        DvGatewayMode::Off,
        TerminalRequest::Restore { plan: &restore },
        JournalFault::None,
        &AtomicBool::new(false),
    )
    .await?;
    assert!(matches!(
        result.err().and_then(|error| error.operation),
        Some(TerminalOperationError::RestorationMismatch)
    ));
    assert!(events.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_failed_backup_record_exits_without_any_write() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    mock.expect(b"E", &[ACK]);
    let (result, events, journal) = run(
        mock,
        DvGatewayMode::Off,
        TerminalRequest::Enter {
            route: TerminalGatewayRoute::Bluetooth,
        },
        JournalFault::Backup,
        &AtomicBool::new(false),
    )
    .await?;
    let error = result.err().ok_or("a failed backup record must fail")?;
    assert!(matches!(
        error.operation,
        Some(TerminalOperationError::Journal(_))
    ));
    assert!(error.exit.is_none(), "a clean compare boundary still exits");
    assert!(!journal.write_started);
    assert!(!events.iter().any(|event| matches!(event, Event::Backup(_))));
    Ok(())
}
