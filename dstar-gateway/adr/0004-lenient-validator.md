# 0004: Typed framing errors and recoverable diagnostics

Status: accepted.

## Decision

Directional decoders return typed errors for fatal framing failures.
Implemented recoverable conditions are recorded through DiagnosticSink:

```rust
pub trait DiagnosticSink {
    fn record(&mut self, diagnostic: Diagnostic);
}
```

Diagnostic is non-exhaustive. NullSink discards observations; VecSink retains
them; TracingSink emits structured tracing records. A sink returns unit, so
recording does not itself change the decoder's Result.

Receive values preserve unknown raw bytes. Their checked text/domain views
let callers inspect them without lossy replacement. A caller requiring a
stricter acceptance policy can inspect diagnostics and the returned value
before using it.

## Consequences

Recoverable input does not require discarding an otherwise usable packet.
The client dispatcher logs and ignores malformed/unknown datagrams after its
peer guard, preserving the session; runtime state, timer and I/O errors have
their separate typed failure contracts.

Diagnostics cover their documented implemented conditions. They certify
neither exhaustive validation of every field nor compatibility with every
reflector revision. Golden vectors, independent primary-source comparisons,
properties and state tests validate distinct contracts.

Adding a diagnostic variant is an API evolution decision. Callers match the
non-exhaustive enum with an explicit fallback; changing an existing variant
or its semantics requires consumer review.

## Alternatives

Separate strict/lenient decoder copies could drift. A Cargo feature selecting
one global acceptance policy would prevent two consumers in the same build
from making different decisions. Typed fatal errors plus supplied diagnostic
sinks retain one decoder and an explicit per-call policy boundary.

## Implementation

- dstar-gateway-core/src/validator/: observations and sink contracts.
- dstar-gateway-core/src/codec/: fatal/diagnostic parsing behavior.
- dstar-gateway-core/src/session/client/core.rs: source/state dispatch policy.
