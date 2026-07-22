# 0003: Symmetric client and server sharing the codec

- **Status**: accepted
- **Date**: 2026-04-10
- **Deciders**: swiftraccoon

## Context

Historically, D-STAR reflector software has been split along the
client/server divide:

- `ircDDBGateway` (the canonical implementation) is a **client**:
  it runs on the operator's machine, talks to a radio over the
  MMDVM protocol, and to a reflector over UDP. It has no code for
  accepting inbound UDP connections from other clients.
- `xlxd` (the canonical reflector implementation) is a **server**:
  it binds UDP ports and fans traffic between multiple connected
  clients. It has no code for talking to a radio.

The two projects share nothing except the wire format, which they
each independently re-implement. Any bug fix in the shared wire
format has to be done twice, and divergence between the two
implementations is a known source of interoperability problems (see
the `xlxd` trailer-byte note in
`dstar-gateway-core/src/validator/diagnostic.rs:96`).

The question the rewrite faced: should `dstar-gateway` implement
only the client half (staying in scope with `ircDDBGateway`) or
should it implement both halves and share the codec?

## Decision

`dstar-gateway` implements **both the client and the server**, and
they share the same `dstar-gateway-core` crate for the wire format.

- Client sessions use `Session<P, S>` with client state markers:
  `Configured`, `Authenticated`, `Connecting`, `Connected`, etc.
- Server sessions use the `ServerSession<P, S>` typestate (backed
  by the non-generic `ServerSessionCore`) with server state markers:
  `Unknown`, `Link1Received`, `Linked`, `Streaming`, `Unlinking`,
  and `Closed`.
- **Both** consume the same `codec::<protocol>::encode_*` /
  `parse_*` functions. An encoder bug fix lands once and benefits
  both sides.

The reflector server lives in its own crate
`dstar-gateway-server` so that client-only consumers do not pay
for the server dependencies (the client-only feature set can
drop `ClientPool`, `Reflector`, and fan-out code entirely).

## Consequences

### Positive

- **Zero codec duplication.** Every wire-format constant is
  defined exactly once in `dstar-gateway-core::codec::*::consts`.
  Fuzz corpus coverage automatically applies to both directions.
- **Symmetric testing.** The client and server can be driven
  against each other in a single tokio test, using the real codec
  on both sides. This catches interop bugs that only surface when
  the encoder and decoder agree about a non-standard field.
- **Single reference for cross-protocol forwarding.** The implemented
  optional forwarding path can bridge two protocols (e.g. DExtra
  module A to DCS module A) can use a `codec::dextra::parse` →
  `codec::dcs::encode` pipeline, all inside the same crate.

### Negative

- **Dependency scope.** Server consumers still pay for tokio and the
  reflector shell, but not for the client shell: the server depends
  directly on `dstar-gateway-core`. Consumers import shared core types
  explicitly because the server crate does not re-export them.
- **Larger test surface.** Every wire-format test now has to
  verify both the encoder and the decoder round-trip. We use
  property tests (`proptest::roundtrip!`) to make this cheap.
- **Parallel but distinct typestates.** Client sessions use
  `Session<P, S>` and server sessions use `ServerSession<P, S>`,
  backed by the non-generic `ServerSessionCore`. The two state sets
  differ (the server side adds `Link1Received`, `Streaming`, and
  `Unlinking` markers for the reflector's inbound handshake), so the
  typestate machinery is written twice rather than shared. Methods
  stay state-gated on both sides: `handle_voice_data` is only on
  `ServerSession<P, Streaming>`, `handle_link2` on
  `ServerSession<DPlus, Link1Received>`, and `handle_unlink` on
  `ServerSession<P, Linked>`, each guarded by compile-fail tests.

## Alternatives considered

- **Client-only (match `ircDDBGateway` scope)**. Rejected because
  the fuzzing investment and the golden fixtures should benefit
  xlxd-shaped workloads too.
- **Server-only (match `xlxd` scope)**. Rejected because
  the primary consumer (`thd75-repl`) is a client.
- **Two separate codec crates, one per side**. Rejected as the
  duplication-of-constants outcome we wanted to avoid.
- **Server as a cargo workspace sub-crate of the client**. This
  is what we actually did, but "symmetric" vs "sub-crate" is a
  framing difference; both share the codec.

## References

- `dstar-gateway-core/src/session/server/`: server-side
  typestate + `ServerSessionCore` state machine.
- `dstar-gateway-server/src/`: tokio shell, reflector top-level
  type, fan-out engine, cross-protocol transcode.
