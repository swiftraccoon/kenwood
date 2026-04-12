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
+----------------------------------------------------------+
|                    dstar-gateway-server                  |
|  (multi-client reflector: UdpSocket + ClientPool + fanout) |
+----------------------------------------------------------+
|                       dstar-gateway                       |
|     (tokio shell: AsyncSession<P>, AuthClient, hosts-fetcher) |
+----------------------------------------------------------+
|                    dstar-gateway-core                    |
|     (sans-io codec + typestate Session<P, S> state machines) |
+----------------------------------------------------------+
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
  depends on the other two and layers a `ClientPool`, a `Reflector`,
  and a fan-out engine on top.

The motivation for keeping the core sans-io is testability:
**every wire-format bug can be reproduced by hand in a unit test**
without booking a UDP port, spinning up a tokio runtime, or
introducing test flakiness. See ADR 0001.

## 2. Sans-io core + typestate client session

Inside `dstar-gateway-core` the code is structured around two
abstractions that compose:

### 2.1 The codec (`codec/`)

The codec module contains three submodules — `dplus`, `dextra`, and
`dcs` — each of which exports pure functions: `encode_header`,
`encode_voice`, `parse_header`, `parse_voice`, `parse_auth_response`,
and so on. These functions take `&[u8]` slices and a
`&mut DiagnosticSink`, and return `Result<Event, CoreError>`. They
are completely independent of any runtime. Golden fixtures in
`codec/*/tests/` pin every byte of every wire format against bytes
captured from `ircDDBGateway` and `xlxd`.

### 2.2 The typestate `Session<P, S>` (`session/client/`)

The `Session<P, S>` type parameterizes over the **protocol** `P`
(`DPlus`, `DExtra`, `DCS`) and the **state** `S` (`Configured`,
`Authenticated`, `Connecting`, `Connected`, `Disconnecting`,
`Disconnected`, or `Failed<S, E>`). A state machine built this way
makes invalid state transitions a compile error: you cannot call
`send_voice` on a `Session<DExtra, Configured>` because only
`Session<DExtra, Connected>` has that method.

The state transitions are one-way except for `Failed<S, E>`, which
lets the caller inspect the last valid state and the error, then
either retry or give up. See ADR 0002.

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

- `dstar-gateway-core::Error` — codec, validator, and state-machine
  errors. No variant ever holds a tokio type or an `io::Error`.
- `dstar-gateway::ShellError` — wraps `Error` plus tokio-specific
  cases: channel closed, session task panicked, socket bind failed.
- `dstar-gateway::AuthError` — wraps `Error` plus TCP-auth-specific
  cases: timeout phase, write phase, read phase.
- `dstar-gateway-server::ShellError` — wraps `Error` plus server-
  specific cases: protocol not supported, endpoint task aborted.

Every error type implements `thiserror::Error`, derives `Debug`,
and has a 1:1 mapping to a single test that constructs it and
asserts the display string. The display string is stable: consumers
can match on it in logs.

## 5. Lenient parsing + structured diagnostics

Every codec function takes a `&mut impl DiagnosticSink`. When the
codec encounters a field that is malformed but recoverable (e.g. a
trailing byte after a well-formed packet, a reserved bit set, a
UTF-8 suffix that isn't canonical) it calls
`sink.emit(Diagnostic { ... })` rather than returning an error.

This has three consequences:
- Real-world captures from ircDDBGateway-era reflectors parse
  cleanly, even though some of them have quirks that a strict
  parser would reject.
- Consumers get structured observability: wire one `DiagnosticSink`
  implementation into `tracing` and every nonstandard byte becomes a
  structured log event with a stable code.
- Strict mode is still available via `StrictnessFilter`: wrap a
  real sink and have it return `Err` on any diagnostic above a
  severity threshold, then pass that filter into the codec and the
  error surfaces as a real parse failure.

See ADR 0004.

## 6. The tokio shell model

`dstar-gateway::tokio_shell::AsyncSession<P>` is the user-facing
handle. Internally it owns two channels (command and event) and
spawns a `SessionLoop` task that owns the `UdpSocket` and the
`Session<P, Connected>`.

The driver loop is a `select!` that reads from the socket, advances
the state machine, drains the outbox back to the socket, and pumps
events into the event channel. It is intentionally small (~100
lines) so the complexity budget stays in the core crate.

`AuthClient` is a separate, synchronous-shape async helper that
owns a transient TCP connection. It has no relationship to the UDP
session; you call it once before building a `Session<DPlus,
Authenticated>` and then throw it away.

## 7. The reflector server

`dstar-gateway-server::Reflector` owns a `ProtocolEndpoint<P>` per
enabled protocol. An endpoint owns:
- a `ClientPool<P>` — a hash map from peer `SocketAddr` to
  `ServerSessionCore<P>` wrapped in a `ClientHandle<P>`, with a
  reverse index from `Module` to `HashSet<SocketAddr>`
- a `UdpSocket`
- a shutdown `watch::Receiver<bool>`

The run loop is a `select!` over `socket.recv_from` and
`shutdown.changed`. Each received datagram is routed via the pool,
the per-peer core is driven, any emitted events are turned into
`ForwardHint`s, and the raw bytes are re-broadcast to every other
peer on the same module by `fan_out_voice`.

Only DExtra has a shell today. The DPlus and DCS server typestates
exist in the core crate but no shell is wired yet — a design
decision driven by the fact that DExtra is the interop backbone for
XLX reflectors and DPlus/DCS can be added incrementally without
reworking the pool or fan-out.

## 8. Testing strategy

dstar-gateway maintains an 8-tier test pyramid:

1. **Unit tests** next to the code. Each codec function, each
   typestate transition, each client pool operation has its own unit
   test.
2. **Property tests** (`proptest`) for every encoder/decoder pair:
   generate random input, round-trip, assert identity. ~75 props.
3. **Golden fixtures** — raw bytes captured from ircDDBGateway and
   xlxd, committed to the tree, parsed by every codec test.
4. **Loopback integration tests** — full tokio shell +
   `FakeReflector` over a real loopback UDP socket. One per
   protocol covers connect / voice / disconnect round-trips.
5. **Compile-fail tests** (`trybuild`) — snapshots of rustc errors
   that verify the typestate actually prevents invalid transitions.
6. **Fuzz targets** — ten `cargo-fuzz` harnesses, one per parser
   entry point. Baseline: ~1.1 billion executions across all
   targets with zero crashes.
7. **Conformance pcap replay** — a `pcap-parser` harness that
   feeds captured UDP traffic through every codec. The corpus
   itself is external; the runner is in-tree.
8. **Hardware-in-the-loop** — tests that drive a real reflector
   over the network with the operator's TH-D75 attached. Gated
   behind the `hardware-tests` feature + `#[ignore]` + an opt-in
   `DSTAR_TEST_TX_OK` env var so nobody accidentally keys the air.

The workspace total is around **2,200 tests**. They all run in
well under a minute.

## Further reading

- [`REFERENCES.md`](REFERENCES.md) — every reference we made to
  ircDDBGateway or xlxd, pinned to a commit hash.
- [`adr/`](adr/) — architectural decision records. Read
  `0001-sans-io.md` and `0002-typestate.md` first if you want to
  understand *why* the core is shaped the way it is.
- [The mdBook](https://swiftraccoon.github.io/dstar-gateway/) — a
  longer narrative version of the above.
