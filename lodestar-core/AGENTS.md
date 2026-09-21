# lodestar-core

This crate does **not** depend on `kenwood-thd75`: the CAT/MCP code here is a
port, not a dependency. Keep it that way.

## Commands

`./lodestar-core/scripts/build-xcframework.sh` runs from anywhere; it derives the
workspace root from its own location.

- Needs the Apple targets installed: `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios aarch64-apple-darwin x86_64-apple-darwin`.
- Outputs `lodestar/LodestarKit.xcframework` with three slices (`ios-arm64`, `ios-arm64_x86_64-simulator`, `macos-arm64_x86_64`; **no Mac Catalyst slice**) plus a regenerated `lodestar/Generated/LodestarCore.swift`. Inside each slice the UniFFI-emitted `*.modulemap` is copied to `Headers/module.modulemap` so Xcode auto-discovers the `LodestarCoreFFI` C module with no `MODULEMAP_FILE` build setting. A content-hashed cache short-circuits an unchanged rebuild.
- `src/bin/uniffi-bindgen.rs` is a crate-local binary so the generator can never drift from the `uniffi` dependency version; the xcframework script runs it.

## FFI contract (this crate owns it; Swift consumes)

- `CatCommand`: `Identify` (`ID\r`) and `FirmwareVersion` (`FV\r`). `CatResponse`: `Identify`, `FirmwareVersion`, `Unknown` (radio replied `?`), `NotAvailableInMode` (`N`), `Raw`.
- `connect_reflector(...)` is async and returns `Arc<ReflectorSession>`. `ReflectorObserver` is a Swift-implemented callback interface invoked from a background tokio task, so implementations must not block.
- `ReflectorEvent` carries `SlowDataUpdate` (sticky; fires whenever assembled text or DPRS position changes mid-stream) and `Unknown { detail }` alongside the voice and lifecycle variants. Adding a variant is a Swift-side breaking change; `Unknown` exists so an unrecognised event is never conflated with teardown.
- `header_bytes` is the raw 41-byte on-wire D-STAR header (MMDVM command `0x10`); `voice_bytes` is a raw 12-byte frame, 9 bytes AMBE plus 3 bytes slow data (MMDVM command `0x11`).
- One background task per session owns the `AsyncSession<P>` and pumps `next_event()`. `disconnect()` signals it over a `oneshot` and awaits the task; the TX path reaches the same task through a bounded `mpsc` with per-command `oneshot` replies.
- DPlus connect runs `AuthClient::authenticate()` over TCP before the UDP handshake. When both fail the returned error says so and points at XRF/DCS, which need no auth. Consumers surface that message instead of inventing one.
- Stream-ID validation lives here: every `send_*` path rejects zero. Generation is caller-side via `dstar_gateway::tokio_shell::fresh_stream_id`.

## Known duplication sites (propagate fixes)

- `drive_handshake_to_connected` delegates to `dstar_gateway::tokio_shell::drive_connecting`, as do the sextant, stargazer and thd75-repl equivalents. Protocol fixes go there.
- `mcp.rs` owns Lodestar's typed MCP-D75 schema: model and firmware identities, Menu 985 interface routing at `0x1093`, Menu 650 gateway mode at `0x1CA0`. These duplicate `kenwood-thd75`, whose registry pins remain the watchdog. Swift consumes the UniFFI schema and must not duplicate identities, offsets or stored values.
- `reflector.rs` owns host-list machinery; sextant's `hosts.rs` is a deliberately separate implementation, with the rationale in `sextant/AGENTS.md`.

## UniFFI gotchas (single owner for the workspace)

- Mixed UDL plus `#[uniffi::export]` crates require `uniffi-bindgen generate --library <path/to/lib.a>`. UDL-only mode silently omits proc-macro items, and the generated Swift simply will not contain them.
- `uniffi::export(async_runtime = "tokio")` needs the `"tokio"` feature on the `uniffi` dependency. Async methods on `#[derive(uniffi::Object)]` types take `self: Arc<Self>`.
- UniFFI 0.31 pulls `getrandom` 0.4 alongside the 0.2 and 0.3 versions other dependencies use; `deny.toml` skips the older two by exact version, so those entries go stale when either bumps.

## Lint opt-outs versus the workspace

All three are forced by UniFFI scaffolding: `unsafe_code = "forbid"` (it emits
`#[unsafe(no_mangle)]`), `clippy::missing_const_for_fn`, and
`unused_qualifications` (it emits `std::vec::Vec<u8>`). UDL-exposed
`fn(Vec<u8>)` entry points additionally carry a file-local
`#[expect(clippy::needless_pass_by_value, reason = "UniFFI FFI boundary requires owned Vec<u8>")]`.
