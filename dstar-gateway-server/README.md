# dstar-gateway-server

Multi-client D-STAR reflector with `DExtra`, `DPlus`, and `DCS` UDP endpoints.
The crate uses the injected-time `ServerSessionCore` from `dstar-gateway-core`;
this Tokio layer owns sockets, clocks, authorization, client limits, and forwarding.
It is unpublished and experimental. Protocol behavior follows the public upstreams
listed in the [protocol references](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/REFERENCES.md).

## Configuration and shutdown

The builder requires a validated seven-byte routing callsign, a nonempty module
set, and a base socket address. All three protocols are enabled by default.
Disable protocols explicitly for a single-endpoint service. With several enabled
protocols, a fixed base address uses their standard ports unless overridden with
`protocol_bind`; port zero requests a separate ephemeral port for each endpoint.

```rust,no_run
use std::collections::HashSet;
use dstar_gateway_core::types::{Module, ProtocolKind, RoutingCallsign};
use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig};

# async fn serve() -> Result<(), Box<dyn std::error::Error>> {
let config = ReflectorConfig::builder()
    .callsign(RoutingCallsign::new("REF999")?)
    .module_set(HashSet::from([Module::C]))
    .bind("127.0.0.1:30001".parse()?)
    .disable(ProtocolKind::DPlus)
    .disable(ProtocolKind::Dcs)
    .build()?;
let reflector = Reflector::new(config, AllowAllAuthorizer);
let shutdown = reflector.shutdown_handle();
let run = reflector.run();
tokio::pin!(run);
tokio::select! {
    result = &mut run => result?,
    signal = tokio::signal::ctrl_c() => {
        shutdown.shutdown();
        let completion = run.await;
        signal?;
        completion?;
    }
}
# Ok(()) }
```

`AllowAllAuthorizer` accepts every link with read-write access. Supply a
`ClientAuthorizer` for a configured access policy. Its synchronous callback must
perform bounded, nonblocking work. Load external policy before starting service.
An endpoint defaults to 250 clients total and 50 per module; setting either
limit to zero rejects the corresponding new clients. `DExtra` and `DCS` take
the initial module from LINK; `DPlus` uses the lowest configured module.
A linked client must unlink before changing its station identity or module.

`run` consumes the reflector. It binds every socket before spawning endpoint
tasks. A task failure requests shutdown and every task is joined before the
returned `ShellError::Endpoints` exposes their typed failures. A shutdown signal
requests completion; awaiting `run` confirms it. Dropping the future requests
task abort without joining task destruction. Already enqueued UDP datagrams
remain outside the server's control.

## Forwarding and bounds

Each endpoint selects one source per module using protocol, UDP peer, and stream
ID together. Headers from other sources wait until that stream ends or its cache
expires. This selection governs both raw forwarding and cross-protocol input.
The stream cache retains headers in memory. `DExtra`/`DPlus` retransmit a header
after the first 20 data frames, then every 21 frames. `DCS` carries its header
in each voice/end packet and sends no separate header rebroadcast. Stalled caches expire after two
seconds by default; silent clients expire after 30 seconds. Maintenance runs no
faster than every 100 ms, including when the configured poll interval is zero.

Every recipient has one voice-frame budget shared by raw and transcoded
forwarding. The default refill and initial burst are 60 frames per second,
above D-STAR's 50-frame/s cadence. Budget exhaustion drops that recipient's
frame. A UDP enqueue has a two-second deadline; success resets the recipient's
failure streak, and five consecutive failures evict it. Delivery to other
recipients continues after a send failure. Cancellation during a recipient pass
can leave a prefix sent. UDP enqueue success does not confirm remote reception.

Cross-protocol forwarding is disabled by default. Enabling it uses a bounded
256-event broadcast channel. Lag drops old events; a target requires a stream
start before forwarding its frames. `DCS` embeds the cached header in every
packet; converting a separate header to `DCS` updates the cache without emitting
an extra packet. Receive
sequence bytes outside the legal transmit domain remain raw observations and
are omitted from transcoding. Valid final sequence values are preserved with
the target's end bit.

## Custom drivers and observations

`ProtocolEndpoint::<P>::from_config` constructs a checked custom-driver endpoint
whose marker selects the protocol. `handle_inbound` accepts complete datagrams
and an injected `Instant`, and returns `EndpointOutcome` datagrams and server
events. Client records and mutation plumbing are private. Concurrent state
operations are serialized; cancellation can retain completed protocol progress,
while client membership and its index always change together.

The endpoint's `run` method drives one pre-bound socket and rejects a second
concurrent UDP driver with `ShellError::AlreadyRunning`. A caller retaining the
endpoint can resume it after cancelling that driver, accounting for partially
sent datagrams. `Reflector::run` instead owns and discards its endpoints when
finished. Its built-in service emits tracing logs; caller-visible protocol
events are returned by the custom-driver `handle_inbound` API.

## Executables and examples

`cargo run -p dstar-gateway-server --bin polaris` starts a local `DExtra` service.
It defaults to `0.0.0.0:30001`, routing identity `POLARIS`, modules `ABCD`, and
`AllowAllAuthorizer`. `POLARIS_BIND`, `POLARIS_CALLSIGN`, and `POLARIS_MODULES`
override those values. Ctrl-C requests shutdown and waits for the endpoint.

Run an example with `cargo run -p dstar-gateway-server --example <name>`:

- `ref_reflector`: a `DExtra` service on modules A through D.
- `02_authorized_reflector`: a completely parsed banlist before listening.
- `03_listen_only_dongles`: an illustrative read-only policy for a callsign prefix.
- `04_observability`: JSON tracing with a runtime filter.

Examples bind sockets when executed. Scoped tests use loopback peers, protocol
vectors, injected time, and compile-fail fixtures; no radio or external reflector
is needed. Full-tree validation follows the workspace contributor instructions.
