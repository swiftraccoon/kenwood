//! Session behavior over fake input and a mock transport: prompt, batch and
//! one-shot command sources, the Gateway guard before each mode write, and the
//! close performed on every termination.

use std::collections::VecDeque;
use std::sync::Mutex;

use kenwood_transport::{MockTransport, TransportError};

use super::*;

type TestResult = AppResult<()>;
type Log = Arc<Mutex<Vec<Vec<u8>>>>;

struct Input {
    events: VecDeque<AppResult<Event>>,
    close_error: bool,
    mode: InputMode,
}

impl Input {
    fn lines(lines: &[&str]) -> Self {
        Self {
            events: lines
                .iter()
                .map(|line| Ok(Event::Line((*line).to_owned())))
                .collect(),
            close_error: false,
            mode: InputMode::Interactive,
        }
    }

    fn batch(lines: &[&str]) -> Self {
        Self {
            mode: InputMode::Batch,
            ..Self::lines(lines)
        }
    }
}

impl CommandInput for Input {
    fn mode(&self) -> InputMode {
        self.mode
    }

    async fn next(&mut self, _cancelled: &AtomicBool) -> AppResult<Event> {
        self.events.pop_front().unwrap_or(Ok(Event::Eof))
    }

    fn close(&mut self) -> AppResult<()> {
        if self.close_error {
            Err(io::Error::other("scripted input close failure").into())
        } else {
            Ok(())
        }
    }
}

struct Connection {
    script: MockTransport,
    log: Log,
    cancelled: Arc<AtomicBool>,
    cancel_after_reply: Option<(Vec<u8>, usize)>,
    close_error: bool,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.log
            .lock()
            .map_err(|_| TransportError::Write(io::Error::other("test log poisoned")))?
            .push(bytes.to_vec());
        self.script.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.script.read(bytes).await?;
        if let Some((reply, matches_to_skip)) = &mut self.cancel_after_reply
            && Some(reply.as_slice()) == bytes.get(..count)
        {
            if *matches_to_skip == 0 {
                self.cancelled.store(true, Ordering::Relaxed);
            } else {
                *matches_to_skip -= 1;
            }
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.log
            .lock()
            .map_err(|_| TransportError::Read(io::Error::other("test log poisoned")))?
            .push(b"close".to_vec());
        self.script.assert_complete();
        if self.close_error {
            Err(TransportError::Read(io::Error::other(
                "scripted radio close failure",
            )))
        } else {
            Ok(())
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.push(b"drop".to_vec());
        }
    }
}

struct Harness {
    directory: tempfile::TempDir,
    log: Log,
    cancelled: Arc<AtomicBool>,
}

impl Harness {
    fn new() -> AppResult<Self> {
        Ok(Self {
            directory: tempfile::tempdir()?,
            log: Arc::default(),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    fn captured(
        &self,
        script: MockTransport,
        close_error: bool,
        cancel_after_reply: Option<(&[u8], usize)>,
    ) -> AppResult<CaptureTransport<Connection, File>> {
        let recorder = Recorder::named(
            crate::capture::create_private_file(&self.directory.path().join("transcript.jsonl"))?,
            Arc::clone(&self.cancelled),
            "transcript.jsonl",
        );
        Ok(CaptureTransport::required(
            Connection {
                script,
                log: Arc::clone(&self.log),
                cancelled: Arc::clone(&self.cancelled),
                cancel_after_reply: cancel_after_reply.map(|(reply, skip)| (reply.to_vec(), skip)),
                close_error,
            },
            recorder,
        ))
    }

    fn assert_retired_once(&self) -> TestResult {
        let log = self.log.lock().map_err(|_| "test log poisoned")?;
        assert_eq!(
            log.iter()
                .filter(|event| event.as_slice() == b"close")
                .count(),
            1
        );
        assert_eq!(
            log.iter()
                .filter(|event| event.as_slice() == b"drop")
                .count(),
            1
        );
        assert!(
            log.ends_with(&[b"close".to_vec(), b"drop".to_vec()]),
            "{log:?}"
        );
        drop(log);
        Ok(())
    }
}

fn identity(firmware: &[u8]) -> MockTransport {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", firmware);
    script.expect(b"TY\r", b"TY K,2,1\r");
    script
}

fn admitted() -> MockTransport {
    let mut script = identity(b"FV 1.02\r");
    script.expect(b"GW\r", b"GW 0\r");
    script
}

#[test]
fn prompt_interrupt_is_orderly_but_never_completes_a_startup_command() {
    assert!(Mode::Interactive.completed(Termination::Interrupted, true));
    assert!(Mode::Interactive.completed(Termination::Quit, false));
    assert!(Mode::Interactive.completed(Termination::Eof, false));
    assert!(Mode::Startup.completed(Termination::StartupComplete, false));
    assert!(!Mode::Startup.completed(Termination::StartupComplete, true));
    assert!(!Mode::Startup.completed(Termination::Interrupted, false));
    assert!(!Mode::Startup.completed(Termination::Interrupted, true));
    assert!(!Mode::Interactive.completed(Termination::Failed, false));
}

#[tokio::test]
async fn prompt_reuses_parser_and_keeps_startup_only_commands_off_the_wire() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"MD 1\r", b"MD 1,0\r");
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::Prompt(Input::lines(&[
        "",
        "raw MD 0,1",
        "mcp backup",
        "dstar probe",
        "dstar start KQ4NIT",
        "help",
        "mode b",
        "quit",
    ]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(evidence.succeeded(), "{evidence:?}");
    assert_eq!(evidence.termination, Termination::Quit);
    assert_eq!(evidence.commands_completed, 3);
    assert!(commands.close().is_none());
    harness.assert_retired_once()
}

#[tokio::test]
async fn invalid_batch_command_fails_before_later_input() -> TestResult {
    for rejected in ["bad-command", "dstar start KQ4NIT", "mcp backup"] {
        let harness = Harness::new()?;
        let transport = harness.captured(admitted(), false, None)?;
        let mut commands = Commands::Prompt(Input::batch(&[rejected, "help"]));
        assert!(matches!(commands.mode(), Mode::Batch));
        let evidence = session(transport, &mut commands, &harness.cancelled).await;
        assert!(evidence.input_error.is_some(), "{rejected}: {evidence:?}");
        assert_eq!(evidence.commands_completed, 0);
        assert_eq!(evidence.termination, Termination::Failed);
        assert!(!evidence.succeeded());
        let Commands::Prompt(input) = &commands else {
            return Err("batch lost its input owner".into());
        };
        assert_eq!(input.events.len(), 1, "later input must remain unread");
        assert!(commands.close().is_none());
        harness.assert_retired_once()?;
    }
    Ok(())
}

#[tokio::test]
async fn valid_batch_finishes_at_eof() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"MD 0\r", b"MD 0,0\r");
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::Prompt(Input::batch(&["", "mode"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(matches!(commands.mode(), Mode::Batch));
    assert!(evidence.succeeded());
    assert_eq!(evidence.commands_completed, 1);
    assert_eq!(evidence.termination, Termination::Eof);
    assert!(commands.mode().completed(evidence.termination, false));
    assert!(commands.close().is_none());
    harness.assert_retired_once()
}

#[tokio::test]
async fn cancelled_batch_cannot_report_completion() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"MD 0\r", b"MD 0,0\r");
    let transport = harness.captured(script, false, Some((b"MD 0,0\r", 0)))?;
    let mut commands = Commands::Prompt(Input::batch(&["mode", "help"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert_eq!(evidence.commands_completed, 1);
    assert_eq!(evidence.termination, Termination::Interrupted);
    assert!(evidence.operation_error.is_none());
    assert!(evidence.close_error.is_none());
    assert!(evidence.transcript.complete);
    assert!(harness.cancelled.load(Ordering::Relaxed));
    assert!(
        !commands.mode().completed(evidence.termination, true),
        "interrupted redirected input must not report successful completion"
    );
    assert!(commands.close().is_none());
    harness.assert_retired_once()
}

#[tokio::test]
async fn buffered_batch_yields_to_cancellation_before_dispatch() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let mut commands = Commands::Prompt(Input::batch(&["", "", "help"]));
    let cancel = async {
        cancelled.store(true, Ordering::Relaxed);
    };
    let (next, ()) = tokio::join!(biased; commands.next(&cancelled), cancel);
    assert!(matches!(next?, Next::End(Termination::Interrupted)));
    Ok(())
}

#[tokio::test]
async fn one_shot_reads_once_without_command_input() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"MD 0\r", b"MD 0,0\r");
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::<Input>::Startup(Some(Command::Mode {
        band: kenwood_tmd750::Band::A,
        set_to: None,
    }));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(evidence.succeeded(), "{evidence:?}");
    assert_eq!(evidence.termination, Termination::StartupComplete);
    assert_eq!(evidence.commands_completed, 1);
    harness.assert_retired_once()
}

#[tokio::test]
async fn every_mode_alias_gets_a_fresh_gateway_guard_and_verified_write() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    for (write, read, reply) in [
        (
            b"MD 0,1\r".as_slice(),
            b"MD 0\r".as_slice(),
            b"MD 0,1\r".as_slice(),
        ),
        (b"MD 1,1\r", b"MD 1\r", b"MD 1,1\r"),
        (b"MD 1,0\r", b"MD 1\r", b"MD 1,0\r"),
        (b"MD 0,0\r", b"MD 0\r", b"MD 0,0\r"),
    ] {
        script.expect(b"GW\r", b"GW 0\r");
        script.expect(write, reply);
        script.expect(read, reply);
    }
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::Prompt(Input::lines(&["mode a dv", "dv b", "fm b", "normal a"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(evidence.succeeded(), "{evidence:?}");
    assert_eq!(evidence.termination, Termination::Eof);
    assert_eq!(evidence.commands_completed, 4);
    harness.assert_retired_once()
}

#[tokio::test]
async fn gateway_change_blocks_mode_write_and_retires_the_prompt() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"GW\r", b"GW 2\r");
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::Prompt(Input::lines(&["dv", "status"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(!evidence.succeeded());
    assert!(
        evidence
            .operation_error
            .as_ref()
            .is_some_and(|error| error.message.contains("Gateway Off"))
    );
    assert_eq!(evidence.commands_completed, 0);
    harness.assert_retired_once()
}

#[tokio::test]
async fn non_off_startup_state_refuses_even_read_only_prompt_commands() -> TestResult {
    for gateway in [b"GW 2\r".as_slice(), b"GW 9\r".as_slice()] {
        let harness = Harness::new()?;
        let mut script = identity(b"FV 1.02\r");
        script.expect(b"GW\r", gateway);
        let transport = harness.captured(script, false, None)?;
        let mut commands = Commands::Prompt(Input::lines(&["mode"]));
        let evidence = session(transport, &mut commands, &harness.cancelled).await;
        assert!(!evidence.succeeded());
        assert!(evidence.operation_error.is_some());
        assert_eq!(evidence.commands_completed, 0);
        harness.assert_retired_once()?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_after_gateway_guard_blocks_the_mode_write() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"GW\r", b"GW 0\r");
    let transport = harness.captured(script, false, Some((b"GW 0\r", 1)))?;
    let mut commands = Commands::Prompt(Input::lines(&["dv"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(harness.cancelled.load(Ordering::Relaxed));
    assert!(
        evidence
            .operation_error
            .as_ref()
            .is_some_and(|error| error.message.contains("cancelled"))
    );
    assert_eq!(evidence.commands_completed, 0);
    harness.assert_retired_once()
}

#[tokio::test]
async fn started_write_finishes_readback_before_cancellation_closes_the_owner() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"GW\r", b"GW 0\r");
    script.expect(b"MD 0,1\r", b"MD 0,1\r");
    script.expect(b"MD 0\r", b"MD 0,1\r");
    let transport = harness.captured(script, false, Some((b"MD 0,1\r", 0)))?;
    let mut commands = Commands::Prompt(Input::lines(&["dv", "mode"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert_eq!(evidence.termination, Termination::Interrupted);
    assert!(evidence.operation_error.is_none(), "{evidence:?}");
    assert_eq!(evidence.commands_completed, 1);
    harness.assert_retired_once()
}

#[tokio::test]
async fn failed_cat_and_close_are_both_retained_without_command_retry() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"MD 0\r", b"?\r");
    let transport = harness.captured(script, true, None)?;
    let mut commands = Commands::Prompt(Input::lines(&["mode", "mode"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(evidence.operation_error.is_some());
    assert!(evidence.close_error.is_some());
    assert!(evidence.transcript.complete);
    assert_eq!(evidence.termination, Termination::Failed);
    harness.assert_retired_once()
}

#[tokio::test]
async fn output_failure_stops_at_a_boundary_and_preserves_owner_retirement() -> TestResult {
    #[derive(Clone, Copy)]
    enum Boundary {
        BeforeTraffic,
        AfterIdentity,
        AfterCommand,
    }

    for boundary in [
        Boundary::BeforeTraffic,
        Boundary::AfterIdentity,
        Boundary::AfterCommand,
    ] {
        let harness = Harness::new()?;
        let script = match boundary {
            Boundary::BeforeTraffic => MockTransport::new(),
            Boundary::AfterIdentity => identity(b"FV 1.02\r"),
            Boundary::AfterCommand => {
                let mut script = admitted();
                script.expect(b"MD 0\r", b"MD 0,0\r");
                script
            }
        };
        let transport = harness.captured(script, true, None)?;
        let mut commands = Commands::Prompt(Input::batch(&["mode", "mode"]));
        let check_output = || -> AppResult<()> {
            let log = harness.log.lock().map_err(|_| "test log poisoned")?;
            let failed = match boundary {
                Boundary::BeforeTraffic => true,
                Boundary::AfterIdentity => log.iter().any(|bytes| bytes == b"TY\r"),
                Boundary::AfterCommand => log.iter().any(|bytes| bytes == b"MD 0\r"),
            };
            drop(log);
            if failed {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "output fixture closed").into())
            } else {
                Ok(())
            }
        };
        let evidence =
            session_with_output(transport, &mut commands, &harness.cancelled, &check_output).await;
        assert!(
            evidence
                .operation_error
                .as_ref()
                .is_some_and(|error| error.message.contains("output fixture closed"))
        );
        assert!(evidence.close_error.is_some());
        assert!(evidence.transcript.complete);
        assert_eq!(evidence.commands_completed, 0);
        assert!(!evidence.succeeded());
        let Commands::Prompt(input) = &commands else {
            return Err("output failure lost its input owner".into());
        };
        let unread = if matches!(boundary, Boundary::AfterCommand) {
            1
        } else {
            2
        };
        assert_eq!(input.events.len(), unread);
        assert!(commands.close().is_none());
        harness.assert_retired_once()?;
    }
    Ok(())
}

#[tokio::test]
async fn input_and_input_close_failures_do_not_hide_radio_close_failure() -> TestResult {
    let harness = Harness::new()?;
    let transport = harness.captured(admitted(), true, None)?;
    let mut commands = Commands::Prompt(Input {
        events: [Err(io::Error::other("scripted input read failure").into())].into(),
        close_error: true,
        mode: InputMode::Interactive,
    });
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(evidence.input_error.is_some());
    assert!(evidence.operation_error.is_none());
    assert!(evidence.close_error.is_some());
    assert!(commands.close().is_some());
    assert!(!evidence.succeeded());
    harness.assert_retired_once()
}

#[tokio::test]
async fn input_interrupt_is_recorded_and_retired_without_new_cat() -> TestResult {
    let harness = Harness::new()?;
    let transport = harness.captured(admitted(), false, None)?;
    let mut commands = Commands::Prompt(Input {
        events: [Ok(Event::Interrupted)].into(),
        close_error: false,
        mode: InputMode::Interactive,
    });
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert_eq!(evidence.termination, Termination::Interrupted);
    assert_eq!(evidence.commands_completed, 0);
    assert!(evidence.close_error.is_none());
    harness.assert_retired_once()
}

#[tokio::test]
async fn unsupported_firmware_reads_but_cannot_change_modes() -> TestResult {
    let harness = Harness::new()?;
    let mut script = identity(b"FV 1.03\r");
    script.expect(b"GW\r", b"GW 0\r");
    script.expect(b"MD 0\r", b"MD 0,0\r");
    script.expect(b"GW\r", b"GW 0\r");
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::Prompt(Input::lines(&["mode", "dv"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert_eq!(evidence.commands_completed, 1);
    assert!(
        evidence
            .operation_error
            .as_ref()
            .is_some_and(|error| error.message.contains("CAT mode writes support only"))
    );
    harness.assert_retired_once()
}

#[tokio::test]
async fn cancellation_between_status_reads_preserves_the_completed_operation() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", b"FV 1.02\r");
    script.expect(b"TY\r", b"TY K,2,1\r");
    script.expect(b"MD 0\r", b"MD 0,0\r");
    let transport = harness.captured(script, false, Some((b"MD 0,0\r", 0)))?;
    let mut commands = Commands::Prompt(Input::lines(&["status", "mode"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(
        evidence
            .operation_error
            .as_ref()
            .is_some_and(|error| error.message.contains("cancelled"))
    );
    assert_eq!(evidence.commands_completed, 0);
    assert!(evidence.transcript.complete);
    harness.assert_retired_once()
}

#[tokio::test]
async fn failed_required_capture_blocks_cat_but_still_retires_the_owner() -> TestResult {
    let harness = Harness::new()?;
    let path = harness.directory.path().join("read-only-transcript.jsonl");
    let _reserved = File::create_new(&path)?;
    let recorder = Recorder::named(
        File::open(&path)?,
        Arc::clone(&harness.cancelled),
        "transcript.jsonl",
    );
    let transport = CaptureTransport::required(
        Connection {
            script: MockTransport::new(),
            log: Arc::clone(&harness.log),
            cancelled: Arc::clone(&harness.cancelled),
            cancel_after_reply: None,
            close_error: false,
        },
        recorder,
    );
    let mut commands = Commands::Prompt(Input::lines(&["status"]));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    assert!(!evidence.succeeded());
    assert!(evidence.identity.is_none());
    assert!(!evidence.transcript.complete);
    assert!(evidence.capture_error.is_some());
    assert_eq!(evidence.commands_completed, 0);
    harness.assert_retired_once()
}

#[tokio::test]
async fn report_requires_completed_startup_and_independent_cleanup() -> TestResult {
    let harness = Harness::new()?;
    let mut script = admitted();
    script.expect(b"MD 0\r", b"MD 0,0\r");
    let transport = harness.captured(script, false, None)?;
    let mut commands = Commands::<Input>::Startup(Some(Command::Mode {
        band: kenwood_tmd750::Band::A,
        set_to: None,
    }));
    let evidence = session(transport, &mut commands, &harness.cancelled).await;
    let endpoint = Resolved {
        address: "01:23:45:67:89:AB".to_owned(),
        rfcomm_channel: 27,
    };
    let mut report = Report {
        format_version: 1,
        operation: "cat_session",
        mode: Mode::Startup,
        transport: "native_bluetooth",
        requested_address: "01:23:45:67:89:AB",
        helper_executable: None,
        started_at_utc: "2026-09-13T00:00:00Z".to_owned(),
        finished_at_utc: "2026-09-13T00:00:01Z".to_owned(),
        outcome: Outcome {
            opening: History {
                attempts: vec![opening::Attempt {
                    number: 1,
                    started: true,
                    resolved: Some(endpoint.clone()),
                    error: None,
                    interruption: None,
                }],
                retry_error: None,
                capture_error: None,
                transcript: evidence.transcript.clone(),
            },
            endpoint: Some(endpoint),
            session: Some(evidence),
        },
        input_close_error: None,
        output_error: None,
        signal_error: None,
        cancelled: false,
    };
    assert!(report.succeeded());
    report.cancelled = true;
    assert!(
        !report.succeeded(),
        "cancelled startup cannot report success"
    );
    report
        .outcome
        .session
        .as_mut()
        .ok_or("missing session")?
        .termination = Termination::Interrupted;
    assert!(
        !report.succeeded(),
        "interruption is not startup completion"
    );
    report.mode = Mode::Interactive;
    assert!(
        report.succeeded(),
        "an orderly interactive interrupt is allowed"
    );
    report.mode = Mode::Batch;
    assert!(!report.succeeded(), "an interrupted batch is incomplete");
    report.mode = Mode::Interactive;
    report.input_close_error = Some(Failure::from_error(&io::Error::other("input close")));
    assert!(!report.succeeded());
    report.input_close_error = None;
    report.signal_error = Some(Failure::from_error(&io::Error::other(
        "signal registration",
    )));
    assert!(!report.succeeded());
    report.signal_error = None;
    report.output_error = Some(Failure::from_error(&io::Error::new(
        io::ErrorKind::BrokenPipe,
        "output closed",
    )));
    assert!(!report.succeeded());
    let serialized = serde_json::to_value(&report)?;
    assert_eq!(
        serialized
            .pointer("/output_error/message")
            .and_then(serde_json::Value::as_str),
        Some("output closed")
    );
    harness.assert_retired_once()
}
