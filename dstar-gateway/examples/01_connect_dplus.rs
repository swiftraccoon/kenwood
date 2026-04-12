//! Minimal `DPlus` connect example.
//!
//! Connects to a `DPlus` reflector over loopback, listens for 10
//! seconds of inbound events, and disconnects gracefully. The
//! reflector address is hardcoded to `127.0.0.1:20001` so the
//! example **builds** hermetically; `cargo run` will fail to
//! handshake unless a real reflector is listening on that port.
//!
//! Run with:
//! ```text
//! cargo run -p dstar-gateway --example 01_connect_dplus
//! ```
//!
//! `DPlus` is the only protocol that requires a TCP auth step
//! before the UDP session can begin. In production code you
//! would fetch a real host list via
//! `dstar_gateway::auth::AuthClient::authenticate`. This example
//! attaches an empty `HostList` as a placeholder, mirroring the
//! `tests/loopback_dplus.rs` integration test.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    Authenticated, ClientStateKind, Configured, Connecting, DPlus, Session,
};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;
use tokio::time::timeout;

// Examples are a separate compilation unit — acknowledge workspace
// dev-deps we don't reference directly so the strict
// `unused_crate_dependencies` lint stays silent.
use pcap_parser as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(Callsign::try_from_str("W1AW")?)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer("127.0.0.1:20001".parse()?)
        .build();

    let authenticated: Session<DPlus, Authenticated> = session.authenticate(HostList::new())?;

    let mut connecting: Session<DPlus, Connecting> = authenticated.connect(Instant::now())?;

    for _ in 0..2 {
        let Some(tx) = connecting.poll_transmit(Instant::now()) else {
            break;
        };
        let _ = sock.send_to(tx.payload, tx.dst).await?;

        let mut buf = [0u8; 64];
        let Ok(recv) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await else {
            eprintln!("timeout waiting for reflector reply — is 127.0.0.1:20001 listening?");
            return Ok(());
        };
        let (n, peer) = recv?;
        let slice = buf.get(..n).unwrap_or(&[]);
        connecting.handle_input(Instant::now(), peer, slice)?;
        if connecting.state_kind() == ClientStateKind::Connected {
            break;
        }
    }

    if connecting.state_kind() != ClientStateKind::Connected {
        eprintln!("handshake did not complete");
        return Ok(());
    }

    let connected = connecting.promote()?;
    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&sock));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            ev = async_session.next_event() => match ev {
                Some(e) => println!("event: {e:?}"),
                None => break,
            },
        }
    }

    async_session.disconnect().await?;
    Ok(())
}
