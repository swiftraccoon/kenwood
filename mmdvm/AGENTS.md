# mmdvm (tokio shell)

Async MMDVM modem I/O and a radio-neutral D-STAR runtime over `mmdvm-core`,
and the clock source for the modem loop. Callers hand it an open connection;
it never selects an endpoint, sends CAT, changes radio routing or reopens a
connection, so entry, proof and restoration stay in each model crate.

## Commands

```bash
cargo nextest run -p mmdvm --no-default-features --all-targets
cargo nextest run -p mmdvm --no-default-features --features runtime --all-targets   # likewise probe, dstar
```

## Rules

- Every feature set builds and tests alone (none, `runtime`, `probe`, `dstar`) in `./lint.sh` and Ubuntu CI. The `use <dep> as _;` acknowledgements in `src/lib.rs` are gated per feature set, so a new dependency or feature needs one in every set that leaves it unused. Both integration tests require `runtime`.
- `pub use mmdvm_core as core` exposes this crate's own codec; model crates import `mmdvm` canonically and never re-export it.
- `probe` borrows `kenwood_transport::Transport` with that crate's serial feature off and starts no modem loop. Its status parse is strict: it rejects the unknown mode bytes the core decodes as `Idle`.
- The modem loop polls status every 250 ms (`MMDVMHost/Modem.cpp:245`) and releases D-STAR TX frames only when the modem reports FIFO space. A queued frame is not a transmitted one; `Event::TxDropped` reports frames discarded at shutdown or failure.
- Command identifiers use `mmdvm::core::Command`; raw frame payloads are validated before channel submission. Raw `SET_MODE` and `SET_CONFIG` are refused so their replies cannot resolve typed operations.
- A failed frame write or flush retires the loop and stream. `set_mode` has one total two-second deadline; an unresolved written mode change survives caller timeout or cancellation and blocks another mode change until its ACK/NAK arrives.
- Debug responses preserve command and binary payload. `DEBUG1` through `DEBUG5` append zero through four integers; they are not severity levels.
- `DstarModem::initialize` takes an explicit `DstarConfiguration`, confirms its version, awaits correlated `SetConfig`, then sends `SetMode(Dstar)`. V1/V2 layouts follow the pinned public upstream. The Kenwood Terminal profile is unsupported and fails during pure construction before mode entry; a generic MMDVM layout is not evidence of Kenwood firmware compatibility. Unresolved written configuration retains its reservation through timeout/cancellation.
- `AsyncModem::shutdown` returns `ModemShutdown` or `ShutdownFailure`, each retaining its `ShutdownSummary`. Inspect dropped-frame count and retained events before extracting the recovered stream. `None` for the count means an interrupted task, never confirmed zero loss.
- The modem-side echo policy and its cap (`MAX_ECHO_RECORDING_FRAMES`, 3,000 frames) live in `dstar`; the record-and-replay state machine is `dstar_gateway_core::echo`.
- `DstarModem::next_event` returning `Ok(None)` is a quiet poll interval, not EOF. Waiting for an event is cancellation-safe; TX submission, initialization and echo playback are not cancellation-atomic.
- Only D-STAR transmission is driven here; the other formats have core codecs and no shell TX workflow.
