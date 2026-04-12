//! End-to-end loopback test: `DPlus` 2-step login rejected with `BUSY`.
//!
//! The `FakeReflector` spawns in rejecting mode — it answers the
//! 28-byte LINK2 with `[0x08, 0xC0, 0x04, 0x00, 'B', 'U', 'S', 'Y']`
//! instead of `OKRW`. The sans-io core should flip to `Closed` with
//! `DisconnectReason::Rejected` **without** the shell ever having to
//! spawn a session loop: the typestate `.promote()` call fails
//! because the session is in `Closed`, not `Connected`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    Authenticated, ClientStateKind, Configured, DPlus, DisconnectReason, Event, Session,
};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;
use tokio::time::timeout;

use dstar_gateway as _;
use pcap_parser as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

#[tokio::test]
async fn dplus_rejecting_reflector_closes_session() -> Result<(), Box<dyn std::error::Error>> {
    let fake = FakeReflector::spawn_dplus_rejecting().await?;
    let reflector_addr = fake.local_addr()?;

    let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(reflector_addr)
        .build();
    let authenticated: Session<DPlus, Authenticated> = session.authenticate(HostList::new())?;
    let mut connecting = authenticated.connect(Instant::now())?;

    // Round 1: LINK1 → LINK1_ACK.
    let now = Instant::now();
    let tx = connecting.poll_transmit(now).ok_or("LINK1 not ready")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    let mut buf = [0u8; 64];
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;

    // LINK2 out.
    let tx = connecting
        .poll_transmit(Instant::now())
        .ok_or("LINK2 not ready")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    // Round 2: BUSY reply.
    let (n, peer) = timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, buf.get(..n).ok_or("n out of bounds")?)?;

    // After the rejection the core has advanced to `Closed`.
    assert_eq!(
        connecting.state_kind(),
        ClientStateKind::Closed,
        "DPlus should be Closed after BUSY"
    );

    // `promote` should fail now — there's no `Session<DPlus, Connected>`
    // to spawn because the session never reached that state.
    let Err(failed) = connecting.promote() else {
        return Err("promote should have failed when Closed".into());
    };
    assert_eq!(
        failed.session.state_kind(),
        ClientStateKind::Closed,
        "the failed handle carries the Closed session back to the caller"
    );

    // Drain any pending events off the unspawned session — there
    // should be a `Disconnected { reason: Rejected }` waiting.
    let mut drained = failed.session;
    let event = drained.poll_event().ok_or("rejected event not emitted")?;
    match event {
        Event::<DPlus>::Disconnected { reason } => {
            assert_eq!(reason, DisconnectReason::Rejected);
        }
        ref other => unreachable!("expected Disconnected {{ Rejected }}, got {other:?}"),
    }

    Ok(())
}
