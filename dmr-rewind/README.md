# dmr-rewind

`dmr-rewind` is a receive-only Tokio client for BrandMeister's REWIND
**Open DMR Terminal** service. It authenticates a seven-digit DMR ID,
subscribes to group and private destinations, maintains the UDP session,
and yields typed DMR media and control events.

The crate deliberately does not transmit DMR, decode AMBE+2, choose a
BrandMeister master, resolve DNS, or reconnect. Those policies belong to
the application supervising the client. [`pulsar`](../pulsar/) is the
recorder built on this layer.

## Access and credentials

Open DMR Terminal normally listens on UDP port **54006**. Its login is
self-service:

1. Register the seven-digit DMR ID that this software will use.
2. In BrandMeister SelfCare, set a **Hotspot Security** password for that
   DMR ID.
3. Select a BrandMeister master that exposes Open DMR Terminal and use its
   address with port 54006.

The password is the Hotspot Security password, not the BrandMeister
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

use dmr_rewind::{Client, ClientConfig, Event, Subscription};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let server: SocketAddr = "203.0.113.10:54006".parse()?;
let password = std::env::var("BM_HOTSPOT_PASSWORD")?;
let config = ClientConfig::new(server, 1_234_567, password)
    .with_subscriptions(vec![
        Subscription::group(91),
        Subscription::private(1_234_567),
    ])
    .with_timeout(Duration::from_secs(15));

let mut client = Client::connect(config).await?;
loop {
    match client.next_event().await? {
        Event::Audio(packet) => {
            // `packet.data` contains the exact 27 AMBE+2 bytes. Metadata
            // retains flags, sequence, receipt time, and the original UDP
            // datagram.
            println!("audio sequence {}", packet.metadata.sequence);
        }
        Event::Close(_) => break,
        _ => {}
    }
}
# Ok(())
# }
```

`ClientConfig` defaults to a five-second keepalive interval and a
15-second valid-packet timeout. The timeout must be longer than the
keepalive interval. Each subscription is installed sequentially and
acknowledged before the next is sent.

Call `Client::close` during an orderly shutdown. If the master sends a
new challenge on an established session, the client authenticates again
and reinstalls its subscriptions. A timeout or socket failure is returned
to the caller so its supervisor can resolve the host again, choose another
address, and reconnect with an appropriate backoff.

## Event fidelity

The event API retains:

- the REWIND transport sequence and all flag bits;
- exact 12-byte voice headers and either empty or exact 12-byte terminators;
- exact 27-byte packed AMBE+2 audio and 10-byte embedded-link-control
  payloads;
- optional 32-byte superheader metadata;
- reports, failures, busy notices, graceful close, and unknown extensions;
- the complete original datagram and local receipt timestamp.

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

Wire encoding lives in [`dmr-rewind-core`](../dmr-rewind-core/). Both
crates are GPL-2.0-or-later.
