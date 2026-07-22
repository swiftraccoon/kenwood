# Hello, local DPlus

This walkthrough connects to a DPlus reflector on loopback,
listens for 10 seconds of inbound traffic, and requests a clean
disconnect. It does not contact REF030. DPlus is the most involved
of the three protocols
because it requires a TCP authentication step before the UDP
session can start.

## The complete code

```rust,ignore
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    Authenticated, ClientStateKind, Configured, Connecting, DPlus, Session,
};
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;
use tokio::time::timeout;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
// 1. Bind a local UDP socket.
let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

// 2. Build a Configured session.
let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
    .callsign(Callsign::try_from_str("W1AW")?)
    .local_module(Module::try_from_char('B')?)
    .reflector_module(Module::try_from_char('C')?)
    .peer("127.0.0.1:20001".parse()?)
    .build();

// 3. DPlus requires an auth step. In production you'd fetch the host
// list via `AuthClient::authenticate`; this example attaches an empty
// list as a placeholder, which is what the loopback tests do.
let authenticated: Session<DPlus, Authenticated> =
    session.authenticate(HostList::new())?;

// 4. Drive the 2-step DPlus handshake manually.
let now = Instant::now();
let mut connecting: Session<DPlus, Connecting> = authenticated.connect(now)?;
for _ in 0..2 {
    let tx = connecting
        .poll_transmit(Instant::now())
        .ok_or("DPlus LINK not ready")?;
    sock.send_to(tx.payload, tx.dst).await?;

    let mut buf = [0u8; 64];
    let (n, peer) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await??;
    connecting.handle_input(
        Instant::now(),
        peer,
        buf.get(..n).ok_or("receive length exceeded buffer")?,
    )?;
    if connecting.state_kind() == ClientStateKind::Connected {
        break;
    }
}
let connected = connecting.promote()?;

// 5. Hand off to the tokio shell and listen for 10 seconds.
let mut session = AsyncSession::spawn(connected, Arc::clone(&sock));
let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
loop {
    tokio::select! {
        _ = tokio::time::sleep_until(deadline) => break,
        ev = session.next_event() => {
            match ev {
                Some(e) => println!("event: {e:?}"),
                None => break,
            }
        }
    }
}

// 6. Graceful disconnect.
session.disconnect().await?;
# Ok(()) }
```

## What's happening

1. **Bind** a local UDP socket. Port 0 lets the OS pick; the
   reflector will send replies to whatever ephemeral port you
   got.
2. **Build** a `Session<DPlus, Configured>`: that's a session
   whose type parameters are `DPlus` (protocol) and `Configured`
   (state). Configured is the starting state; calling `.connect()`
   on it produces a `Session<DPlus, Connecting>`.
3. **Attach a host list.** `Session<DPlus, Authenticated>` is the
   "has-a-host-list" state. In production you get this list by
   running the real auth flow via
   `dstar_gateway::auth::AuthClient::authenticate`. In a test you
   can attach an empty list.
4. **Drive the handshake.** DPlus is a two-round-trip handshake:
   LINK1 out → LINK1\_ACK in → LINK2 out → OKRW in. We poll the
   outbox and feed it into the socket, wait for a reply, and
   feed that back into the session. The session core detects
   the ACK sequence and transitions the state machine forward.
5. **Promote** `Connecting` to `Connected`. This is infallible
   once `state_kind()` reports `Connected`.
6. **Hand off** to the tokio shell. `AsyncSession::spawn` spawns
   a dedicated task that owns the socket and the session; your
   code interacts with it via `next_event` and `send_*`.
7. **Listen** for 10 seconds of inbound events. Each event is an
   `Event<DPlus>`: `Connected`, `VoiceStart`, `VoiceFrame`,
   `VoiceEnd`, `Disconnected`, `PollEcho`.
8. **Request a graceful disconnect.** `session.disconnect().await?`
   returns once the session loop has queued UNLINK. Continue reading
   events if you need to observe `Event::Disconnected` and its reason.

## Running it

The code above binds to `0.0.0.0:0` and attempts to connect to
`127.0.0.1:20001`, which is a local loopback address; no RF
energy leaves your machine. To connect to a real reflector, swap
the peer address for a real REF reflector (e.g. look up REF030
in the Pi-Star DPlus hosts file) and run the real auth step via
`AuthClient`. Use your own valid callsign and follow the service's
access rules. This listing receives events but sends no voice stream.

DCS and DExtra use simpler one-round-trip handshakes with no TCP
auth step. See the runnable
[`02_connect_dcs`](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/examples/02_connect_dcs.rs)
and
[`03_connect_dextra`](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/examples/03_connect_dextra.rs)
examples.
