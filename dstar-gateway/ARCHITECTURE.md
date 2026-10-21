# D-STAR stack architecture

The stack separates wire/state logic from network I/O. Protocol comparisons
use the public revisions in [REFERENCES](REFERENCES.md); behavior changes are
recorded in [CHANGELOG](CHANGELOG.md).

| Crate | Responsibility |
| --- | --- |
| dstar-gateway-core | Directional codecs, checked transmit values, lossless receive fields, client/server state machines, slow data and DPRS. Uses std, opens no socket and reads no clock. |
| dstar-gateway | Tokio and synchronous client drivers, TCP authentication and optional HTTP directory retrieval. Owns network I/O and obtains time. |
| dstar-gateway-server | Protocol endpoints, authorization, client pools, stream caches, recipient budgets and reflector task coordination. |

## Core state and wire boundaries

`Session<P, S>` constrains client operations through sealed protocol and
state markers. Consuming transitions return `Failed<S, E>` with the session
and typed error. Driver input, timeout and in-place disconnect can change
runtime state later; every send checks that runtime state.

`SessionCore` accepts named `SessionConfig` and `ClientProtocol` values.
DCS configuration contains a required target identity. Transmit indices use
`FrameSequence`; routing prefixes reserve the eighth callsign byte for a
module. `RepeaterIdentity` carries the local prefix and module in control
packets. Receive arrays, header flags and sequence bytes retain unknown values.

`ServerSessionCore` checks per-peer transitions at runtime. It retains linked
identity/module, admits an active stream and rejects unrelated stream endings.
Client and server share the directional codecs rather than a server typestate
facade.

Ready transmits and events are caller-drained queues. Equal transmit release
times preserve FIFO. Public time-bearing mutations reject unrepresentable
timer spans before changing state; timeout at the clock limit closes once.
The core's unbounded queues require the embedding driver to keep draining.

## Client drivers and completion

`drive_connecting` owns the asynchronous handshake pump. One absolute
deadline bounds sends, receives and timer work. Timers are processed before
receive, including under continuous input. Cancellation drops the consumed
session while the caller retains the socket.

`AsyncSession` retains the spawned task handle, command/event channels and
a peer-activity watch receiver. A send reply follows core acceptance and
flushing every ready datagram to the local socket. Each UDP send has a
two-second local budget. Cancellation after enqueue can still transmit;
local completion does not establish peer receipt.

Commands have 32 FIFO slots; events have 256. A full event queue still permits
outbound commands and UDP sends. Input and protocol timers wait for event
capacity, so callers must keep consuming events. Foreign source datagrams do
not update peer activity.

Normal terminal state exits the task. `join` drains pending events and
returns the task result once; a cancelled wait retains the task for another
call. `disconnect` waits for UNLINK acknowledgement or its protocol timeout,
then joins completion. Dropping a handle closes its channels without awaiting
graceful unlink.

The `blocking` feature has a separate synchronous I/O pump and the same
checked core. Its socket is configured for blocking mode; per-call receive
waits total at most 100 ms, with separate two-second send timeouts.
Polling returns queued events before reading. None means idle or closed;
`is_closed` distinguishes them. Explicit TX, disconnect and `into_parts`
cover the caller-driven resource lifecycle.

## Authentication, directory and errors

`AuthClient` owns one transient TCP stream. DNS and all sequential address
attempts share the configured connect deadline; writing has an independent
budget. Reading has a fixed total deadline, an idle deadline and a retained
byte limit. Builder settings and Default/new share one documented policy.
Cancellation closes the stream; resolver work already started by the OS may
finish afterward.

Optional directory retrieval uses a configured client, total deadline and
retained body limit, then checked UTF-8/host parsing. The selected plaintext
HTTP endpoint provides no authenticated address provenance.

Core errors retain protocol/state/type causes and include a tagged
`std::io::Error` variant for I/O drivers; they contain no Tokio types.
Shell errors retain core causes, task join failures, local send deadlines and
terminal disconnect outcomes. A shared typed cause allows the failed command
and task completion to report the same UDP failure. Authentication and directory
errors retain their underlying I/O/parser/HTTP causes with explicit resource
and deadline variants.

## Diagnostics and validation

Decoders distinguish fatal framing errors from the recoverable conditions
they diagnose. `DiagnosticSink` receives those implemented observations;
raw receive bytes and checked text views remain available to callers.
Recording a diagnostic does not itself reject a parsed packet. A caller can
collect observations and reject the returned value according to its policy.

Validation includes unit tests, independently specified golden bytes,
encoder/decoder properties, compile-fail state checks, loopback I/O tests,
doctests and mutation proofs of load-bearing guards. Ten standalone fuzz
targets cover decode/parser boundaries; selected targets additionally assert
round trips or scrambling invariants. Stable compilation and nightly fuzz
execution are separate checks.

Live-reflector tests require their feature and ignored-test selection. Voice
TX additionally checks `DSTAR_TEST_TX_OK=1`. Protocol and loopback checks
establish no radio hardware behavior.

The [book](book/) introduces client use, and the
[ADRs](adr/) record architectural decisions. Full-tree checks follow the
workspace [contributor workflow](../CONTRIBUTING.md).
