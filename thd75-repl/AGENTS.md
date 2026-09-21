# thd75-repl

Line-based REPL for screen-reader accessibility: no cursor tricks, no full-screen UI, one self-contained spoken line per message.

## Commands

```bash
cargo run -p thd75-repl -- --port 00-11-22-33-44-55 --port-interface bluetooth
cargo run -p thd75-repl -- --script run.txt --yes            # batch; `-` reads stdin
cargo nextest run -p thd75-repl --features testing --all-targets   # what lint.sh runs
scripts/aprs-validation/run.sh <phase1..phase5>              # FIFO-paced on-air phases
cargo run -p aprs-is --example monitor -- <CALL>             # receive-only network witness
```

- `--features testing` MUST be named explicitly: it gates `--mock-radio` and the `[[test]] required-features` integration suites, so a bare test run skips them silently and they pass by omission. Never use `--all-features` here: that enables `dstar-gateway/hardware-tests`, whose test opens live reflectors.
- The APRS phase runner needs `APRS_CALL`, `APRS_LAT`, and `APRS_LON`; TX phases run the REPL with `--yes`, which skips every transmit confirmation.
- `--port-interface {usb|bluetooth}` requires `--port` and disambiguates Windows `COM` ports and custom symlinks. `--baud` defaults to 115200 for USB; Bluetooth SPP is forced to 9600 with RTS/CTS by the library preset.
- Native Bluetooth accepts ONLY a syntactically exact paired address. A display name or a Bluetooth `cu.*` node is rejected, because the OS cannot preserve physical identity through a display name.
- The running binary is also the RFCOMM helper: the opener passes `std::env::current_exe()`, so a stale or renamed binary changes which helper child is spawned.

## Ownership boundaries

- The echo-test state machine (UR `"       E"` detection, arming, capped recording, reply-delay window, playback-header convention) lives in `dstar_gateway_core::echo`; the REPL drives it and keeps narration, 15 ms pacing, Ctrl-C, and the stale-event drain.
- The gated relay loop stays REPL-side deliberately: it is single-consumer and entangled with accessibility narration. Do not absorb it into the library without a second radio-bearing consumer.
- The REPL does not decode voice. It relays raw AMBE to the radio's DVSI modem and records frames verbatim for `AMBE_CAPTURE`; the only in-process decode is slow-data text.
- Runtime configuration and history are read-only through `gateway.modem()`; there is no replaceable mutable modem reference. Startup stores a validated modem config before radio IO, with one callsign for modem and network identity.
- `lock` is informational only: no verified CAT key-lock operation exists, and it must never be wired to the unrelated `LC` backlight mnemonic.
- Fatal errors print through `Display`; never return multi-line guidance from main's `Err` path, because the Debug escapes are announced by screen readers.
- `tests/static_rules.rs` walks `src/` enforcing the accessibility rule set; add a rule there when a new structural pattern lands.

## Code patterns and debugging

- `aprintln!` (`src/lib.rs`) prefixes the optional `[HH:MM:SS]` timestamp and records every line in the `last` history buffer. Verbose gating happens at the call site through `is_verbose()`, never inside the macro; session-wide flags are `AtomicBool` plus macro, not state threaded through every call.
- The relay diagnostics `relay → radio: header` (per `VoiceStart`) and `relay → radio: EOT` (per `VoiceEnd`) log at TRACE under target `thd75_repl::reflector`, and `D-STAR modem: buffer N, transmit active|idle` lines fire on `tx()` edges. They reach the session log only with `--trace` and never print to the REPL.

## Terminal mode

- Terminal mode engages SLOWLY after the MCP-write reboot: CAT alive at +10 s, dead about +49 s, MMDVM answering after that. Never probe once; `ensure_terminal_mode` polls up to 90 s. The `GET_VERSION` reply is `e0 12 00 01` + `"TH-D75 RTM1.00"`.
- Menu 985 binds terminal mode to USB or Bluetooth. Never enable Menu 650 without explicitly binding Menu 985 to the physical link the caller owns: the OTHER port is what keeps CAT alive.
- `--set-gateway-off` clears Menu 650 by MCP write, but only over the port Menu 985 does NOT bind; the gateway port rejects the `0M PROGRAM` handshake, with a 5 s timeout as the symptom.
- Terminal mode keeps the same USB PID and device node, so there is no re-enumeration to detect it by.
- The REPL never re-execs itself. `--exit-terminal-mode` keeps the gateway link read-only (it cannot prove model and firmware for a safe memory write), prompts for the Menu 650 change, reconnects, and re-proves identity before returning to CAT.

## Operational cautions

- `connect_with_tnc_exit` first proves a quiet, exact `ID TH-D75` response and preserves the current TNC data band. Only a failed or ambiguous proof runs the recovery preamble, whose `TN 0,0` selects TNC Off and Band A; re-set mode and data band afterwards if the operator wants radio-side packet use.
- Never `timeout`-kill a live `monitor`: it strands the radio in KISS mode. The preamble auto-recovers with a KISS Return frame, but the session dies.
- After `0M PROGRAM` acknowledges, only binary page read, page write, ACK, and exit framing belong on the connection. It is not an entry command for the ASCII adjustment dispatcher.
- If an MCP session does not prove its exit, issue no further radio command until the radio has been fully power-cycled.
- The raw `0G`/`0E`/`9R`/`9E`/`2V` APIs came from shifted handler associations and invalid framing. They stay quarantined from the shared codec and must not be restored without independent entry, length, direction, and exit proofs. Factory calibration data is a separate high-risk surface from typed CAT settings and the binary MCP image.
