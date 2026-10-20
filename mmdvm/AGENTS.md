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
- `DstarModem::initialize` sends the D-STAR-only `SetConfig`, waits for its ACK, then sends `SetMode(Dstar)`, and returns the original modem handle on failure. The modem-side echo policy and its cap (`MAX_ECHO_RECORDING_FRAMES`, 3,000 frames) live in `dstar`; the record-and-replay state machine is `dstar_gateway_core::echo`.
- `DstarModem::next_event` returning `Ok(None)` is a quiet poll interval, not EOF. Waiting for an event is cancellation-safe; TX submission, initialization and echo playback are not cancellation-atomic.
- Only D-STAR transmission is driven here; the other formats have core codecs and no shell TX workflow.
