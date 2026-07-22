# dstar-gateway Architecture

This document gives a 5000-foot view of how `dstar-gateway` is laid
out. It is aimed at developers who want to understand the codebase
well enough to contribute or to embed the library in their own
application. The per-protocol wire-format details live in
[REFERENCES.md](REFERENCES.md) and the per-release change log lives
in [CHANGELOG.md](CHANGELOG.md).

## 1. The three-crate split

dstar-gateway is shipped as **three** crates so that consumers can
depend on exactly the layer they need and so that each crate can
enforce its own dependency boundary.

```
+------------------------------+  +------------------------------+
|    dstar-gateway-server      |  |       dstar-gateway          |
| Reflector + ClientPool       |  | AsyncSession + AuthClient    |
+---------------+--------------+  +--------------+---------------+
                \                            /
                 +------------+-------------+
                 |   dstar-gateway-core      |
                 | codecs + session machines |
                 +---------------------------+
```

- **`dstar-gateway-core`** has zero I/O dependencies. No tokio, no
  sockets, no filesystem access. It is a pure `&[u8] <-> types`
  library. You can compile it for `no_std`-adjacent embedded targets
  (desktop `std` is still required for collections but not for I/O)
  or for WebAssembly without reaching for a custom runtime shim.
- **`dstar-gateway`** is the tokio shell. It pulls in `tokio`, owns
  the `UdpSocket`, and spawns the session driver loop. It is the
  right dependency for the common case: an async application talking
  to a reflector.
- **`dstar-gateway-server`** is the multi-client reflector server. It
  depends directly on `dstar-gateway-core` (not the client shell) and
  layers a `ClientPool`, a `Reflector`, and a fan-out engine on top.

The motivation for keeping the core sans-io is testability:
**every wire-format bug can be reproduced by hand in a unit test**
without booking a UDP port, spinning up a tokio runtime, or
introducing test flakiness. See ADR 0001.

## 2. Sans-io core + typestate client session

Inside `dstar-gateway-core` the code is structured around two
abstractions that compose:

### 2.1 The codec (`codec/`)

The codec module contains three submodules (`dplus`, `dextra`, and
`dcs`), each of which exports pure functions: encoders like
`encode_voice_header`, `encode_voice_data`, and `encode_voice_eot`,
decoders like `decode_server_to_client` and
`decode_client_to_server`, plus DPlus's `parse_auth_response`.
The decoders take `&[u8]` slices and a `&mut dyn DiagnosticSink`
and return `Result` values carrying the decoded packet or a
protocol error. They
are completely independent of any runtime. Golden fixtures in
`dstar-gateway-core/tests/golden/{dplus,dextra,dcs}/` pin every byte of
every wire format against bytes captured from `ircDDBGateway` and `xlxd`,
and are exercised by the `codec_{dplus,dextra,dcs}_golden` test runners.

### 2.2 The typestate `Session<P, S>` (`session/client/`)

The `Session<P, S>` type parameterizes over the **protocol** `P`
(`DPlus`, `DExtra`, `Dcs`) and the **state** `S` (`Configured`,
`Authenticated`, `Connecting`, `Connected`, `Disconnecting`, or
`Closed`). A state machine built this way
makes invalid state transitions a compile error: you cannot call
`send_voice` on a `Session<DExtra, Configured>` because only
`Session<DExtra, Connected>` has that method.

Fallible consuming transitions return `Failed<S, E>`, a wrapper that
preserves the last valid session and the error so the caller can retry
or give up. See ADR 0002.

### 2.3 The server typestate (`session/server/`)

The same approach is applied to the server side: a
`ServerSessionCore` lives in the core crate and drives per-client
state machines. This is what the reflector server consumes. See ADR
0003 for why client and server share the codec.

## 3. Symmetric client and server

Historically, D-STAR reflector libraries have been asymmetric:
ircDDBGateway implements only the client half, xlxd implements only
the server half. `dstar-gateway` reuses the same codec and the same
state machines on both sides. The benefit is that a bug fix in the
codec immediately benefits both the client and the server, and that
the fuzz corpus we maintain against the wire formats exercises both
the encoder and the decoder on every target.

This symmetry is the reason ADR 0003 exists: it is worth the extra
design friction to get codec reuse, because the codec is where the
correctness risk lives.

## 4. The error hierarchy

Errors are layered, not flattened. Each crate owns its error types,
and higher layers wrap lower-layer errors with context:

- `dstar-gateway-core::Error`: codec, validator, and state-machine
  errors. No variant ever holds a tokio type or an `io::Error`.
- `dstar-gateway::ShellError`: wraps the core error (`Core`) and
  channel/task closure (`SessionClosed`). The legacy
  `DisconnectTimeout` variant is retained but not returned by the
  current shell; timeout outcomes arrive as disconnect events.
- `dstar-gateway::AuthError` covers the TCP-auth-specific cases: `Io` (tagged
  with the connect/write/read operation), `Timeout` (tagged with the
  phase), and `Parse`, which wraps a `DPlusError` from the auth
  response parser.
- `dstar-gateway-server::ShellError`: wraps the core error (`Core`)
  plus `Protocol(String)` (framing and unexpected-variant failures, which
  also carry the endpoint-task-aborted case) and `Io`.

Every error type implements `std::error::Error` and `Debug`. Tests cover
representative formatting and conversion paths; consumers should match
typed variants rather than treating display strings as a stable API.

## 5. Lenient parsing + structured diagnostics

Every codec decoder takes a `&mut dyn DiagnosticSink`. When the
codec encounters a field that is malformed but recoverable (e.g. a
trailing byte after a well-formed packet, a reserved bit set, a
UTF-8 suffix that isn't canonical) it calls `sink.record(diagnostic)`
(where `Diagnostic` is an enum variant) rather than returning an
error. `DiagnosticSink::record` returns `()`, so a decoder never
fails on a recoverable quirk.

This has three consequences:
- Real-world captures from ircDDBGateway-era reflectors parse
  cleanly, even though some of them have quirks that a strict
  parser would reject.
- Consumers get structured observability: the shipped `TracingSink`
  forwards every recorded diagnostic to `tracing`, so each
  nonstandard byte becomes a structured log event.
- Strict rejection is not a shipped type but a pattern a caller can
  implement inside its own `record` impl: record the diagnostic,
  then treat any recorded diagnostic as a hard failure in the
  caller's own downstream logic. The crate ships `NullSink`,
  `VecSink`, and `TracingSink`; anything stricter is a custom impl.

See ADR 0004.

## 6. The tokio shell model

`dstar-gateway::tokio_shell::AsyncSession<P>` is the user-facing
handle. Internally it owns three channels: a command channel, an
event channel, and a peer-activity `watch` channel (`activity_rx`)
that surfaces link-health timing. It also spawns a `SessionLoop` task
that owns the `UdpSocket` and the `Session<P, Connected>`.

The driver loop is a `select!` that reads from the socket, advances
the state machine, drains the outbox back to the socket, and pumps
events into the event channel. It is intentionally small (~100
lines) so the complexity budget stays in the core crate.

`AuthClient` is a separate async helper that
owns a transient TCP connection. It has no relationship to the UDP
session; call it to obtain a host list, attach that list with
`Session::authenticate`, and then discard the client.

## 7. The reflector server

`dstar-gateway-server::Reflector` owns a `ProtocolEndpoint<P>` per
enabled protocol. An endpoint owns:
- a `ClientPool<P>`: a hash map from peer `SocketAddr` to
  `ServerSessionCore` wrapped in a `ClientHandle<P>`, with a
  reverse index from `Module` to `HashSet<SocketAddr>`
- a `UdpSocket`
- a shutdown `watch::Receiver<bool>`

The run loop is a `select!` over `socket.recv_from` and
`shutdown.changed`. Each received datagram is routed via the pool,
the per-peer core is driven, any emitted events are turned into
`ForwardHint`s, and the raw bytes are re-broadcast to every other
peer on the same module by `fan_out_voice`.

All three protocols are wired at the shell level. The `Reflector`
constructs and spawns one `ProtocolEndpoint<P>` per enabled
protocol, and each inbound datagram is dispatched by protocol
through a dedicated `handle_inbound_dextra` / `handle_inbound_dplus`
/ `handle_inbound_dcs` helper. `ReflectorConfig::enabled_protocols`
defaults to all three, so a default-configured server accepts
DExtra, DPlus, and DCS clients simultaneously.

## 8. Testing strategy

dstar-gateway maintains an 8-tier test pyramid:

1. **Unit tests** next to the code. Each codec function, each
   typestate transition, each client pool operation has its own unit
   test.
2. **Property tests** (`proptest`) for encoder/decoder pairs:
   generate random input, round-trip, and assert the documented invariant.
3. **Golden fixtures**: raw bytes captured from ircDDBGateway and
   xlxd, committed to the tree, parsed by every codec test.
4. **Loopback integration tests**: full tokio shell +
   `FakeReflector` over a real loopback UDP socket. One per
   protocol covers connect / voice / disconnect round-trips.
5. **Compile-fail tests** (`trybuild`): snapshots of rustc errors
   that verify the typestate actually prevents invalid transitions.
6. **Fuzz targets**: ten `cargo-fuzz` harnesses covering parser
   entry points and exercised by the fuzz workflow.
7. **Conformance pcap replay**: a `pcap-parser` harness that
   feeds captured UDP traffic through every codec. The corpus
   itself is external; the runner is in-tree.
8. **Live-reflector integration**: ignored tests that connect to
   real reflectors, plus an explicitly opted-in short voice burst. Gated
   behind the `hardware-tests` feature + `#[ignore]` + an opt-in
   `DSTAR_TEST_TX_OK` env var so nobody accidentally keys the air.

The workspace contains thousands of unit, property, integration, and
compile-fail cases. Full runtime varies by host and feature matrix.

## Further reading

- [`REFERENCES.md`](REFERENCES.md): every reference we made to
  ircDDBGateway or xlxd, pinned to a commit hash.
- [`adr/`](adr/): architectural decision records. Read
  `0001-sans-io.md` and `0002-typestate.md` first if you want to
  understand *why* the core is shaped the way it is.
- [`book/`](book/): mdBook source for a longer narrative version of
  the above. It currently covers the introduction and Getting Started;
  the remaining chapters are unwritten. Build it locally with
  `mdbook serve book`.
