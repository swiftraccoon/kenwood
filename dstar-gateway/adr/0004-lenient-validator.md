# 0004 — Lenient parsing with opt-in strict validator

- **Status**: accepted
- **Date**: 2026-04-10
- **Deciders**: swiftraccoon

## Context

Strict parsers are well-behaved: they reject anything that isn't
exactly what the spec says, and they produce clean, informative
errors for every malformed byte. The D-STAR reflector protocols do
not reward this discipline.

Across the real-world reflector ecosystem there are at least three
variants of DPlus, two variants of DCS, and one variant of DExtra
that each have small deviations from the canonical
`ircDDBGateway`/`xlxd` wire format:

- Some reflectors emit a trailing `0xFF` byte on DCS voice frames
  (noted in `dstar-gateway-core/src/validator/diagnostic.rs:96`).
- Some reflectors use a different magic byte in the DPlus keepalive
  packet.
- Some reflectors emit a DPlus OKRW packet with the `R` byte
  replaced by `W` (the semantic meaning is the same).
- Some reflectors include an extra null byte at the end of the
  DCS LINK packet.

A strict parser would reject all four of these with a generic
"unexpected byte" error, and consumers would have to either fork
the library or wrap the parser in a retry loop that ignores errors.
Both outcomes lose information about *what* is wrong, which is
what the consumer actually wants.

At the same time, a parser that silently accepts everything is
also wrong. A consumer writing a diagnostic tool needs to know
that the reflector is emitting non-canonical bytes so they can
report it.

## Decision

The codec parses **leniently** by default but **emits structured
diagnostics** for every deviation. The two ideas are combined in
the `DiagnosticSink` trait:

```rust
pub trait DiagnosticSink {
    fn emit(&mut self, diag: Diagnostic);
}
```

Every codec function takes `&mut impl DiagnosticSink`. When the
parser encounters a recoverable deviation (trailing byte, non-
canonical magic byte, etc.) it calls `sink.emit(Diagnostic { ... })`
with a stable, machine-readable code and then continues parsing.

Consumers who want a strict parser wrap their sink in the provided
`StrictnessFilter`:

```rust
let mut sink = StrictnessFilter::new(NullSink, Severity::Warning);
// ...
let result = codec::dplus::parse_header(&bytes, &mut sink);
// result is Err if the parser emitted any diagnostic >= Warning.
```

The default sink `NullSink` drops every diagnostic silently, which
is what the test suite and the high-performance path use. A
production tracing integration wires a `TracingSink` that forwards
diagnostics as structured `tracing` events.

## Consequences

### Positive

- **Real-world reflectors parse cleanly.** The loopback integration
  tests pass against all three canonical reflectors plus every
  variant we've encountered in the wild.
- **Observability is free.** Wiring a `TracingSink` into the shell
  turns every wire-format quirk into a structured log event with a
  stable code. Consumers can filter on `dgstar_gateway::diag::dcs::extra_trailing_byte`
  without parsing text.
- **Strict mode is still available.** Tools that want strict
  validation (e.g. a conformance test runner, or a CI check that a
  new reflector is spec-compliant) wrap their sink in
  `StrictnessFilter` and get the error behavior they want.
- **No version skew.** Adding a new lenient-parse case does not
  break existing consumers because the default sink ignores the
  new diagnostic. Callers who care set up their own filter.

### Negative

- **Every codec function has an extra parameter.** All public
  parse functions take `&mut impl DiagnosticSink`. This is one
  extra argument at every call site. We mitigate this by providing
  convenience wrappers for the common case of "use NullSink".
- **The `Diagnostic` type needs careful versioning.** Adding new
  variants to the enum is additive and safe, but changing existing
  variants is a breaking change. We use `#[non_exhaustive]` on the
  enum to preserve forward compatibility.
- **"Is this a bug or a lenient parse?" is harder to answer.** A
  consumer seeing unexpected output has to check whether a
  diagnostic was emitted. We mitigate this with `StrictnessFilter`
  as the recommended debug setup.

## Alternatives considered

- **Strict-by-default**. Rejected because it makes real-world
  reflectors unusable without a manual retry wrapper.
- **Lenient-by-default with no observability**. Rejected because
  it hides information from consumers who need it.
- **Two separate codec functions per operation (`parse_strict` vs
  `parse_lenient`)**. Rejected because the code duplication would
  be extensive and the two implementations would drift.
- **A feature flag**. Rejected because feature-gated behavior is
  hard to document and hard to test.

## References

- `dstar-gateway-core/src/validator/` — `DiagnosticSink` trait
  plus `NullSink`, `VecSink`, `TracingSink`, and
  `StrictnessFilter` implementations.
- `dstar-gateway-core/src/codec/{dplus,dextra,dcs}/decode.rs`
  — every decoder takes `&mut dyn DiagnosticSink` so consumers
  opt into strictness per call site.
- `dstar-gateway-core/src/session/client/core.rs` — lenient
  client-input path that swallows unknown-length datagrams so
  reflector-emitted traffic never kills an established session.
