# dstar-gateway

Tokio and synchronous drivers for the sans-io
[`dstar-gateway-core`](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway-core)
client. The client supports `DPlus`, `DExtra` and DCS reflector sessions,
TCP authentication and an optional untrusted HTTP directory client.
[`dstar-gateway-server`](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway-server)
provides the separate multi-client reflector.

These unpublished APIs may change. Wire comparisons use the pinned public
sources in
[REFERENCES](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/REFERENCES.md);
model crates specify radio firmware/interface behavior separately.

## Connect, receive and disconnect

This network example expects a local `DExtra` reflector:

```rust,no_run
use std::sync::Arc;
use std::time::{Duration, Instant};
use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::{DExtra, Module, RoutingCallsign, Session};
use tokio::net::UdpSocket;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
let connecting = Session::<DExtra, _>::builder()
    .callsign(RoutingCallsign::new("W1AW")?)
    .local_module(Module::B)
    .reflector_module(Module::C)
    .peer("127.0.0.1:30001".parse()?)
    .build()
    .connect(Instant::now())?;
let connected = drive_connecting(connecting, &socket, Duration::from_secs(5)).await?;
let mut client = AsyncSession::spawn(connected, socket);
let stop = tokio::time::sleep(Duration::from_secs(10));
tokio::pin!(stop);
loop {
    tokio::select! {
        () = &mut stop => break,
        event = client.next_event() => match event {
            Some(event) => println!("{event:?}"),
            None => break,
        }
    }
}
client.disconnect().await?;
# Ok(()) }
```

`drive_connecting` bounds UDP sends, receives and timer work by one total
deadline; an unrepresentable duration fails before sending. Cancellation drops
the consumed session; already sent datagrams may have reached the peer.
The caller retains the socket. DNS and authentication precede that handshake
and have their own configured limits.

For `DPlus`, call `AuthClient::authenticate`, then attach the resulting host
list through the configured session's `authenticate` method before linking.
For DCS, additionally supply
`.dcs_reflector(DcsReflector::new("DCS030")?)`; its target identity is required.
The checked
[core workflow](https://swiftraccoon.github.io/kenwood/dstar_gateway_core/)
describes headers, frame indices and protocol state failures.

## Completion and queue ownership

`send_header`, `send_voice` and `send_eot` succeed after every currently
ready datagram has been handed to the local UDP socket. This is local send
completion, not remote acknowledgement. Each UDP send has a two-second local
deadline. Cancelling before enqueue sends no command; after enqueue, the
task may still transmit. Errors retain the underlying typed cause.

`next_event` returns None after the task exits and its queued events drain.
`join` drains remaining events and returns the task result once. Cancelling
a join retains its task handle for another wait. Repeated calls after the
result was consumed return success.

`disconnect` sends UNLINK, consumes queued events and awaits the core's
terminal outcome and task completion. The peer has two seconds within a
five-second shell deadline. No acknowledgement returns
`ShellError::DisconnectUnacknowledged`; channel/task/I/O failures remain typed.
`disconnect_reason` retains an observed terminal reason. Dropping a handle
closes its channels and requests local task exit; explicit disconnect/join is
the completion boundary.

Commands use a 32-entry FIFO and events a 256-entry FIFO. Event capacity
represents roughly five seconds at 50 voice frames/s, shared with control
events. Commands and their socket writes continue while the event queue is
full. Receive and protocol timers pause until callers consume events.
`AnyAsyncSession`/`AnyEvent` expose the same operations without a protocol
type parameter.

## Authentication and directory limits

`AuthClient::new` and Default share ten seconds for DNS plus all TCP
connection attempts, ten seconds for writing, five seconds total response
time, a five-second idle limit, and at most two MiB retained response data.
The address attempts are sequential, each capped at three seconds inside the
shared connect budget. Builder methods configure these local resource limits.
Continuous partial data never renews the total response deadline. Cancelling
closes its TCP stream; an OS DNS lookup already started by Tokio may finish
after cancellation.

The `insecure-plaintext-xlx-directory` feature exposes
`InsecurePlaintextXlxDirectoryFetcher`. Its chosen endpoint uses plaintext
HTTP; returned addresses have no authenticated provenance. Its fallible
constructor disables redirects, defaults to ten seconds and two MiB, and
accepts a caller-configured HTTP client and limits. Invalid recognized host
rows reject the response with typed line errors.

## Synchronous use and examples

The `blocking` feature provides
`blocking_shell::drive_connecting` and `BlockingSession` without a running
Tokio runtime. Construction configures the owned socket for blocking I/O.
Send, event polling, disconnect and `into_parts` cover its complete lifecycle.
None from `run_until_event` means idle or closed; inspect `is_closed` and
keep polling while linked. Receive waits total at most 100 ms per call,
subject to OS scheduling/resolution; sends have separate two-second timeouts.

The
[examples](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway/examples)
provide protocol-specific connection, TX, diagnostics, bounded DPRS
reconstruction, reconnection, controlled bridging, synchronous driving and
raw capture recipes. External-service examples require `examples-network`;
local loopback examples compile by default. `hardware-tests` compiles ignored
live-reflector tests. Ordinary tests use loopback fixtures; external
reflector/RF cases remain gated.

See the
[book](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway/book),
[architecture](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/ARCHITECTURE.md),
[changelog](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/CHANGELOG.md)
and
[contributor workflow](https://github.com/swiftraccoon/kenwood/blob/main/CONTRIBUTING.md).

## License

GPL-2.0-or-later. Upstream source-header license declarations are recorded
separately in REFERENCES.
