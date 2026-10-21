# Hello, local DPlus

This example connects to a local DPlus reflector, receives for ten seconds,
then awaits unlink and task completion. Its empty host list is a loopback
fixture. A remote DPlus session first obtains a real list with AuthClient.

The listing below includes the maintained Cargo example directly. The
workspace example compilation checks this same source:

```rust,no_run
{{#include ../../../examples/01_connect_dplus.rs}}
```

The configured session requires a checked station prefix, local module,
reflector module and peer address. Attaching a host list enters Authenticated;
connect creates the Connecting session and its first outbound packet.
The shared drive_connecting pump exchanges the two handshake rounds and
promotes only when the runtime state is Connected.

AsyncSession::spawn then starts the event loop. next_event is cancellation
safe; unconsumed events remain queued. If its channel closes, join returns the
task's typed result. disconnect consumes queued events, waits for the terminal
acknowledgement or protocol timeout, and joins the task. A missing
acknowledgement is DisconnectUnacknowledged, not a successful remote unlink.

Run the loopback example with:

```bash
cargo run -p dstar-gateway --example 01_connect_dplus
```

No voice is sent by this example. A reflector must be listening on
127.0.0.1:20001. The
[DCS](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/examples/02_connect_dcs.rs)
and
[DExtra](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/examples/03_connect_dextra.rs)
examples use one-round handshakes; DCS additionally requires the target
identity carried in its control packets.
