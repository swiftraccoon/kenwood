//! Hardware-in-the-loop tests against real reflectors.
//!
//! Gated behind the `hardware-tests` feature flag. Requires a
//! network-reachable D-STAR reflector for each protocol and
//! environment variables telling the tests which reflector to reach.
//!
//! Run with:
//!
//! ```bash
//! cargo test -p dstar-gateway --features hardware-tests --test hardware -- --test-threads=1 --ignored
//! ```
//!
//! # Environment variables
//!
//! - `DSTAR_TEST_REFLECTOR_DPLUS` — host name of a `DPlus` (REF)
//!   reflector (default: `REF030`). The tests append `:20001`.
//! - `DSTAR_TEST_REFLECTOR_DEXTRA` — host name of a `DExtra` (XRF/XLX)
//!   reflector (default: `XLX307`). The tests append `:30001`.
//! - `DSTAR_TEST_REFLECTOR_DCS` — host name of a `DCS` reflector
//!   (default: `DCS001`). The tests append `:30051`.
//! - `DSTAR_TEST_CALLSIGN` — user callsign to authenticate with
//!   (default: `TEST    `). Must be a valid amateur radio callsign when
//!   running the TX test against a real reflector.
//! - `DSTAR_TEST_TX_OK` — must be set to `1` to enable the voice-burst
//!   TX test. Unset by default so accidentally running `--ignored`
//!   against a live reflector does not key the air.
//!
//! # Triple gate
//!
//! The tests are triple-gated — `#[cfg(feature = "hardware-tests")]`
//! excludes them from default compilation, `#[ignore]` excludes them
//! from the default test pass, and `DSTAR_TEST_TX_OK=1` is further
//! required for any test that transmits. Remove none of these gates.

#![cfg(feature = "hardware-tests")]
#![allow(
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "integration test — recv_from returns n within buf; test functions are necessarily long"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    Authenticated, ClientStateKind, Configured, Connecting, DExtra, DPlus, Dcs, Session,
};
use dstar_gateway_core::types::{Callsign, Module, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;
use pcap_parser as _;
use thiserror as _;
use tokio::net::{UdpSocket, lookup_host};
use tokio::time::timeout;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

/// Default reflector hostnames when the env vars are unset.
const DEFAULT_REFLECTOR_DPLUS: &str = "REF030";
const DEFAULT_REFLECTOR_DEXTRA: &str = "XLX307";
const DEFAULT_REFLECTOR_DCS: &str = "DCS001";

/// Default callsign used when `DSTAR_TEST_CALLSIGN` is unset. Must stay
/// eight characters wide (space-padded) to match the `Callsign` invariant.
const DEFAULT_TEST_CALLSIGN: &str = "TEST    ";

/// Port numbers for the three protocols (fixed by the D-STAR spec).
const DPLUS_PORT: u16 = 20001;
const DEXTRA_PORT: u16 = 30001;
const DCS_PORT: u16 = 30051;

/// Upper bound on the handshake + listen window per test. The real
/// reflector has to ACK within this or we fail the test.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long we listen for server frames after the session is spawned.
const LISTEN_DURATION: Duration = Duration::from_secs(5);

/// Final disconnect deadline.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Pull the station callsign from the env or fall back to a default.
fn test_callsign() -> Result<Callsign, Box<dyn std::error::Error>> {
    let raw =
        std::env::var("DSTAR_TEST_CALLSIGN").unwrap_or_else(|_| DEFAULT_TEST_CALLSIGN.to_string());
    Ok(Callsign::try_from_str(&raw)?)
}

/// Resolve `"<host>:<port>"` to a `SocketAddr`. Fails the test on DNS
/// error or empty result.
async fn resolve_peer(host: &str, port: u16) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let target = format!("{host}:{port}");
    let mut addrs = lookup_host(&target).await?;
    addrs
        .next()
        .ok_or_else(|| format!("reflector host {target} resolved to zero addresses").into())
}

/// Build a well-formed `DStarHeader` with the given reflector callsign
/// prefix (e.g., `"REF030"`) and local callsign. Pattern matches the
/// loopback tests in `loopback_dplus.rs`.
fn build_header(
    reflector_prefix: &str,
    reflector_module: char,
    my_call: Callsign,
) -> Result<DStarHeader, Box<dyn std::error::Error>> {
    let rpt1 = format!("{reflector_prefix} {reflector_module}");
    let rpt2 = format!("{reflector_prefix} G");
    Ok(DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::try_from_str(&rpt2)?,
        rpt1: Callsign::try_from_str(&rpt1)?,
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call,
        my_suffix: Suffix::EMPTY,
    })
}

/// Drive a `Session<DExtra, _>` or `Session<Dcs, _>` through its
/// single-round handshake against a real reflector.
///
/// Both protocols share the same shape: enqueue LINK, pop it, send it
/// over the socket, wait for the ACK, feed it back.
async fn drive_single_round_handshake<P>(
    connecting: &mut Session<P, Connecting>,
    client_sock: &UdpSocket,
) -> Result<(), Box<dyn std::error::Error>>
where
    P: dstar_gateway_core::session::client::Protocol + Send + 'static,
{
    let now = Instant::now();
    let tx = connecting
        .poll_transmit(now)
        .ok_or("LINK not ready in outbox after connect")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    let mut buf = [0u8; 2048];
    let (n, peer) = timeout(HANDSHAKE_TIMEOUT, client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, &buf[..n])?;
    Ok(())
}

/// Drive a `Session<DPlus, Connecting>` through the two-round
/// `LINK1` / `LINK1_ACK` / `LINK2` / `OKRW` dance. Identical to
/// `loopback_dplus::drive_dplus_handshake` but with a hardware-sized
/// timeout.
async fn drive_dplus_handshake(
    connecting: &mut Session<DPlus, Connecting>,
    client_sock: &UdpSocket,
) -> Result<(), Box<dyn std::error::Error>> {
    // Round 1: LINK1 out -> LINK1_ACK in.
    let now = Instant::now();
    let tx = connecting
        .poll_transmit(now)
        .ok_or("LINK1 not ready in outbox")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    let mut buf = [0u8; 2048];
    let (n, peer) = timeout(HANDSHAKE_TIMEOUT, client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, &buf[..n])?;

    // Handling the LINK1_ACK causes the core to enqueue LINK2.
    let tx = connecting
        .poll_transmit(Instant::now())
        .ok_or("LINK2 not ready in outbox")?;
    let _ = client_sock.send_to(tx.payload, tx.dst).await?;

    // Round 2: wait for the LINK2 reply (OKRW or BUSY).
    let (n, peer) = timeout(HANDSHAKE_TIMEOUT, client_sock.recv_from(&mut buf)).await??;
    connecting.handle_input(Instant::now(), peer, &buf[..n])?;
    Ok(())
}

/// Drain any inbound events for `duration`, discarding them. This is
/// the "listen while the reflector transmits at us" phase of each
/// test — we don't assert on frame contents, only that the session
/// machinery does not crash.
async fn listen_for<P>(session: &mut AsyncSession<P>, duration: Duration)
where
    P: dstar_gateway_core::session::client::Protocol + Send + 'static,
{
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return,
            evt = session.next_event() => {
                if evt.is_none() {
                    return;
                }
            }
        }
    }
}

// ─── DPlus (REF) ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires hardware + live reflector"]
async fn dplus_connect_listen_disconnect_against_real_ref030()
-> Result<(), Box<dyn std::error::Error>> {
    let reflector = std::env::var("DSTAR_TEST_REFLECTOR_DPLUS")
        .unwrap_or_else(|_| DEFAULT_REFLECTOR_DPLUS.to_string());
    let callsign = test_callsign()?;
    let peer = resolve_peer(&reflector, DPLUS_PORT).await?;

    // Pull the host list over TCP before touching UDP.
    let hosts = AuthClient::new().authenticate(callsign).await?;

    let client_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build();
    let authenticated: Session<DPlus, Authenticated> = session.authenticate(hosts)?;
    let mut connecting = authenticated.connect(Instant::now())?;

    drive_dplus_handshake(&mut connecting, &client_sock).await?;
    assert_eq!(
        connecting.state_kind(),
        ClientStateKind::Connected,
        "DPlus must be Connected after OKRW"
    );
    let connected = connecting.promote()?;

    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    listen_for(&mut async_session, LISTEN_DURATION).await;

    timeout(DISCONNECT_TIMEOUT, async_session.disconnect()).await??;
    drop(async_session);
    Ok(())
}

// ─── DExtra (XRF/XLX) ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires hardware + live reflector"]
async fn dextra_connect_listen_disconnect_against_real_xlx307()
-> Result<(), Box<dyn std::error::Error>> {
    let reflector = std::env::var("DSTAR_TEST_REFLECTOR_DEXTRA")
        .unwrap_or_else(|_| DEFAULT_REFLECTOR_DEXTRA.to_string());
    let callsign = test_callsign()?;
    let peer = resolve_peer(&reflector, DEXTRA_PORT).await?;

    let client_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build();
    let mut connecting = session.connect(Instant::now())?;

    drive_single_round_handshake(&mut connecting, &client_sock).await?;
    assert_eq!(
        connecting.state_kind(),
        ClientStateKind::Connected,
        "DExtra must be Connected after ACK"
    );
    let connected = connecting.promote()?;

    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    listen_for(&mut async_session, LISTEN_DURATION).await;

    timeout(DISCONNECT_TIMEOUT, async_session.disconnect()).await??;
    drop(async_session);
    Ok(())
}

// ─── DCS ──────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires hardware + live reflector"]
async fn dcs_connect_listen_disconnect_against_real_dcs001()
-> Result<(), Box<dyn std::error::Error>> {
    let reflector = std::env::var("DSTAR_TEST_REFLECTOR_DCS")
        .unwrap_or_else(|_| DEFAULT_REFLECTOR_DCS.to_string());
    let callsign = test_callsign()?;
    let peer = resolve_peer(&reflector, DCS_PORT).await?;

    let client_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<Dcs, Configured> = Session::<Dcs, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build();
    let mut connecting = session.connect(Instant::now())?;

    drive_single_round_handshake(&mut connecting, &client_sock).await?;
    assert_eq!(
        connecting.state_kind(),
        ClientStateKind::Connected,
        "DCS must be Connected after ACK"
    );
    let connected = connecting.promote()?;

    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    listen_for(&mut async_session, LISTEN_DURATION).await;

    timeout(DISCONNECT_TIMEOUT, async_session.disconnect()).await??;
    drop(async_session);
    Ok(())
}

// ─── DPlus voice TX (doubly gated) ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires hardware + live reflector + DSTAR_TEST_TX_OK=1"]
async fn dplus_voice_burst_to_real_reflector() -> Result<(), Box<dyn std::error::Error>> {
    // Safety belt: this test keys the transmitter against a live
    // reflector and will broadcast on-air to any station monitoring
    // that reflector. Refuse to run unless the operator has explicitly
    // opted in.
    if std::env::var("DSTAR_TEST_TX_OK").ok().as_deref() != Some("1") {
        eprintln!(
            "Skipping voice-burst TX test: set DSTAR_TEST_TX_OK=1 to enable \
             (transmits on real reflector)"
        );
        return Ok(());
    }

    let reflector = std::env::var("DSTAR_TEST_REFLECTOR_DPLUS")
        .unwrap_or_else(|_| DEFAULT_REFLECTOR_DPLUS.to_string());
    let callsign = test_callsign()?;
    let peer = resolve_peer(&reflector, DPLUS_PORT).await?;

    let hosts = AuthClient::new().authenticate(callsign).await?;

    let client_sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build();
    let authenticated: Session<DPlus, Authenticated> = session.authenticate(hosts)?;
    let mut connecting = authenticated.connect(Instant::now())?;

    drive_dplus_handshake(&mut connecting, &client_sock).await?;
    assert_eq!(connecting.state_kind(), ClientStateKind::Connected);
    let connected = connecting.promote()?;

    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&client_sock));

    // Drain a brief listen window so any pending server frames are
    // flushed before we start TX.
    listen_for(&mut async_session, Duration::from_millis(500)).await;

    // Build a header + transmit three voice frames + EOT.
    let header = build_header(reflector.as_str(), 'C', callsign)?;
    let sid = StreamId::new(0x1234).ok_or("zero stream id")?;
    async_session.send_header(header, sid).await?;

    let frame = VoiceFrame::silence();
    for seq in 0u8..3 {
        async_session.send_voice(sid, seq, frame).await?;
    }
    async_session.send_eot(sid, 3).await?;

    // Let the last packets hit the wire.
    tokio::time::sleep(Duration::from_millis(100)).await;

    timeout(DISCONNECT_TIMEOUT, async_session.disconnect()).await??;
    drop(async_session);
    Ok(())
}
