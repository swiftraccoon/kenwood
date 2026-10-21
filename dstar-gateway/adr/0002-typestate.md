# 0002: Typed client transitions with runtime driving

Status: accepted.

## Decision

Session<P, S> uses sealed protocol/state markers. The builder requires checked
station, local/target module and peer values; DCS additionally requires its
target identity. DPlus attaches a host list before entering Connecting.
Consuming transitions return a new wrapper or Failed<S, E>, retaining the
session and typed error.

Only the Connected wrapper exposes voice sends. Driver callbacks, timeout
and in-place disconnect can subsequently change its runtime state; sends
therefore also check that runtime state and return WrongState after closure
or disconnect. Typestate constrains available operations; it does not freeze
network state.

## Consequences

Compile-fail tests pin unavailable operations and missing builder inputs.
Runtime tests separately exercise timeout, disconnect, reply correlation,
receive phase and state rejection. Both are necessary for the public contract.

AnySession<P> stores an explicitly selected state wrapper for callers that
need state erasure. Live wrappers are uniquely owned; Failed returns the
original resource on a rejected consuming transition.

Sealing markers prevents arbitrary downstream state/protocol implementations.
Driver remains a supported extension interface for other event loops.

## Alternatives

A single runtime enum would make every operation available at compile time.
A builder-only marker would constrain construction but provide no method
availability for consuming transitions. Typed wrappers plus runtime checks
cover both boundaries without pretending that I/O state is immutable.

## Implementation

- dstar-gateway-core/src/session/client/session.rs: operation implementations.
- dstar-gateway-core/src/session/client/core.rs: runtime state checks.
- dstar-gateway-core/tests/compile_fail/: operation and builder refusals.
- dstar-gateway/tests/compile_fail/: spawn accepts only Connected wrappers.
