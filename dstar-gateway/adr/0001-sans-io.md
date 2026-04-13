# 0001 — Sans-IO core crate

- **Status**: accepted
- **Date**: 2026-04-10
- **Deciders**: swiftraccoon

## Context

The previous iteration of `dstar-gateway` combined wire-format
encoding, state-machine logic, and tokio I/O in a single crate. This
coupling made three categories of work painful:

1. **Testing.** Every state-machine test needed a real tokio
   runtime, a real UDP socket, and a real fake reflector. Even
   with `tokio::test`, opening a port and round-tripping a packet
   took enough wall time that the test suite became noticeably
   slow. Worse, port-allocation races occasionally surfaced on CI
   under load, causing flaky failures that were hard to reproduce.
2. **Determinism.** The codec and the state machine both read the
   system clock for keepalive and timeout logic. Property tests had
   to either mock the clock at great expense or skip timer branches
   entirely — in practice, the timer branches stayed under-tested.
3. **Portability.** Embedding the crate in a WebAssembly module or
   a no-tokio async runtime was impossible because the codec
   reached for tokio types in its signatures.

We looked at quinn, rustls, and h2 for inspiration. All three use
a sans-io core pattern: the codec and the protocol state machine
live in a runtime-agnostic crate that accepts byte slices and
returns byte slices plus events, and a thin shell crate wires that
core to a specific async runtime.

We explicitly asked whether sans-io was worth the extra boilerplate
of the two-crate split. The answer was
yes, primarily because the alternative — a small tokio integration
test that pretends to be a unit test — is a well-known anti-pattern
and we wanted to exit this state definitively rather than incrementally.

## Decision

`dstar-gateway-core` is a sans-io crate. It has no dependency on
tokio, no dependency on any async runtime, and no dependency on the
standard library's `std::time::Instant` except where it is passed
in from the caller. Every codec function and every state-machine
step is a pure function from `(current state, input bytes, now)`
to `(new state, output bytes, events)`.

The tokio shell lives in a separate crate `dstar-gateway` that
depends on `dstar-gateway-core`. The shell owns the `UdpSocket`,
the `tokio::select!` driver loop, and the command/event channels.
It is the only crate in the workspace that depends on tokio.

## Consequences

### Positive

- Every wire-format bug can be reproduced in a unit test with zero
  async setup.
- Property tests using `proptest` run ~100x faster than the
  previous async tests.
- Time is injected, so timer-driven state transitions are
  deterministically testable.
- The core can be embedded in a WebAssembly module, a no-tokio
  runtime, or a test harness that mocks the clock. The conformance
  replay harness already uses this to feed pcap fixtures into the
  codec without booking a port.
- The ten `cargo-fuzz` targets under
  `dstar-gateway-core/fuzz/fuzz_targets/` can target the core
  directly: each target is a `Vec<u8> -> Result<_, _>` function,
  which is the ideal shape for libfuzzer.

### Negative

- Users must understand the two-crate split: which layer a given
  type lives in. We mitigate this by re-exporting the common types
  from `dstar-gateway` at the crate root, so `use
  dstar_gateway::{Callsign, Module}` works without importing the
  core crate.
- Writing a new runtime shell (e.g. `async-std` or `smol`) requires
  duplicating the ~100-line driver loop. Acceptable trade-off; the
  driver is intentionally small.
- The typestate state machine is slightly more verbose because
  each state is a distinct type. ADR 0002 addresses the typestate
  choice separately.

## Alternatives considered

- **Runtime-agnostic trait object**. Accept a `trait AsyncSocket`
  boundary instead of splitting the crate. Rejected because
  dynamic dispatch on the hot path is measurable and because it
  doesn't solve the clock injection problem.
- **Async trait with default tokio implementation**. Considered
  and rejected for the same reasons plus the `#[async_trait]`
  overhead.
- **Tokio-first with a `no-tokio` feature flag**. Considered but
  rejected because feature-gated async code is famously hard to
  test and the ergonomics of `tokio::select!` become awkward when
  half the branches are `cfg`-gated.

## References

- quinn's `quinn-proto` crate: the canonical example of this
  pattern in the async-networking space.
- `dstar-gateway-core/fuzz/fuzz_targets/`: direct consumers of
  the sans-io core.
