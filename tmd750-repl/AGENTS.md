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
- The CLI tests must not open a radio endpoint.
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
