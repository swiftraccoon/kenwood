# dstar-gateway

[![CI](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/ubuntu.yml?label=CI)](https://github.com/swiftraccoon/kenwood/actions)
[![Docs](https://img.shields.io/badge/docs-github.io-blue)](https://swiftraccoon.github.io/kenwood/dstar_gateway/)
[![License: GPL-2.0-or-later](https://img.shields.io/badge/License-GPL%20v2%2B-blue.svg)](https://www.gnu.org/licenses/old-licenses/gpl-2.0.html)

Async Rust D-STAR reflector gateway library. Implements the **DPlus**,
**DExtra**, and **DCS** reflector protocols with a full **typestate
session machine**, a **sans-io core**, **lenient parsing with
structured diagnostics**, and **symmetric client + server** scope.

## Status

**Version 0.1.0 alpha, unpublished.** The rewrite is feature-complete
on both the client side (`DPlus` + `DExtra` + DCS) and the reflector
server, which supports all three protocols (`DExtra`, `DPlus`, and
DCS) with every protocol enabled by default. It has not yet been
stress-tested against real reflectors for an extended period.
Cross-protocol forwarding, per-client rate limiting, and the
`StreamCache` header retransmit are all wired; cross-protocol
forwarding is off by default.

This crate lives in the [kenwood monorepo](https://github.com/swiftraccoon/kenwood)
and is not yet published to crates.io. Consume it via a path or
git dependency until the first published release lands. See
[CHANGELOG.md](CHANGELOG.md) for the work-in-progress feature
list.

## The three crates

| Crate | What it does |
|-------|--------------|
| [`dstar-gateway-core`](../dstar-gateway-core) | Sans-io codec + typestate `Session<P, S>` state machines. No tokio, no I/O. |
| [`dstar-gateway`](.) (this crate) | Tokio `AsyncSession<P>` shell, `DPlus` TCP `AuthClient`, optional XLX reflector-directory fetcher. |
| [`dstar-gateway-server`](../dstar-gateway-server) | Multi-client `Reflector` server. Supports `DExtra`, `DPlus`, and DCS, all enabled by default. |

## Quickstart

Connect to a `DExtra` reflector, listen for 10 seconds, then disconnect
cleanly.

```rust,no_run
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, DExtra, Session,
};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;
use tokio::time::timeout;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
// 1. Bind a local UDP socket.
let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

// 2. Build a Configured session.
let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
    .callsign(Callsign::try_from_str("W1AW")?)
    .local_module(Module::try_from_char('B')?)
    .reflector_module(Module::try_from_char('C')?)
    .peer("127.0.0.1:30001".parse()?)
    .build();

// 3. Drive the handshake manually on the test thread.
let mut connecting = session.connect(Instant::now())?;
let tx = connecting
    .poll_transmit(Instant::now())
    .ok_or("LINK not ready")?;
sock.send_to(tx.payload, tx.dst).await?;

let mut buf = [0u8; 64];
let (n, peer) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
    .await??;
connecting.handle_input(
    Instant::now(),
    peer,
    buf.get(..n).ok_or("receive length exceeded buffer")?,
)?;
assert_eq!(connecting.state_kind(), ClientStateKind::Connected);
let connected = connecting.promote()?;

// 4. Hand off to the tokio shell and listen for 10 seconds.
let mut session = AsyncSession::spawn(connected, Arc::clone(&sock));
let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
loop {
    tokio::select! {
        _ = tokio::time::sleep_until(deadline) => break,
        ev = session.next_event() => {
            match ev {
                Some(e) => println!("{e:?}"),
                None => break,
            }
        }
    }
}

// 5. Graceful disconnect.
session.disconnect().await?;
# Ok(()) }
```

For `DPlus`, fetch a host list with
[`AuthClient::authenticate`](src/auth/client.rs), build the configured
session, and call `Session::authenticate` before `connect`; `DCS` uses
the same shape as `DExtra`.

See the `dstar-gateway/examples/` directory for standalone runnable
versions of each.

## Features

| Feature | State | Notes |
|---------|-------|-------|
| `DPlus` client (REF, TCP auth) | Implemented | `AuthClient` + `Session<DPlus, _>` |
| `DExtra` client (XRF, XLX) | Implemented | `Session<DExtra, _>` |
| `DCS` client | Implemented | `Session<Dcs, _>`; header caching handled by core |
| `DExtra` reflector server | Implemented | `dstar-gateway-server::Reflector`, on by default |
| `DPlus` reflector server | Implemented | `handle_inbound_dplus` endpoint, on by default |
| `DCS` reflector server | Implemented | `handle_inbound_dcs` endpoint, on by default |
| `blocking` feature | Optional | CLI-friendly variant of `AsyncSession` |
| `hosts-fetcher` feature | Optional | Pulls `reqwest`; fetches the XLX reflector directory over HTTP |
| Slow-data sub-codec | Implemented | Short messages embedded in voice frames |
| DPRS position reports | Implemented | Decodes `$$CRC`-prefixed slow-data strings |
| Lenient parsing | Implemented | Structured `Diagnostic` via `DiagnosticSink` trait |
| Property tests + fuzz | Testing | 10 fuzz targets plus property and loopback tests |

## Feature flags

```toml
[dependencies]
dstar-gateway = { path = "../dstar-gateway", features = ["hosts-fetcher"] }
```

- `blocking`: compile a caller-driven synchronous shell backed by
  `std::net::UdpSocket`. Callers do not run a Tokio runtime, although
  Tokio remains an unconditional dependency of this async client crate.
- `hosts-fetcher`: pulls `reqwest`; fetches the XLX reflector
  directory from `http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname`
  via `HostsFetcher::fetch_xlx_directory`, returning protocol-tagged
  host entries.
- `examples-network`: compile examples that contact live network services.
- `hardware-tests`: compile ignored live-reflector integration tests.
  These last two flags are for examples/tests rather than downstream APIs.

## Documentation

- [API reference](https://swiftraccoon.github.io/kenwood/dstar_gateway/)
- [`book/`](book/): mdBook source; currently covers the introduction and Getting Started. Build with `mdbook serve book`
- [ARCHITECTURE.md](ARCHITECTURE.md): 5000-foot design overview
- [REFERENCES.md](REFERENCES.md): line-numbered references into
  `ircDDBGateway` and `xlxd`
- [CHANGELOG.md](CHANGELOG.md): per-release notes
- `adr/`: architectural decision records

## License

GPL-2.0-or-later, matching the upstream `g4klx/ircDDBGateway` and
`LX3JL/xlxd` reference implementations.
