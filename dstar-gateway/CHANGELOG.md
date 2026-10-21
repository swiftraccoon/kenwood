# Changelog

Changes to dstar-gateway-core, dstar-gateway and dstar-gateway-server.
The packages are experimental and unpublished; unreleased APIs can change
without compatibility adapters.

## Unreleased

### Core wire and state contracts

- Client typestate and runtime drivers share directional DPlus, DExtra and DCS
  codecs, typed errors and supplied diagnostic sinks. Received bytes remain
  lossless; checked text views and transmit construction are separate.
- FrameSequence restricts transmit indices to 0..=20. RoutingCallsign reserves
  the eighth byte for a module; RelayRouting groups route inputs.
  HeaderFlags preserves wire bytes and distinguishes independent flags from
  the exclusive repeater-control code.
- DCS construction requires DcsReflector; the builder no longer substitutes
  DCS001. Runtime SessionCore construction takes SessionConfig/ClientProtocol.
- Module-bearing control codecs take RepeaterIdentity. ACK/NAK retain the
  client module; ModuleSlot represents a selected or empty target.
  Reply identity/modules/phase are correlated, and late link replies cannot
  complete unlink. Client voice/poll input applies only while connected.
- Equal-deadline transmits preserve FIFO; client/server protocol-marker
  mismatches leave events queued. Public time-bearing transitions reject
  unrepresentable timer spans before mutation.
- ServerSessionCore checks linked identity/module, active streams and terminal
  input. The unsupported server typestate facade is removed.
- Host parsing uses checked names/addresses/nonzero ports and atomic typed row
  failures. HostFile/HostList support standard collection and iteration traits.
- DprsReport owns parsing and encoding with verified/default CRC, explicit
  diagnostic checksum bypass, validated source/symbol/comment/position and
  coordinate rounding carry. SlowDataTextMessage owns encoding; fixed text
  blocks and checked borrowed views replace allocating compatibility views.
- The README is executable rustdoc; golden/property/state/UI tests and
  independently linted fuzz targets cover their respective contracts.

### Client shell completion and resources

- Shared asynchronous and synchronous handshake drivers enforce one total
  deadline around I/O and timer work; unrepresentable deadlines fail before
  sending. Session drivers retain source-address checks.
- Async send replies follow flushing ready datagrams to the local UDP socket.
  Two-second send deadlines and shared typed failures expose local send
  completion. Cancellation after enqueue can still transmit.
- AsyncSession/AnyAsyncSession retain task completion through join.
  Cancellation retains the task handle; the first join consumes its result.
  Disconnect observes the terminal unlink outcome and joins completion.
- The 32-entry command and 256-entry event FIFOs preserve ordering.
  Commands continue under receive backpressure; callers keep consuming events
  for input/timers to progress.
- AuthClient Default/new share bounded DNS/connect, write, response and idle
  windows plus a retained-byte limit. Sequential address attempts fit inside
  one connect deadline; partial reads do not renew total response time.
- Optional plaintext directory retrieval has a fallible constructor, configured
  client, total deadline, body limit and typed UTF-8/host-row errors.
- The synchronous shell exposes TX, bounded idle polling, closure status,
  disconnect and resource extraction. Its examples use the shared handshake.
- Examples reconstruct complete bounded DPRS lines, retain capture write errors
  and use the checked core domains. The book includes maintained example source.

### Reflector server

- Private checked configuration requires a routing identity, nonempty modules
  and protocols, finite recipient rates and representable timing windows.
  Named pre-bound sockets replace interchangeable positional socket arguments.
- Endpoint markers determine protocols. Client creation, protocol transitions
  and module indexes update atomically; public observations are owned snapshots
  and selected voice routes rather than mutable pool handles.
- One active protocol/peer/stream identity governs raw and cross-protocol
  forwarding. Both paths charge the same recipient budget and bound each UDP
  enqueue by two seconds. Successful sends reset consecutive-failure accounting.
- DCS header conversion caches context without synthesizing a voice packet.
  Combined DCS voice packets are forwarded once, including across superframes;
  cross-protocol end packets retain the final sequence and set the end bit.
- Run consumes the reflector, binds sockets before spawning, and joins every
  endpoint after shutdown or a task failure. Typed failures retain all task
  outcomes; custom endpoints reject concurrent UDP drivers. POLARIS signals
  shutdown and awaits completion on Ctrl-C.
- Published README examples, loopback forwarding checks, concurrency and
  cancellation tests, and targeted guard mutations cover these contracts.

### Migration

Workspace consumers use canonical checked core types and the new report/text
methods directly. There are no old-signature shims. Optional directory
construction and synchronous session construction are fallible. Callers
requiring background completion await disconnect/join; channel closure alone
is not a task result.
