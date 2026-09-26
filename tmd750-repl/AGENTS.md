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
  and serializes the run. This crate no longer owns the entry, transition or
  readiness state machines. `mcp::reconnect` remains the readiness engine for
  the other MCP commands (backup, menu apply, the trials and `dstar probe
  --manage-terminal`), which are unchanged.
- Every capture directory is created exclusively, mode 0700, with 0600 files;
  an existing path is refused rather than overwritten. `--output` names a new
  directory whose parent must exist.
- Gates: `mcp menu apply` and `mcp text set` require `--port` and `--apply`;
  `dstar probe` and the one-shot trials listed under Commands require `--port`
  and `--approve-live-test`; `dstar start` is the one radio-writing workflow
  without a flag, since invoking it is the request to write the Terminal and
  routing pages and open the modem link. Do not add a workflow that reaches the
  radio outside these gates.
- Native Bluetooth admits only `mcp probe` and `mcp backup`; general MCP
  settings writes and the fixed trials are USB-only and are refused before the
  helper launches.
- Every new native connection gets its `Radio` from `native::cat::wrap` (or,
  in the shared fixed read, `Some(BLUETOOTH_FIRST_REPLY_TIMEOUT)`), so the
  first reply may take ten seconds and every later exchange 1.5. The fresh
  post-exit connection's first reply instead gets
  `bluetooth_first_reply_timeout_after_exit` of the time since the exiting read
  returned, never a deadline counted from the reopen. Never raise
  `EXCHANGE_TIMEOUT` instead, and never give USB the first-reply deadline: the
  silent-`ID` readiness retry depends on the 1.5 s timeout.
- `mcp text set` admits either USB role for every form, and the role selects
  the post-exit check: one open on the main-unit endpoint
  (`reconnect::verify_required`, or `verify_required_gateway_off` for
  `dstar-my-callsign-1`), and on the operation-panel endpoint the bounded
  silent-`ID` retry backups use (`reconnect::verify_readiness`, or
  `verify_readiness_gateway_off`), because that endpoint returns within
  seconds but answers `ID` only once its tuple is ready. A single-attempt check
  on that endpoint ends the update at `possibly_changed` even after an
  acknowledged write.
- Every MCP entry on the operation-panel endpoint (`mcp probe`, `mcp backup`,
  `mcp menu apply`, both `mcp text set` sessions) goes through
  `reconnect::settle_before_entry`, which waits until the endpoint has been
  enumerated for `SETTLE_QUIET` without a gap, because the panel endpoint
  re-enumerates once more after first answering `ID` and an entry sent at that
  moment fails with `ENXIO`; the main-unit endpoint opens at once. A text
  update's second session settles on either role.
- `mcp menu apply` writes one positional `FIELD VALUE` pair plus any number of
  `--and FIELD VALUE` pairs in one session; `--slot` binds every per-slot field
  of the run and is refused when every field is global, and a repeated field
  is rejected by the plan.
- `mcp menu apply` admits either USB role: the main-unit endpoint verifies
  post-exit with one open (`reconnect::verify_required_gateway_off`), the
  operation-panel endpoint with `reconnect::verify_readiness_gateway_off`, the
  bounded silent-`ID` retry followed by one `GW` query per matched attempt.
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
  the memory-format byte every write path requires.
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
cargo run -p tmd750-repl -- mcp text list|show|preview       # set needs --port and --apply
cargo run -p tmd750-repl -- mcp terminal preflight|compare
cargo run -p tmd750-repl -- --port PATH dstar probe --approve-live-test [--output NEW_DIRECTORY]
cargo run -p tmd750-repl -- dstar start CALL [REFLECTOR]     # --control-port pins the independent USB CAT connector
```

- `mcp menu list|describe|show|preview`, `mcp text list|show|preview` and
  `mcp terminal preflight|compare` are offline: they read a completed backup
  report and never enumerate or open an endpoint.
- The guarded one-shot trials (`mcp pm1-trial`, `mcp my1-trial`,
  `mcp reentry-probe`, `mcp terminal-exit-trial`) each require an explicit
  `--port` before `mcp` plus `--approve-live-test`.
- `dstar probe --manage-terminal` additionally requires `--control-port` and
  `--backup`, and the control endpoint must be the other USB role of the same
  radio.
- Interactive vocabulary: `help`, `identity` (`id`), `status`, `mode [a|b]
  [fm|dv|am|nfm]`, `dv [a|b]`, `fm [a|b]` (`normal`), `gateway`, `terminal`,
  `quit` (`exit`), plus the typed reads and verified writes in `src/cat.rs`
  (`freq`, `power`, `tuning`, `squelch`, `step`, `up`/`down`, `bands`, GPS,
  VOX, Bluetooth and the rest listed by `help`). `dstar start` consumes the
  connection and is startup-only. Every write goes through
  `CommandPolicy::admit_mode_write`, so native Bluetooth writes require a fresh
  Gateway Off reply.
- In a `dstar start` session, Ctrl-C returns to the D-STAR prompt and
  `dstar stop` closes the link and rewrites exactly the pages startup changed.
