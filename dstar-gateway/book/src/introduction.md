# Introduction

`dstar-gateway` is an async Rust library for speaking the three
D-STAR reflector protocols — **DPlus** (REF reflectors, port
20001), **DExtra** (XRF/XLX reflectors, port 30001), and **DCS**
(DCS reflectors, port 30051). It provides a typed client API, a
multi-client reflector server, and a runtime-agnostic sans-io core
suitable for embedding in test harnesses, alternate async runtimes,
or WebAssembly modules.

This book is the narrative companion to the [rustdoc API
reference](https://docs.rs/dstar-gateway). It exists to answer the
"why" questions that rustdoc can't: *why* does the library split
into three crates, *why* is the client session a typestate, *why*
is parsing lenient by default, *why* does the server use a runtime
state enum when the client uses compile-time states. If you want
to embed the library in your own project and never touch its
internals, the [Getting Started](getting-started/what-is-dstar.md)
chapters are enough. If you want to contribute or to adapt the
design for a different protocol family, the [Type System](typestate/why.md)
and [Sans-IO Core](sans-io/why.md) chapters are the places to go.

## The three crates

`dstar-gateway` ships as three crates:

- **`dstar-gateway-core`** — the sans-io core. Contains the wire
  format codecs for all three protocols, the typestate
  `Session<P, S>` state machines, the slow-data sub-codec, the
  DPRS parser, and the server typestate skeleton. Has no
  dependency on tokio, no dependency on the filesystem, and no
  dependency on an async runtime. You drive it by calling pure
  functions that take byte slices and return byte slices plus
  events.
- **`dstar-gateway`** — the tokio shell. Wraps `dstar-gateway-core`
  in a `tokio::net::UdpSocket`-backed driver loop, spawns it as a
  task, and exposes an `AsyncSession<P>` handle with the familiar
  `send_header` / `send_voice` / `send_eot` / `disconnect` /
  `next_event` methods. Also contains the DPlus TCP `AuthClient`,
  the optional `blocking` feature (a non-tokio variant of the
  shell for CLI scripts), and the optional `hosts-fetcher`
  feature (pulls `reqwest` and downloads the Pi-Star host files).
- **`dstar-gateway-server`** — the reflector server. Provides the
  `Reflector` type, the `ClientPool<P>`, the `fan_out_voice`
  function, the `ClientAuthorizer` trait, and an optional
  cross-protocol forwarding bus. The client side supports all
  three protocols; the server side currently ships `DExtra`
  endpoint support with `DPlus` and `DCS` server endpoints
  queued as open work.

See [ARCHITECTURE.md](https://github.com/swiftraccoon/dstar-gateway/blob/main/dstar-gateway/ARCHITECTURE.md)
for a denser 5000-foot overview of the same ground.

## Design philosophy

Three ideas drive every design decision in the library:

1. **Sans-io everything.** Every line of code that can be
   runtime-agnostic *is* runtime-agnostic. Wire-format bugs are
   reproducible by hand in a unit test, not by booking a UDP
   port. (See [ADR 0001](https://github.com/swiftraccoon/dstar-gateway/blob/main/dstar-gateway/adr/0001-sans-io.md).)
2. **Typestate the happy path.** Invalid state transitions are
   compile errors, not runtime checks. You cannot call
   `send_voice` on a `Session<DExtra, Configured>` — the type
   system rejects the call before the test suite runs. (See
   [ADR 0002](https://github.com/swiftraccoon/dstar-gateway/blob/main/dstar-gateway/adr/0002-typestate.md).)
3. **Lenient bytes, strict diagnostics.** The codec parses every
   real-world reflector's quirks without failing, but every
   non-canonical byte becomes a structured diagnostic through the
   `DiagnosticSink` trait. Strict mode is still available via
   `StrictnessFilter`. (See [ADR 0004](https://github.com/swiftraccoon/dstar-gateway/blob/main/dstar-gateway/adr/0004-lenient-validator.md).)

## Where to start

- **I want to connect to a reflector right now.** Jump to
  [Hello, XLX307 (DExtra)](getting-started/hello-dextra.md) —
  DExtra is the simplest of the three protocols (no TCP auth, one
  round-trip handshake) and the code snippet compiles as-is.
- **I want to understand the typestate API.** Read
  [Why typestate?](typestate/why.md) and
  [The Session<P, S> shape](typestate/session.md).
- **I want to run a reflector.** Jump to
  [The Reflector type](server/reflector.md).
- **I want to know what version of ircDDBGateway we copied from.**
  See [REFERENCES.md](https://github.com/swiftraccoon/dstar-gateway/blob/main/dstar-gateway/REFERENCES.md)
  for the pinned commit hashes and the file/line-range table.
