# tmd750-repl

## Rules

- No direct or transitive `kenwood-thd75` dependency.
  `tests/dependency_seam.rs` rejects another model's source imports and
  manifest ownership.
- D-STAR configuration, events and modem processing come from `mmdvm::dstar`;
  strict version identification comes from `mmdvm::probe`. The exact selected
  connection stays local policy here.
- The `dstar start` Bluetooth Terminal lifecycle (preflight, entry, MMDVM
  handoff and restoration) is owned by `kenwood_tmd750::TerminalLifecycle`.
  `dstar/hosts.rs` implements the library `ControlHost`, `ModemHost` and
  `TerminalJournal` over this crate's capture transcripts, native Bluetooth
  backend and recovery journal; `dstar/startup` reserves the capture directory
  and serializes the run. This crate owns no entry, transition or readiness
  state machine.
- Post-exit readiness for every other MCP command runs the library
  `radio::readiness::verify_readiness` through `mcp::reconnect::host`
  (`RecordingHost` writes each enumeration, wait and open to the one
  verification transcript and synchronizes it in `close`; `SettleHost` does
  the same for `await_continuous_enumeration`). `mcp::reconnect::readiness`
  only serializes the library report into the unchanged `ReadinessVerification`
  JSON; a failure the transcript caused is the `capture` stage. Never add a
  loop, retry or budget check to the REPL side.
- Every capture directory is created exclusively, mode 0700, with 0600 files;
  an existing path is refused rather than overwritten. `--output` names a new
  directory whose parent must exist.
- Gates: every `mcp ... apply` and `mcp ... set` workflow reaches the radio
  only with `--apply` and an explicit endpoint: `--port`, or for `mcp menu
  apply` and `mcp text set` alone, `--bluetooth`/`--bluetooth-address` with a
  native backup of that radio. `dstar probe` and `mcp reentry-probe` require
  `--port` and `--approve-live-test`; `dstar start` is the one radio-writing
  workflow without a flag, since invoking it is the request to write the
  Terminal and routing pages and open the modem link. Do not add a workflow
  that reaches the radio outside these gates.
- Native Bluetooth admits `mcp probe`, `mcp backup` without
  `--position-memory`, `mcp text set` and `mcp menu apply`; the reentry probe
  is USB-only and is refused before the helper launches. A native write
  validates its options, native backup and value before selecting a radio,
  and refuses one whose address differs from the backup's.
- Write workflows reach their endpoint only through `mcp::link::WriteLink`
  (`UsbLink` or `NativeLink`): open, reply deadlines, pre-entry check, hold
  after the exit, post-exit check. Never branch on the transport inside a
  workflow.
- Every `mcp ... apply` and `mcp ... set` workflow except `mcp text set` is a
  page-plan write sharing `mcp::page_apply`: journal, raw transcript
  synchronization before each write intent, one session, fresh CAT check,
  report. A new page write adds a `PlanKind` with its report vocabulary and a
  `PagePlan`, never a parallel workflow; the menu report and journal shapes
  stay unchanged.
- Every new native connection gets its `Radio` from `Radio::new_bluetooth`
  (or, in the shared fixed read, `Some(BLUETOOTH_FIRST_REPLY_TIMEOUT)`), so the
  first reply may take ten seconds and every later exchange 1.5. The fresh
  post-exit connection's first reply instead gets
  `bluetooth_first_reply_timeout_after_exit` of the time since the exiting read
  returned, never a deadline counted from the reopen. Never raise the regular
  reply deadline instead, and never give USB the first-reply deadline: the
  silent-`ID` readiness retry depends on the 1.5 s timeout.
- `mcp text set`, `mcp menu apply` and `mcp reentry-probe` admit either USB
  role and verify post-exit with the bounded silent-`ID` retry backups use
  (`reconnect::verify_readiness`, or `verify_readiness_gateway_off` with one
  `GW` query on the matched connection), because the operation-panel endpoint
  returns within seconds but answers `ID` only once its tuple is ready; a
  single-attempt check there ends an update at `possibly_changed` even after
  an acknowledged write.
- Every MCP entry on the operation-panel endpoint (`mcp probe`, `mcp backup`,
  `mcp menu apply`, both `mcp text set` sessions) goes through
  `reconnect::settle_before_entry`, which waits until the endpoint has been
  enumerated for `SETTLE_QUIET` without a gap, because the panel endpoint
  re-enumerates once more after first answering `ID` and an entry sent at that
  moment fails with `ENXIO`; the main-unit endpoint opens at once. A text
  update's second session settles on either role
  (`reconnect::await_settled_endpoint`).
- `mcp menu apply` writes one positional `FIELD VALUE` pair plus any number of
  `--and FIELD VALUE` pairs in one session; `--slot` binds every per-slot field
  of the run and is refused when every field is global, and a repeated field
  is rejected by the plan.
- New library MCP writes are qualified on hardware through this crate's
  journaled `mcp text set` workflow, never through an ad-hoc driver, so every
  run leaves a journal, captures and a report. A new text field adds a
  `SetTarget` value, an `UpdateKind` and a `PreparedUpdate` variant;
  `target::Update` is implemented once for every `TextFieldUpdate`, never per
  field.
- The CLI tests must not open a radio endpoint.
- Backup fixtures for `mcp text set` tests come from
  `snapshot::tests::fixture()`: patch a page's `data` in place (records carry
  `length` and must keep standard order; removing or appending a segment fails
  the parser or the order check) and zero byte 2 of the address-8 fragment,
  the memory-format byte every write path requires. Every standard page is
  filled with `0x42`: patch any byte a decoder reads canonically (a bool menu
  field such as `radio.AutoOffset`, zero padding) or the load fails.
- Hardware-derived policy is imported from the library, never copied:
  `radio::readiness` constants (`SETTLE`, `READINESS_BUDGET`,
  `RETRY_INTERVAL`, `MAXIMUM_OPEN_ATTEMPTS`, `EXCHANGE_TIMEOUT`,
  `SETTLE_QUIET`, ...) and `transport::reenumeration` (`select_pinned`,
  `is_unambiguous`, `deduplicate_aliases`).
- Report JSON strings are a contract apart from library names: a library
  rename maps onto the existing string (`durable_intent`, `unqualified`);
  changing a string bumps that report's `format_version`, and loaders keep
  reading older backup formats. Build report strings with a local exhaustive
  `match` (`mode_label`, `gateway_label`), never a library `Display`.
- Committed prose states contracts only: no dated bench narrative, no approval
  vocabulary, no capture paths. `README.md` is the crate's published front page.

## Commands

```bash
cargo run -p tmd750-repl                                    # interactive; USB first, then the unique recognized paired Bluetooth radio
cargo run -p tmd750-repl -- --bluetooth                     # force Bluetooth; --bluetooth-address ADDRESS pins one device
cargo run -p tmd750-repl -- --port PATH status              # one-shot command; --baud defaults to 9600
cargo run -p tmd750-repl -- --port PATH mcp backup|probe --output NEW_DIRECTORY
cargo run -p tmd750-repl -- mcp menu list|describe|show|preview
cargo run -p tmd750-repl -- --port PATH mcp menu apply --backup REPORT --apply FIELD VALUE
cargo run -p tmd750-repl -- --bluetooth mcp menu apply --backup NATIVE_REPORT --apply FIELD VALUE
cargo run -p tmd750-repl -- mcp text list|show|preview       # set needs --apply and --port or --bluetooth
cargo run -p tmd750-repl -- mcp terminal preflight|compare
cargo run -p tmd750-repl -- --port PATH dstar probe --approve-live-test [--output NEW_DIRECTORY]
cargo run -p tmd750-repl -- dstar start CALL [REFLECTOR]     # --control-port pins the independent USB CAT connector
```

- Every `mcp` `list`, `describe`, `show`, `names` and `preview` subcommand,
  and `mcp terminal preflight|compare`, is offline: it reads the registry or
  completed backup reports and never enumerates or opens an endpoint.
- `mcp reentry-probe` requires an explicit `--port` before `mcp` plus
  `--approve-live-test`.
- `dstar probe --manage-terminal` additionally requires `--control-port` and
  `--backup`, and the control endpoint must be the other USB role of the same
  radio.
- Interactive vocabulary: `help`, `identity` (`id`), `status`, `mode [a|b]
  [fm|dv|am|nfm]`, `dv [a|b]`, `fm [a|b]` (`normal`), `gateway`, `terminal`,
  `quit` (`exit`), plus the typed reads, verified writes and transmit actions
  in `src/cat.rs` (`freq`, `power`, `tuning`, `squelch`, `step`, `up`/`down`,
  `bands`, GPS, TNC, VOX, Bluetooth, `tx`, `rx`, `send-beacon`, `power-off`
  and the rest listed by `help`). `dstar start` consumes the connection and is
  startup-only. Every write and every action except `rx` goes through
  `CommandPolicy::admit_mode_write`, so native Bluetooth requires a fresh
  Gateway Off reply first; `rx` is never gated, so a keyed transmitter can
  always be released.
- `tx` holds only through `Radio::transmit_until` with a deadline raced against
  Ctrl-C (`TransmitHold`, 1 ms to `MAX_TRANSMIT_HOLD`); never add an unbounded
  key-up. The library sends `RX` itself after an unconfirmed `TX`, so "send
  rx" is the next action exactly on `Error::ReleaseUnconfirmed`. `power-off`
  ends the session.
- In a `dstar start` session, Ctrl-C returns to the D-STAR prompt and
  `dstar stop` closes the link and rewrites exactly the pages startup changed.
