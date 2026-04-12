# dstar-gateway

[![CI](https://github.com/swiftraccoon/dstar-gateway/actions/workflows/ci.yml/badge.svg)](https://github.com/swiftraccoon/dstar-gateway/actions/workflows/ci.yml)
[![docs.rs](https://docs.rs/dstar-gateway/badge.svg)](https://docs.rs/dstar-gateway)
[![Book](https://img.shields.io/badge/book-mdbook-blue)](https://swiftraccoon.github.io/dstar-gateway/)
[![License: GPL-2.0-or-later](https://img.shields.io/badge/License-GPL%20v2%2B-blue.svg)](https://www.gnu.org/licenses/old-licenses/gpl-2.0.html)

Async Rust D-STAR reflector gateway library. Implements the **DPlus**,
**DExtra**, and **DCS** reflector protocols with a full **typestate
session machine**, a **sans-io core**, **lenient parsing with
structured diagnostics**, and **symmetric client + server** scope.

## Status

**Alpha. Pre-0.1.0, unpublished.** The rewrite is feature-complete
on the client side (`DPlus` + `DExtra` + DCS) but has not been
stress-tested against real reflectors for an extended period.
The reflector server currently ships `DExtra` only; `DPlus` and DCS
server support, cross-protocol forwarding, per-client rate
limiting, and the `StreamCache` header retransmit wiring are all
open deferred work.

This crate lives in the [kenwood monorepo](https://github.com/swiftraccoon/kenwood)
and is not yet published to crates.io. Consume it via a path or
git dependency until the first numbered release lands. See
[CHANGELOG.md](CHANGELOG.md) for the work-in-progress feature
list.

## The three crates

| Crate | What it does |
|-------|--------------|
| [`dstar-gateway-core`](../dstar-gateway-core) | Sans-io codec + typestate `Session<P, S>` state machines. No tokio, no I/O. |
| [`dstar-gateway`](.) (this crate) | Tokio `AsyncSession<P>` shell, `DPlus` TCP `AuthClient`, optional Pi-Star host-file fetcher. |
| [`dstar-gateway-server`](../dstar-gateway-server) | Multi-client `Reflector` server (`DExtra` only today; `DPlus`/DCS deferred). |

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
let tx = connecting.poll_transmit(Instant::now()).expect("LINK");
sock.send_to(tx.payload, tx.dst).await?;

let mut buf = [0u8; 64];
let (n, peer) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
    .await??;
connecting.handle_input(Instant::now(), peer, &buf[..n])?;
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

For `DPlus` you additionally call
[`AuthClient::authenticate`](src/auth/client.rs) before building the
`Session<DPlus, Configured>`; `DCS` uses the same shape as `DExtra`.

See the `dstar-gateway/examples/` directory for standalone runnable
versions of each.

## Features

| Feature | Status | Notes |
|---------|--------|-------|
| `DPlus` client (REF, TCP auth) | Stable | `AuthClient` + `Session<DPlus, _>` |
| `DExtra` client (XRF, XLX) | Stable | `Session<DExtra, _>` |
| `DCS` client | Stable | `Session<Dcs, _>` — header caching handled by core |
| `DExtra` reflector server | Stable | `dstar-gateway-server::Reflector` |
| `DPlus` reflector server | Deferred | Core typestate exists; shell not wired |
| `DCS` reflector server | Deferred | Core typestate exists; shell not wired |
| `blocking` feature | Optional | CLI-friendly variant of `AsyncSession` |
| `hosts-fetcher` feature | Optional | Pulls `reqwest`; downloads Pi-Star host files |
| Slow-data sub-codec | Stable | Short messages embedded in voice frames |
| DPRS position reports | Stable | Decodes `$$CRC`-prefixed slow-data strings |
| Lenient parsing | Stable | Structured `Diagnostic` via `DiagnosticSink` trait |
| Property tests + fuzz | Stable | 10 fuzz targets, 75M executions, 0 crashes |

## Feature flags

```toml
[dependencies]
dstar-gateway = { path = "../dstar-gateway", features = ["hosts-fetcher"] }
```

- `blocking` — compile a non-tokio blocking shell for CLI scripts and
  test fixtures.
- `hosts-fetcher` — pulls `reqwest`; downloads the Pi-Star
  `DPlus_Hosts.txt`, `DExtra_Hosts.txt`, and `DCS_Hosts.txt` files.

## Documentation

- [API reference on docs.rs](https://docs.rs/dstar-gateway)
- [The dstar-gateway Book (mdBook)](https://swiftraccoon.github.io/dstar-gateway/)
- [ARCHITECTURE.md](ARCHITECTURE.md) — 5000-foot design overview
- [REFERENCES.md](REFERENCES.md) — line-numbered references into
  `ircDDBGateway` and `xlxd`
- [CHANGELOG.md](CHANGELOG.md) — per-release notes
- `adr/` — architectural decision records

## License

GPL-2.0-or-later, matching the upstream `g4klx/ircDDBGateway` and
`LX3JL/xlxd` reference implementations.
