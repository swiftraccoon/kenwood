# 0001: Sans-io core crate

Status: accepted.

## Decision

Keep byte codecs and protocol state machines in dstar-gateway-core.
The core uses std but has no async-runtime dependency, opens no I/O resource,
and obtains no clock value. Callers inject Instant values and drain output
datagrams/events.

Network clients live in dstar-gateway; reflector endpoints live in
dstar-gateway-server. These shells own socket operations, task lifetimes,
clock reads and completion errors.

## Consequences

Protocol tests can drive exact input bytes and times without networking.
Golden vectors, properties, state regressions and fuzz targets can exercise
the core directly. Loopback tests separately cover shell I/O and cancellation.

An embedding application must supply an event loop and keep draining the
core's queues. A different runtime needs its own I/O driver while reusing the
same checked state/wire logic. Supporting a target still requires validating
that target's std/runtime capabilities; sans-io alone is no portability claim.

The split adds explicit dependencies for callers that drive core sessions.
The client shell reexports selected leaf values; its direct session/driver
users import dstar-gateway-core.

## Alternatives

Putting socket methods in the state machine would couple core tests and
clock behavior to one runtime. An async socket trait would still require
separate time/cancellation contracts and provides no demonstrated benefit
for the current concrete Tokio and synchronous drivers.

## Implementation

- dstar-gateway-core/src/session/driver.rs: input, time and queue interface.
- dstar-gateway/src/tokio_shell/: asynchronous I/O implementation.
- dstar-gateway/src/blocking_shell/: caller-driven synchronous implementation.
- dstar-gateway-core/fuzz/: direct parser/state fuzz entry points.
