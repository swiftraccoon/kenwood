# Changelog

All notable changes to `dstar-gateway-core`, `dstar-gateway`, and
`dstar-gateway-server` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project aims to adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once it reaches 1.0. Pre-1.0 releases may include breaking changes
in any version bump.

**Status: alpha.** Not yet published to crates.io. The entire
dstar-gateway rewrite is tracked under `[Unreleased]` until the
library has been stress-tested against real reflectors and the
open deferred work (see [ARCHITECTURE.md](ARCHITECTURE.md)) is
either closed or explicitly scoped out of 0.1.0.

## [Unreleased]

### `dstar-gateway-core`

#### Added
- **Workspace foundation**: three-crate workspace, shared `types`
  module with `Callsign`, `Module`, `ReflectorCallsign`,
  `StreamId`, `Suffix`, `BandLetter`, `ProtocolKind`, and the
  `DStarHeader` struct with CRC-CCITT validation.
- **DPlus codec**: full wire-format encode/decode for the DPlus
  (REF port 20001) protocol. Covers LINK1, LINK1_ACK, LINK2,
  OKRW, NAK, keepalive, voice header, voice data, EOT, and the
  TCP auth request/response parsers. Golden fixtures pinned
  against `ircDDBGateway` and `xlxd` captures.
- **DExtra codec**: full wire-format encode/decode for the
  DExtra (XRF/XLX port 30001) protocol. LINK / UNLINK / ACK /
  NAK, keepalive, voice header, voice data, EOT.
- **DCS codec**: full wire-format encode/decode for the DCS
  (port 30051) protocol including the 519-byte LINK packet and
  the 100-byte voice frame layout.
- **Typestate client session**: sans-io `Session<P, S>` with
  compile-time-enforced state transitions
  (`Configured → Authenticated → Connecting → Connected →
  Disconnecting → Disconnected`) and a `Failed<S, E>` recovery
  state. Includes a `Driver` trait that every state implements
  for uniform `poll_transmit` / `handle_input` / `pop_event`
  access.
- **Sans-io Driver + timer wheel**: time is injected via
  `Instant` arguments, not read from the system clock. Enables
  deterministic property tests.
- **Slow-data sub-codec**: encoder and decoder for the slow-data
  blocks embedded in voice frames. Covers the scrambler and the
  `SlowDataAssembler` finite state machine.
- **DPRS position reports**: full DPRS `$$CRC` parser with the
  `calcCRC` algorithm from `ircDDBGateway`.
- **Server typestate**: `ServerSessionCore<P>` + per-client
  state machines mirroring the client side. Currently
  DExtra-only at the endpoint-shell level; DPlus and DCS server
  handshakes are implemented at the core layer and the
  `ProtocolEndpoint<P>` shell dispatches all three.
- **State-gated typestate methods**: `handle_voice_data`
  restricted to `ServerSession<P, Streaming>`, `handle_link2`
  to `ServerSession<DPlus, Link1Received>`, `handle_unlink` to
  `ServerSession<P, Linked>`. Compile-fail tests enforce each.
- **Lenient parsing + `DiagnosticSink`**: every codec function
  emits structured `Diagnostic`s for recoverable wire-format
  quirks instead of failing, with a `StrictnessFilter` wrapper
  available for opt-in strict mode. Unknown-length datagrams in
  the client session dispatcher are swallowed as diagnostics so
  reflector-emitted quirks never kill an established session.
- **Error hierarchy**: layered `Error` / `ProtocolError` /
  per-protocol error enums (`DPlusError`, `DExtraError`,
  `DcsError`), each tied to specific failure modes.

### `dstar-gateway`

#### Added
- **Tokio shell**: `tokio_shell::AsyncSession<P>` wraps a
  `Session<P, Connected>` and a `UdpSocket` in a spawned task
  with command/event channels. `send_header`, `send_voice`,
  `send_eot`, `disconnect`, and `next_event` surface on the
  handle. Every session-loop exit path is instrumented with
  tracing so the tokio task never vanishes silently.
- **DPlus TCP auth**: `auth::AuthClient` performs the 56-byte
  auth packet exchange over TCP and parses the host-list
  response into a `HostList`. Uses a happy-eyeballs-style
  fallback across the multiple round-robin A records of
  `auth.dstargateway.org` with a short per-address timeout so
  dead addresses don't burn the overall connect budget.
- **`blocking` feature**: non-tokio blocking shell variant for
  CLI scripts and test fixtures.
- **`hosts-fetcher` feature**: pulls `reqwest`; downloads the
  Pi-Star `DPlus_Hosts.txt`, `DExtra_Hosts.txt`, and
  `DCS_Hosts.txt` files over HTTPS.
- **Loopback integration tests**: full tokio shell +
  `FakeReflector` UDP harness exercising connect → voice → EOT
  → disconnect round trips for all three protocols, plus a
  `dplus_auth` TCP harness for the auth flow.
- **Conformance pcap replay**: `tests/conformance.rs` uses
  `pcap-parser` to strip Ethernet/IPv4/UDP headers from
  captured traffic and feeds UDP payloads through every
  codec. The corpus itself lives outside the tree; the runner
  is shipped and no-ops on an empty corpus.
- **Hardware-in-the-loop tests**: `tests/hardware.rs` contains
  real connect/listen/disconnect bodies for all three protocols
  plus a voice-burst TX test, triple-gated behind the
  `hardware-tests` feature, `#[ignore]`, and an opt-in
  `DSTAR_TEST_TX_OK=1` env var so nobody accidentally keys the
  air on a real-world reflector.

### `dstar-gateway-server`

#### Added
- **`Reflector` top-level type**: owns the per-protocol
  endpoints, spawns one tokio task per enabled protocol, and
  manages shutdown via a `tokio::sync::watch` channel.
- **`ProtocolEndpoint<P>` run loop**: binds a pre-configured
  `UdpSocket` and drives `ServerSessionCore<P>` for every
  inbound peer. All three protocol handshakes
  (DPlus, DExtra, DCS) dispatch through dedicated
  per-protocol `handle_inbound_*` helpers.
- **`ReflectorConfig` + typed builder**: bind address, enable
  flags per protocol, max clients, keepalive cadence, TX
  rate-limit frames per second, cross-protocol forwarding
  flag. Missing/Provided marker typestate ensures required
  fields are supplied at compile time.
- **`ClientAuthorizer` trait** consulted on every inbound LINK
  attempt, with `AllowAllAuthorizer` and `DenyAllAuthorizer`
  as the default implementations. Rejected attempts produce
  a protocol-appropriate NAK plus a `ServerEvent::ClientRejected`
  event.
- **`AccessPolicy::ReadOnly` enforcement**: voice from read-only
  clients is dropped (not relayed, not transitioned to
  `Streaming`) and emits a `VoiceFromReadOnlyDropped` event.
- **`ClientPool<P>`**: thread-safe map from peer `SocketAddr`
  to `ClientHandle<P>` with a reverse index from `Module` to
  the set of peers linked on that module.
- **`StreamCache` header retransmit**: the endpoint maintains a
  per-module stream cache, records each voice frame, and
  re-fans-out the cached voice header every 21 frames to
  match xlxd's cadence.
- **`fan_out_voice`** engine: broadcasts an inbound voice frame
  to every peer on the same module except the sender. Per-peer
  token-bucket rate limiting gates outbound frames so one slow
  client can't stall fan-out for everyone.
- **Unhealthy-client eviction**: `ClientPool::mark_unhealthy`
  tracks consecutive send failures and triggers eviction after
  `DEFAULT_UNHEALTHY_THRESHOLD` strikes, emitting a
  `ServerEvent::ClientEvicted` event.
- **Cross-protocol forwarding** (optional, off by default):
  `transcode_voice` re-encodes voice events across protocol
  boundaries. `Reflector` allocates a `tokio::sync::broadcast`
  channel for cross-protocol events when the config flag is
  set, and each endpoint subscribes, transcodes, and fans out
  events from other protocols.

### Test infrastructure

#### Added
- **Ten `cargo-fuzz` targets** covering every parser entry
  point across DPlus, DExtra, DCS, slow-data, DPRS, and auth.
  Baseline: ~1.1 billion executions across all targets, zero
  crashes.
- **CI fuzz workflow**: a GitHub Action runs every fuzz target
  for a capped wall-clock budget per PR touching the core
  crate.
- **Server-side compile-fail tests** for the typestate contract
  (invalid `handle_voice_data` / `handle_link2` / `handle_unlink`
  calls are rejected at compile time).
- **`pcap-parser` dev-dep** for the conformance replay runner.

### Internal migration notes

The previous-generation hand-rolled `dstar-gateway` crate
(still 0.1.0, never published) has been fully replaced in-place
by this rewrite. `thd75-repl` was migrated to the new typestate
API in the same rewrite pass. There is no external 0.x to
migrate from; a `MIGRATING.md` document will be written when an
actual 0.x is cut on crates.io.
