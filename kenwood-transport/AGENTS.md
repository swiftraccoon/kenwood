# kenwood-transport

Model-neutral async byte I/O for both radio libraries, their REPLs and TUI,
`mmdvm` and `azimuth-core`. It owns connections, never radio policy: no model
names, VID/PIDs, line presets, readiness checks, retries or mode commands.
Those belong to each model crate's transport wrapper.

## Commands

```bash
cargo nextest run -p kenwood-transport                                                     # default `serial` feature
cargo nextest run -p kenwood-transport --no-default-features --features native-bluetooth   # macOS helper tests, no radio
```

## Rules

- `Transport::read` stays cancellation-safe: `StreamAdapter` races a pending read against outbound writes and drops the losing read future, so a backend that loses bytes on cancellation corrupts every stream consumer.
- A successful write reaches its backend's completion boundary (the README table) and nothing more; never document a backend write as delivery or command acceptance.
- `unsafe_code` is `deny`, not `forbid`, for the macOS `inner` module of `src/bluetooth.rs` alone (the helper shim's C ABI, pipe flags, the liveness descriptor). `lint.sh` audits that exact path, so unsafe anywhere else, `src/bluetooth/types.rs` included, fails the gate; every block needs a `SAFETY` comment (`undocumented_unsafe_blocks`).
- The native helper is the consumer's own executable. `build.rs` compiles `src/bluetooth_mac.m` with `native-bluetooth` on macOS, and its `__attribute__((constructor))` takes over a process started with `KENWOOD_BT_HELPER_PROCESS_V2` before Rust `main`, so consumers call no hook. Every spawn path calls `bt_helper_link_anchor()`: without a Rust reference the static archive member and its constructor can be dropped from the link, and the child then runs the consumer's own `main`.
- `src/bluetooth_startup_tests.m` and `src/bluetooth_open_failure_tests.m` are `#include`d into the shim and run inside the helper under `KENWOOD_BT_HELPER_TEST_MODE` (the sentinel is still required); they never call IOBluetooth. `build.rs` reruns on all three `.m` files.
- The macOS `inner` module compiles only on macOS, so `./lint.sh` and the macOS workflow (`--no-default-features --features native-bluetooth`) are the only gates that build native changes; elsewhere the feature compiles just the portable selector types.
- `MockTransport` scripting rules for consumers are in the root `AGENTS.md` section `Test code`.
