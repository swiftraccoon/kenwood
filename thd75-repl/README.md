# thd75-repl

Line-oriented CAT control and APRS KISS commands for the Kenwood TH-D75,
with checked command parsing and plain-text output. The library supplies pure
argument, script, formatting and lint operations; the binary owns connections
and mode transitions. APIs are experimental and the package is unpublished.

## Connections and modes

```text
thd75-repl --port /dev/cu.usbmodem1234 --baud 115200
thd75-repl --port 00-11-22-33-44-55 --port-interface bluetooth
```

Initial discovery prefers USB, then Bluetooth. An explicit `--port-interface`
disambiguates an opaque serial path. Baud rates are nonzero; serial Bluetooth
uses the model library's preset. Native macOS Bluetooth requires an exact
paired address so reconnecting retains the selected physical radio.

CAT startup reads the model, firmware and AE serial identity before command
dispatch. Later CAT recovery compares the retained serial/model identity before
restoring cached streaming settings or accepting further commands. A mismatched
identity blocks CAT use and reports the original and observed identities.

The binary has CAT, APRS and binary-only MMDVM connection states. CAT commands
read or change supported settings; `help`, `help <command>` and `help all`
describe their accepted arguments. An omitted band selects A; an invalid
supplied band or setter value is rejected before a radio exchange. `lock`
reports that no verified CAT key-lock operation is available. Direct frequency
writes remain unavailable; `tune` delegates to the model's checked workflow.

`aprs start W1AW b 7` enters KISS on TNC data Band B with source W1AW-7.
APRS commands send checked messages, positions, objects, status and motion
updates. `aprs stop` exits KISS and re-proves CAT synchronization. Required
transmit confirmations refuse noninteractive input unless `--yes` is supplied.
APRS startup requires confirmation before entering KISS because it enables
automatic acknowledgements and query replies. Adding `digi` also enables
automatic relaying. Refusal keeps the CAT connection unchanged.
The `igate` command runs the APRS-IS bridge with a checked locality policy;
uploads wait for a matching server-verified login. It disconnects on Ctrl-C.

Kenwood D-STAR modem initialization is unavailable because its configuration
layout is unverified. `dstar start <callsign> [reflector]` validates its arguments
and returns the typed unsupported-profile failure before discovery, Terminal
activation or radio writes in batch startup. Interactive rejection leaves the
open connection unchanged. This binary provides no reflector relay or echo
runtime. The shared D-STAR and MMDVM libraries retain their independent APIs.

A positively identified MMDVM connection blocks CAT commands. With
`--exit-terminal-mode`, the program waits for an input line after the radio's
Menu 650 change, then reopens the retained connection and checks CAT identity
and firmware. EOF or an input failure closes the connection without reopening.
`--set-gateway-off` instead uses the CAT-capable interface, validates the exact
supported firmware schema and performs the model's verified Menu 650 update.
An interrupted MCP exchange completes its bounded protocol cleanup before the
program reports the signal and resulting radio state.

## Output, scripts and errors

Program messages pass through one stdout/history operation. LF separates
logical lines; other controls and non-ASCII characters use visible Rust-style
escapes. Physical lines contain at most 80 printable ASCII bytes. Wrapped
continuations begin `Continued: `. A selected timestamp prefixes every physical
line. Tracing diagnostics are separate, opt-in developer output.

`--timestamps` selects UTC. `--local-time` detects a fixed UTC offset where
available; `--utc-offset=+05:30` selects a checked explicit offset. `last`,
`last N` and `last all` replay original stored lines without adding timestamps
or recording duplicates. `--history-lines` sets capacity, default 30; zero
clears and disables retention. Output and resizing share one mutex. The first
stdout write or flush failure stops subsequent writes and is returned before
process success. Ordinary blocking terminal output has no application deadline.

```text
thd75-repl --script commands.txt
thd75-repl --script - --yes
thd75-repl check
```

Scripts preserve command order, skip blank lines and lines starting with `#`,
and stop at quit or end of input. Parse errors and ordinary CAT command errors
are printed while processing continues. Input, confirmation I/O, APRS I/O,
APRS-IS bridge, mode-restoration or connection-close failures terminate with a
failing exit status.
There is no strict-script flag. Batch D-STAR startup is rejected while loading
commands, before any connection opens. Both quit and end of input explicitly
exit APRS and close the connection; simultaneous operation and cleanup failures
retain their typed causes. Closing does not restore a persistent radio setting.

`check` evaluates eight line rules on its named samples and identifies six
other rules requiring command or source tests. Its zero exit status describes
those evaluated samples. These are local presentation rules, with no external
accessibility certification claim.

## Library examples

Argument parsing validates the complete station identity before I/O:

```rust
use thd75_repl::aprs_args::StartArgs;
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let request = StartArgs::parse(&["W1AW", "b", "7"])?;
assert_eq!(request.address().to_string(), "W1AW-7");
# Ok(()) }
```

Scripts expose read-only command order or transfer their owned commands:

```rust
use thd75_repl::script::Script;
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let script = Script::from_reader(&b"# setup\nhelp\nquit\n"[..])?;
assert_eq!(script.commands(), ["help", "quit"]);
# Ok(()) }
```

Pure rendering and linting can be used without a connection:

```rust
use thd75_repl::{lint, render_message};
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let lines = render_message("Station: W1AW\u{1b}", None);
assert_eq!(lines, [r"Station: W1AW\u{1b}"]);
lint::check_output(&lines.join("\n"))?;
# Ok(()) }
```

`aprintln!` applies the same rendering, timestamps, history and error retention:

```rust
thd75_repl::aprintln!("Status: {}", "ready");
```

History can also retain rendered messages without writing to stdout:

```rust
use thd75_repl::{last_lines, record_output, set_history_capacity};
set_history_capacity(2);
record_output("Status: ready");
assert_eq!(last_lines(1), ["Status: ready"]);
```

## Logging and testing

Logging is disabled by default. `--log-level=debug` or `--trace` reserves a new
file with exclusive creation under the platform log directory. UTC start time
names the file; same-second collisions receive numbered suffixes, up to 1024
attempts. Existing logs are never truncated and files do not rotate. The
asynchronous tracing sink is best effort and can drop records under sustained
load; it is not a durable capture journal. `RUST_LOG` independently enables
stderr tracing.

Use the pinned workspace toolchain and `--features testing` for hardware-free
mock integration tests. The feature can be selected in any build profile.
Full-tree validation follows
[CONTRIBUTING](https://github.com/swiftraccoon/kenwood/blob/main/CONTRIBUTING.md).
GPL-2.0-or-later licensing is preserved.
