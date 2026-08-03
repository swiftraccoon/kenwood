# Code Review — thd75-tui and thd75-repl

## Critical Safety Findings

### TX Confirmation

**`thd75-tui/src/app.rs:1994-2010` — TUI transmits a position beacon of `(0.0, 0.0)` with no operator confirmation.** Pressing `b` on the APRS panel sends `BeaconPosition { lat: 0.0, lon: 0.0, comment: "" }` unconditionally. Failure: an operator hits `b` while browsing the panel and immediately keys the radio with a bogus null-island position, no dialog, no `--yes` gate. The REPL builds an entire `confirm::tx_confirm` gate for this exact scenario — the TUI ignores it.

**`thd75-tui/src/app.rs:1411-1414` — TUI sends an APRS message with no transmit confirmation.** `M` opens the compose buffer; pressing Enter sends immediately. Same defect class as beacon: no `tx_confirm` in the TUI at all. Ham-radio law compliance is preserved only because AX.25 carries the source callsign; the operator has no chance to cancel a mistyped addressee.

**`thd75-tui/src/app.rs:2534-2545` — TUI can flip `BeaconType` to `Auto` / `SmartBeaconing` / `PTT` via `+`/`-` with no gate.** These modes make the radio's own firmware transmit unattended; the REPL's `commands.rs:197-205` guards this with `tx_confirm`, the TUI does not. Failure: user cycles through the setting list, accidentally sets `Auto`, and the radio starts beaconing at its interval.

**`thd75-repl/src/commands.rs:822-836, 840-844, 786-807` — `reflector`, `unreflector`, and `urcall` skip `tx_confirm` even though `cq` in the same file (line 810) has it.** All four write URCALL to a value that causes the *next* PTT to send a link/unlink/callsign packet. The `cq` command being gated but the others not is either a bug or an unintended inconsistency — the operator can't reason about which URCALL writes are "safe".

### Ham-Radio Compliance

**`thd75-repl/src/main.rs:1656-1667` — `beacon` sends a hard-coded status "REPL beacon".** The AX.25 source callsign satisfies FCC 97.119, so law compliance holds, but a fixed vendor string leaking on-air is bad practice and undocumented; the help says only "Send a status beacon".

**`thd75-repl/scripts/aprs-validation/tx-persist.sh:41-46, 65-79` — persistent beacon loop is correctly bounded.** Success exit is `-f "$marker"` set by either witness match or digi-echo grep. Watchdog kills any cycle exceeding 150 s. `max_cycles` (default 36) bounds the loop even with no exit. `witness_loop` polls with 10 s sleep between reconnects. No infinite-TX path found.

## thd75-repl

### Concurrency / State

**`thd75-repl/src/main.rs:2544-2604` — `ensure_host_files` uses blocking `stdin.read_line` and does not check `SCRIPT_MODE`.** When called from `enter_dstar` under piped stdin, this consumes the next scripted command as the "y/n" answer. Same issue at `main.rs:3013-3022` (`exit_dstar`) and `main.rs:2079-2082` (`guide_exit_terminal_mode`). Silent script corruption.

**`thd75-repl/src/main.rs:1022-1026` — `--script-strict` accepted, silently ignored** (`let _ = script_strict;`). Documented on the flag as "reserved for future"; safe but user-visible dead code.

**`thd75-repl/src/main.rs:2872-2894` (`try_reconnect_reflector`) — auto-reconnect on `KeepaliveInactivity` is unbounded** — no per-session retry cap, no backoff. If the reflector is genuinely down, this reconnect fires every 30 s indefinitely. Not a leak, but the operator gets no "giving up" signal.

### Unwrap / Panic Justification

All ten `unwrap*`/`unreachable!` sites in `main.rs` are justified.

**`thd75-repl/src/mock_scenarios.rs` contains 0 `#[expect]`/`#[allow]` attributes.** The reviewer's "27" evidently referred to the 30 `mock.expect(bytes, response)` calls — those are `MockTransport::expect` method invocations programming the mock queue, not lint attributes. All are correct exchange-programming pairs verified against the corresponding wire bytes.

### TX Test Gating

**`thd75-repl/Cargo.toml:126-128` — the transmit-driving integration test is properly feature-gated:** `[[test]] name = "script_integration" required-features = ["testing"]`, and the test file itself carries `#![cfg(feature = "testing")]`. `--mock-radio` is gated by the same feature (`main.rs:200-202`), so no on-air TX can occur in default CI or release builds.

### Accessibility

Output is well-structured for screen readers. `output.rs::freq_mhz` (line 35-45) correctly renders `146.5 megahertz` etc.; every user-facing function has lint coverage. The `check` subcommand exercises every formatter through 14 rules.

**`thd75-repl/src/output.rs:663` — special-case fix for zero-frame `VoiceEnd`** avoids a misleading "0 frames in 0.00 seconds" line. Good.

**Comment mismatch at `thd75-repl/src/help_text.rs:448-453` — `CONFIRM_HELP` claims `beacon, position, msg, cq, echo, and link` prompt.** The list is out of date: `object`, `mice`, `compressed`, `status`, and `motion` also prompt (main.rs:1728, 1759, 1782, 1802). Sighted operator gets stale info.

### Command Parsing

**`thd75-repl/src/commands.rs:832` — `reflector` module char defaults to `'A'` on empty string.** `let module = module_arg.chars().next().unwrap_or('A');` — a caller passing an empty second arg silently gets module A. Rejecting would be safer; the surrounding `if args.len() < 2` guard means the second slot exists but could be `""`.

**`thd75-repl/src/commands.rs:411-427` — `channels` command's `end.saturating_add(20)` and `end <= start` guards are correct**; verified `channels 65530` doesn't overflow. `channels 0 0` fails cleanly with "end must be greater than start".

**`thd75-repl/src/main.rs:460-487` (`parse_utc_offset`)** properly rejects non-ASCII up-front (line 455) and has a regression test at line 4474; the byte-slicing that once panicked is guarded.

## thd75-tui

### Concurrency / Radio Task

**`thd75-tui/src/radio_task.rs:568-575` — BT reconnect via cross-thread channel uses `recv_timeout(10s)`.** If the main thread's CFRunLoop pump lags, the reconnect times out with `"BT requires main thread"` even when reconnect would succeed. Non-fatal — the exponential backoff (line 609) retries — but confusing error message.

**`thd75-tui/src/radio_task.rs:187-244` — `aprs_pending`/`dstar_pending` mode transitions correctly `take()` the radio out of `Option<Radio<_>>` before calling `enter_*_session` that consumes it by value.** On failure `radio_opt` is `None` and control falls into the reconnect loop, which drops the (already-consumed) transport reference and rediscovers. No dangling-radio bug.

**`thd75-tui/src/radio_task.rs:277-305` — AI-push handler for `Response::Busy`.** `SM read failed on BY` warns and marks busy anyway (line 287-294). Correct semantics: signal presence is authoritative from BY; meter value is best-effort.

**`thd75-tui/src/radio_task.rs:1039-1095` (`discover_and_open`) — auto-discovery opens the first USB port found.** With multiple TH-D75s the wrong radio may be opened; user must pass `--port`. Documented behavior, not a bug.

### TUI Panic Surface

**`thd75-tui/src/ui/mod.rs:30-32, 39-41, 51-53` — layout destructures with `let [a, b, c] = *chunks else { return; }`.** Correctly bails without panicking if a resize produces a degenerate layout.

**`thd75-tui/src/ui/aprs.rs:80-92, 114-119` — station-list scroll math uses `saturating_sub` for visible height and `min(station_count)` for end.** When window height < 2, `visible_height = 0`, `get(0..0).unwrap_or(&[])` yields empty slice, loop body never runs — safe.

**`thd75-tui/src/ui/channels.rs:44-45` — `channel_list_index.min(items.len().saturating_sub(1))` for `ListState`** safely clamps when a filter empties the list.

**`thd75-tui/src/event.rs:215` — `unreachable!("command receiver already taken")` in `take_command_receiver`.** Guarded by construction: only `main.rs` calls it, exactly once.

### TUI Text-Wrap Note

**`thd75-tui/src/ui/aprs.rs:98-118` — station rows format columns with fixed widths (`{:<10}`, `{:<26}`)** and use `Paragraph::new(lines)` (not `.wrap(Wrap { trim: false })`), so long callsigns or comments truncate at the pane edge rather than wrapping. No panic, but on narrow terminals data is silently clipped without a "…" indicator.

### TUI Panic-Hook

**`thd75-tui/src/main.rs:88-93` — installs a panic hook that restores the terminal before delegating to the original hook.** Correct; a mid-render panic won't leave the terminal in raw mode.

## Cross-cutting

**`thd75-repl/src/main.rs:1385-1421` and `main.rs:1428-1481` — mode transitions correctly handle borrow-vs-move.** `Radio<EitherTransport>` is consumed on `enter_aprs`/`enter_dstar` and returned on stop. State machine boxes each variant to keep enum size uniform (`clippy::large_enum_variant`).

**`thd75-repl/src/main.rs:3735` — `session.tx_seq = (session.tx_seq + 1) % 21`** — using `+ 1` on `u8` at seq range 0-20 cannot overflow; safe.

**`thd75-repl/src/main.rs:3730-3735` — slow-data cycling** uses `session.tx_slow_data_idx += 1;` unbounded. Over a very long TX (~2^32 frames = ~2.7 years continuous) this overflows. Not realistic; `saturating_add` would be tidier.

**No `TODO`/`FIXME`/`HACK` markers found** in any of the reviewed files. All `#[expect(...)]` attributes carry substantive `reason = ...` prose and are justified.

## Summary — Highest-Impact Bugs

1. `thd75-tui/src/app.rs:1994-2010` — beacon transmits at 0,0 with no confirmation.
2. `thd75-tui/src/app.rs:1411-1414` — message TX with no confirmation.
3. `thd75-tui/src/app.rs:2534-2545` — beacon-type switch to Auto/PTT/Smart with no confirmation triggers unattended radio TX.
4. `thd75-repl/src/commands.rs:822, 840, 786` — `reflector`/`unreflector`/`urcall` skip the `tx_confirm` gate that the sibling `cq` uses.
5. `thd75-repl/src/main.rs:2557, 3022, 2081` — `stdin.read_line` under piped script mode silently consumes the next scripted command.
6. `thd75-repl/src/help_text.rs:448-453` — `CONFIRM_HELP` lists an outdated subset of commands that prompt.
7. `thd75-repl/src/commands.rs:832` — `reflector` silently defaults module to `'A'` on empty arg.
