# dmr-rewind

`dmr-rewind` is a receive-only Tokio client for `BrandMeister`'s REWIND
**Open DMR Terminal** service. It authenticates a seven-digit DMR ID,
subscribes to group and private destinations, maintains the UDP session,
and yields typed DMR media and control events.

The crate deliberately does not transmit DMR, decode AMBE+2, choose a
`BrandMeister` master, resolve DNS, or reconnect. Those policies belong to
the application supervising the client. [`pulsar`](https://github.com/swiftraccoon/kenwood/tree/main/pulsar) is the
recorder built on this layer.

## Access and credentials

Open DMR Terminal normally listens on UDP port **54006**. Its login is
self-service:

1. Register the seven-digit DMR ID that this software will use.
2. In `BrandMeister` `SelfCare`, set a **Hotspot Security** password for that
   DMR ID.
3. Select a `BrandMeister` master that exposes Open DMR Terminal and use its
   address with port 54006.

The password is the Hotspot Security password, not the `BrandMeister`
website password, a dashboard API key, or a repeater password. A master
operator may still restrict the service, destinations, connection count,
or recording use under local policy.

REWIND **Simple External Application** on UDP port **54005** is deliberately
unsupported. That service uses administrator-provisioned application
credentials and a different handshake. `dmr-rewind` exposes only the Open DMR
Terminal login and never sends a Simple Application configuration request.

## Example

```rust,no_run
use std::net::SocketAddr;
use std::time::Duration;

use dmr_rewind::{Client, ClientConfig, DestinationId, EventKind, SubscriberId, Subscription};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let server: SocketAddr = "203.0.113.10:54006".parse()?;
let password = std::env::var("BM_HOTSPOT_PASSWORD")?;
let config = ClientConfig::new(server, SubscriberId::new(1_234_567)?, password)?
    .with_subscriptions(vec![
        Subscription::group(DestinationId::new(91)?),
        Subscription::private(DestinationId::new(1_234_567)?),
    ])?
    .with_timeout(Duration::from_secs(15))?;

let mut client = Client::connect(config).await?;
println!("{} -> {}", client.local_addr()?, client.server_addr());
loop {
    match client.next_event().await?.into_kind() {
        EventKind::Audio(packet) => {
            // `packet.data()` contains the exact 27 AMBE+2 bytes. Metadata
            // retains flags, sequence, receipt time, and the original UDP
            // datagram.
            println!("audio sequence {}", packet.metadata().sequence());
        }
        EventKind::Close(_) => break,
        _ => {}
    }
}
client.close().await?;
assert!(client.is_closed());
# Ok(())
# }
```

`ClientConfig` defaults to a five-second keepalive interval and a
15-second valid-packet timeout, and a five-second budget for each control send
and socket setup operation. Configuration construction and every consuming
setter reject invalid settings before returning a usable value. Subscriber IDs
follow the seven-digit service policy, destinations are nonzero 24-bit IDs,
and unknown receive-wire session codes cannot become outbound subscriptions.
The timeout must be longer than the positive keepalive interval. Duplicate
subscriptions, empty credentials/descriptions, oversized descriptions, zero
remote ports and mismatched local/remote IP families are rejected. Each subscription is installed sequentially and
acknowledged before the next is sent.

Call `Client::close` during an orderly shutdown. If the master sends a
new challenge on an established session, the client authenticates again
and reinstalls its subscriptions. A timeout or socket failure is returned
to the caller so its supervisor can resolve the host again, choose another
address, and reconnect with an appropriate backoff.

Use `next_event_for` for periodic application work between receive waits. Its
deadline bounds waiting for a datagram; an already started control send can
extend the call by one configured send budget. Each send finishes or times out
before returning the receive deadline. Dropping during a control send leaves
it unconfirmed; the next operation returns `Error::ControlSendInterrupted` and
the caller must drop that client and reconnect. No detached send continues.
Dropping while waiting to receive retains queued events and recovery state;
a datagram already received can have been consumed.

```rust,no_run
use std::time::Duration;
use dmr_rewind::{Client, EventKind};

async fn poll(client: &mut Client) -> Result<bool, dmr_rewind::Error> {
    match client.next_event_for(Duration::from_millis(250)).await? {
        Some(event) => {
            if matches!(event.kind(), EventKind::Close(_)) { return Ok(false); }
            let metadata = event.metadata();
            println!("sequence {}: {} bytes", metadata.sequence(), metadata.raw_datagram().len());
            Ok(true)
        }
        None => Ok(true), // The application can service other work here.
    }
}
```

## Event fidelity

Events expose immutable metadata and packet-specific records through `kind()`
or consuming `into_kind()`. Full-link-control bytes derive from one parsed
representation; a caller cannot mutate the subtype, bytes, declared length or
parsed fields independently. The event API retains:

- the REWIND transport sequence and all flag bits;
- exact 12-byte voice headers and either empty or exact 12-byte terminators;
- exact 27-byte packed AMBE+2 audio and uninterpreted embedded-data bodies;
- optional 32-byte superheader metadata;
- reports, failures, busy notices, graceful close, and unknown extensions;
- the complete original datagram and local receipt timestamp.

Offline readers can parse captured datagrams through the same validation
boundary, supplying their recorded receipt time. This checks bytes without
authenticating the original sender:

```rust
use std::time::SystemTime;
use dmr_rewind::{Event, EventKind};
use dmr_rewind_core::{DmrAudioSubtype, Packet, PacketFlags, PacketType, Payload};

let packet = Packet::new(
    PacketType::DmrAudio(DmrAudioSubtype::new(6)?), PacketFlags::from_bits(0x8001),
    u32::MAX.into(), Payload::DmrAudio([0xa5; 27]),
)?;
let event = Event::from_datagram(packet.to_bytes(), SystemTime::UNIX_EPOCH)?;
assert_eq!(event.metadata().sequence().get(), u32::MAX);
assert_eq!(event.metadata().received_at(), SystemTime::UNIX_EPOCH);
assert_eq!(event.metadata().raw_datagram(), packet.to_bytes());
let EventKind::Audio(audio) = event.into_kind() else { unreachable!() };
assert_eq!(audio.subtype().get(), 6);
assert_eq!(audio.data(), [0xa5; 27]);
assert!(Event::from_datagram(vec![0; 18], SystemTime::UNIX_EPOCH).is_err());
# Ok::<(), Box<dyn std::error::Error>>(())
```

One Open Terminal connection carries at most one active call. Several
subscriptions share that slot, so overlapping calls can hide one another.
Use additional connections only when the selected master's policy and
capacity permit it.

## Transport and secret handling

REWIND is unencrypted UDP. Authentication sends
`SHA-256(challenge || hotspot-security-password)` instead of the password
itself, but traffic metadata and voice payloads remain visible and do not
have general peer-identity or integrity protection. Use a trusted network
path or a mutually supported VPN where appropriate.

The client connects its UDP socket to the selected peer, which lets the
operating system reject datagrams from other source addresses. Credential
fields have redacted `Debug` implementations; applications must still
avoid logging the original password string or environment variable value.

## Development

From the repository root:

```bash
cargo test -p dmr-rewind
cargo clippy -p dmr-rewind --all-targets -- -D warnings
```

Packets are converted through the core's `Packet::from_bytes` and `to_bytes`;
payload and header inspection use accessors. Full-LC IDs are validated `DmrId`
values. Embedded-data events retain variable byte vectors. The experimental
`ClientEvent` and `ConnectError` compatibility aliases have been removed; use
`Event` and `Error` directly.

`ClientConfig::new` and its optional setters now return `Result`; use validated
`SubscriberId`, `DestinationId` and `SubscriptionKind` values. `Event` protects
its raw/parsed relationship; match `event.kind()` or `event.into_kind()` against
`EventKind`, and use record/metadata accessors. Wire IDs, flags and subtypes use
the core's canonical types. This is an unpublished experimental API migration.

Wire encoding lives in [`dmr-rewind-core`](https://github.com/swiftraccoon/kenwood/tree/main/dmr-rewind-core). Both
crates are GPL-2.0-or-later.
