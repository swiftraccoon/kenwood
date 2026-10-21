# thd75-repl

Line-oriented CAT control and APRS KISS commands. Read the root `AGENTS.md`
and `RUST_STANDARDS.md` before changing this crate.

## Commands

```bash
cargo run -p thd75-repl -- --port 00-11-22-33-44-55 --port-interface bluetooth
cargo nextest run -p thd75-repl --features testing --all-targets
cargo test -p thd75-repl --features testing --doc
```

- Name `--features testing` explicitly for mock integration tests. Never use `--all-features` here; preserve the explicitly selected hardware-free configuration.
- Every Cargo invocation uses the pinned workspace toolchain PATH.
- The library includes its README as rustdoc; examples must be pure or explicitly `no_run` when they require a connection.

## Connection and mode boundaries

- Initial discovery may select an endpoint. Reconnect uses the retained `Radio`; never repeat discovery during a mode transition.
- CAT startup observes AE serial/model identity before dispatch. Preserve that checked identity through mode ownership and require it again before recovered CAT use or cached streaming restoration.
- Native macOS Bluetooth accepts an exact paired address, never a display name or a `cu.*` path as physical identity.
- `--port-interface {usb|bluetooth}` requires a concrete `--port`; reject conflicting enumerator metadata before opening.
- Kenwood D-STAR configuration is unsupported. Validate startup arguments and return the exact `UnsupportedProfile` result before discovery, Terminal activation or radio writes. Never substitute generic MMDVM V1/V2 configuration bytes or restore the binary-only relay/echo facade without a verified Kenwood profile and a complete lifecycle implementation.
- MMDVM diagnosis proves binary framing. Keep CAT dispatch blocked on that connection. `--exit-terminal-mode` prompts for Menu 650 Off, closes on input failure/EOF, and reconnects only the retained connection before checking identity and firmware.
- `--set-gateway-off` operates on the CAT-capable interface. Prove the model and exact supported firmware before a schema-bound MCP operation. Complete a started bounded exchange after interruption; preserve operation, signal and close failures.
- `connect_with_tnc_exit` first tries exact CAT identity. Its recovery preamble selects TNC Off and Band A; it must not run while merely inspecting an already CAT-capable gateway-control interface.
- The two mode exits differ: APRS stop restores CAT; closing a binary-only MMDVM connection preserves the radio's persistent setting.

## Output and command contracts

- User messages use `aprintln!`; `last` uses `replay_output`. No ordinary stdout path may bypass escaping, physical line bounds, synchronized history and first-error retention.
- Runtime presentation and history have one private mutex. Expose deliberate operations, never writable globals or mutable storage handles.
- `render_message` bounds physical lines to 80 printable ASCII bytes, preserves LF as logical line boundaries and escapes other controls/non-ASCII. Tracing diagnostics are a separate opt-in stream.
- Verbose gating occurs at the call site. Errors and confirmation prompts remain visible when recurring monitor detail is disabled.
- Confirm before transmitting, including APRS startup's automatic acknowledgements/query replies and optional digipeating. A required noninteractive prompt must refuse without reading the next queued command. Preserve refusal, EOF and I/O outcomes.
- Omitted bands may default to A. Invalid supplied band, boolean and numeric values reject before any read or mutation.
- A CAT routing-destination update is a local radio result, never confirmation that a remote reflector linked or unlinked.
- `check` reports eight evaluated line rules on named samples and six rules needing separate tests. No external accessibility certification or complete formatter-input coverage is implied.
- Quit and EOF share explicit APRS exit and connection close. Retain simultaneous operation/cleanup errors; do not print Goodbye after a failed close.
- Log files use exclusive creation and bounded collision suffixes. Tracing is best effort, not a durable capture journal.
- The `lock` command stays informational: no verified CAT key-lock operation is available and `LC` controls an unrelated backlight setting.

## Protocol limits

- After `0M PROGRAM` acknowledges, only binary page read, page write, ACK and exit framing belong on the connection.
- If an MCP session cannot prove its exit boundary, send no further radio command; retain the model's recovery-required result.
- The raw `0G`/`0E`/`9R`/`9E`/`2V` APIs remain quarantined. Do not restore them without independently checked entry, length, direction and exit framing.
