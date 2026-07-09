# Introduction

`dstar-gateway` is an async Rust library for speaking the three
D-STAR reflector protocols — **DPlus** (REF reflectors, port
20001), **DExtra** (XRF/XLX reflectors, port 30001), and **DCS**
(DCS reflectors, port 30051). It provides a typed client API, a
multi-client reflector server, and a runtime-agnostic sans-io core
suitable for embedding in test harnesses, alternate async runtimes,
or WebAssembly modules.

This book is the narrative companion to the [rustdoc API
reference](https://swiftraccoon.github.io/kenwood/dstar_gateway/). It exists to answer the
"why" questions that rustdoc can't: *why* does the library split
into three crates, *why* is the client session a typestate, *why*
is parsing lenient by default, *why* does the server use a runtime
state enum when the client uses compile-time states. If you want
to embed the library in your own project and never touch its
internals, the [Getting Started](getting-started/what-is-dstar.md)
chapters are enough. If you want to contribute or to adapt the
design for a different protocol family, the type-system and sans-io
sections of
[ARCHITECTURE.md](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/ARCHITECTURE.md)
are the places to go.

## The three crates

`dstar-gateway` ships as three crates:

- **`dstar-gateway-core`** — the sans-io core. Contains the wire
  format codecs for all three protocols, the typestate
  `Session<P, S>` state machines, the slow-data sub-codec, the
  DPRS parser, and the server session implementation. Has no
  dependency on tokio, no dependency on the filesystem, and no
  dependency on an async runtime. You drive it by calling pure
  functions that take byte slices and return byte slices plus
  events.
- **`dstar-gateway`** — the tokio shell. Wraps `dstar-gateway-core`
  in a `tokio::net::UdpSocket`-backed driver loop, spawns it as a
  task, and exposes an `AsyncSession<P>` handle with the familiar
  `send_header` / `send_voice` / `send_eot` / `disconnect` /
  `next_event` methods. Also contains the DPlus TCP `AuthClient`,
  the optional `blocking` feature (a caller-driven synchronous
  shell for CLI scripts), and the optional `hosts-fetcher`
  feature (pulls `reqwest` and fetches the XLX reflector
  directory over HTTP).
- **`dstar-gateway-server`** — the reflector server. Provides the
  `Reflector` type, the `ClientPool<P>`, the `fan_out_voice`
  function, the `ClientAuthorizer` trait, and an optional
  cross-protocol forwarding bus. Both the client and server sides
  support all three protocols — `DExtra`, `DPlus`, and `DCS`. The
  server enables all three by default, spawning one
  `ProtocolEndpoint<P>` task per enabled protocol.

See [ARCHITECTURE.md](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/ARCHITECTURE.md)
for a denser 5000-foot overview of the same ground.

## Design philosophy

Three ideas drive every design decision in the library:

1. **Sans-io everything.** Every line of code that can be
   runtime-agnostic *is* runtime-agnostic. Wire-format bugs are
   reproducible by hand in a unit test, not by booking a UDP
   port. (See [ADR 0001](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/adr/0001-sans-io.md).)
2. **Typestate the happy path.** Invalid state transitions are
   compile errors, not runtime checks. You cannot call
   `send_voice` on a `Session<DExtra, Configured>` — the type
   system rejects the call before the test suite runs. (See
   [ADR 0002](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/adr/0002-typestate.md).)
3. **Lenient bytes, strict diagnostics.** The codec parses every
   real-world reflector's quirks without failing, but every
   non-canonical byte becomes a structured diagnostic through the
   `DiagnosticSink` trait. Strict rejection is a caller-side
   policy: there is no shipped strict-mode wrapper type — a
   consumer who wants it supplies a `DiagnosticSink` whose
   `record` flags (or short-circuits on) any diagnostic. (See
   [ADR 0004](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/adr/0004-lenient-validator.md).)

## Where to start

> **Status.** This book currently covers only the introduction and
> Getting Started. The type system, sans-io core, wire formats,
> errors, server, testing, and cookbook chapters are not written yet.
> Until they land, use
> [ARCHITECTURE.md](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/ARCHITECTURE.md),
> the [rustdoc API reference](https://swiftraccoon.github.io/kenwood/dstar_gateway/),
> and the runnable programs in
> [`examples/`](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway/examples).

- **I want to exercise a client against a local reflector.** Jump to
  [Hello, local DPlus](getting-started/hello-dplus.md) — the one
  walkthrough with a complete, working code listing.
- **I want to understand the typestate API.** Read §2.2 of
  [ARCHITECTURE.md](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/ARCHITECTURE.md)
  and [ADR 0002](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/adr/0002-typestate.md).
- **I want to run a reflector.** See the `dstar-gateway-server`
  crate and its `polaris` binary.
- **I want to know what version of ircDDBGateway we copied from.**
  See [REFERENCES.md](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/REFERENCES.md)
  for the pinned commit hashes and the file/line-range table.
