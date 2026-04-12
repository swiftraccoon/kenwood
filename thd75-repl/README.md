# thd75-repl

Accessible command-line REPL for the Kenwood TH-D75 transceiver. Designed for screen-reader compatibility and rigorously conformant to nine published accessibility standards.

## Features

- CAT radio control (frequency, mode, squelch, power, VOX, etc.)
- D-STAR reflector gateway (DPlus/DExtra/DCS, REF/XRF/XLX/DCS reflectors)
- APRS KISS mode (packet radio)
- Auto-detect Reflector Terminal Mode on startup
- Auto-download Pi-Star host files
- Plain text output, one line at a time
- No box drawing, escape sequences, or cursor repositioning
- Per-command help via `help <command>` and `help all`
- Full radio state dump via `status`
- Re-announce previous output via `last`, `last N`, or `last all`
- Transmit confirmation on every TX command (disable with `--yes`)
- Verbose / quiet toggle for monitor modes (`verbose on|off`, `quiet`)
- Accessibility compliance self-check via `thd75-repl check`
- Script mode via `--script <file>` for batch operation
- Local-time timestamps via `--local-time` and `--utc-offset=+HH:MM`

## Accessibility compliance

This REPL conforms to nine published accessibility standards:

- WCAG 2.1 Level AA
- Section 508 of the US Rehabilitation Act
- CHI 2021 CLI accessibility paper (Pradhan et al.)
- EN 301 549 v3.2.1
- ISO 9241-171
- ITU-T Recommendation F.790
- BRLTTY compatibility
- Handi-Ham Program recommendations
- ARRL accessibility resources

Conformance is enforced by 14 mechanically-checked rules (R1-R14) covering line length, ASCII purity, error prefixes, unit spelling, boolean rendering, label format, and more. Every user-facing string is unit-tested against the rule set.

Run `thd75-repl check` to verify your build meets the spec. The subcommand exercises every formatter, runs the accessibility lint, and prints a rule-by-rule report. Exit 0 means every string passes every rule.

## Usage

```
thd75-repl [--port /dev/cu.usbmodem1234] [--baud 115200]
```

### Session-wide flags

- `--timestamps` / `-t` — prepend `[HH:MM:SS]` to every output line (UTC by default)
- `--local-time` — use local time in timestamps (detected from `date +%z` on Unix)
- `--utc-offset=+HH:MM` — override the timezone offset manually
- `--history-lines=N` — set the `last` command history buffer size (default 30)
- `--yes` — skip transmit confirmation prompts for this session (use with caution)

### Interactive commands

At the `d75>` prompt, the most useful commands are:

- `status` — dump the full radio state in one block
- `last` / `last 5` / `last all` — re-announce previous output
- `help` / `help <cmd>` / `help all` — help, per-command or all commands
- `verbose on` / `quiet` — toggle monitor verbosity
- `confirm on` / `confirm off` — toggle transmit confirmation

Standard commands cover frequency, mode, power, squelch, attenuator, VOX, dual band, Bluetooth, lock, FM radio, memory channels, GPS, URCALL, and reflector linking.

## Script mode

Run a sequence of commands from a file and exit:

```
thd75-repl --script ~/contest-setup.txt
```

Lines starting with `#` are comments. Blank lines are skipped. `exit` / `quit` ends the script.

Example `contest-setup.txt`:

```
# Contest startup
power a high
mode a fm
tune a 146.520
squelch a 3
status
```

Transmit commands in script mode require `--yes` on the command line to run unattended.

## D-STAR Gateway

```
d75> dstar start KQ4NIT
dstar> link REF030C
dstar> monitor
dstar> unlink
dstar> dstar stop
```

## Logging

By default no log file is created and no tracing output is written —
the terminal only shows normal REPL output. File logging is opt-in
because trace-level capture during D-STAR voice flow generates large
files fast (~1 MB/s per active reflector link).

To enable a log file, pass `--log-level` or `--trace`:

```
thd75-repl --trace               # trace level — captures every packet
thd75-repl --log-level=debug     # state transitions + decoded events
thd75-repl --log-level=info      # high-level session flow only
```

File location (rotated daily):

- macOS: `~/Library/Logs/thd75-repl/thd75-repl.log.<date>`
- Linux: `~/.local/state/thd75-repl/thd75-repl.log.<date>`
- Windows: `%LOCALAPPDATA%\thd75-repl\logs\thd75-repl.log.<date>`

For live stderr output (power users, no file), set `RUST_LOG`:

```
RUST_LOG=dstar_gateway=debug thd75-repl
RUST_LOG=dstar_gateway=trace,kenwood_thd75::slow_data=debug thd75-repl
```

`RUST_LOG` and `--log-level` are independent — you can combine them
(live stderr stream + persistent file) or use either alone.

## Requirements

- Rust 1.94+
- Kenwood TH-D75 connected via USB or Bluetooth
