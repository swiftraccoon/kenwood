//! The shared connect pump drives a `Connecting` session to `Connected`
//! against the fake reflector, and times out cleanly against silence.

// Deps visible to every dstar-gateway test target but unused here.
#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use pcap_parser as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway::tokio_shell::{ConnectError, drive_connecting};
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};
use tokio::net::UdpSocket;

mod common;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn configured_dextra(
    peer: std::net::SocketAddr,
) -> Result<Session<DExtra, Configured>, dstar_gateway_core::TypeError> {
    Ok(Session::<DExtra, Configured>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build())
}

#[tokio::test]
async fn drive_connecting_reaches_connected_against_the_fake_reflector() -> TestResult {
    let fake = FakeReflector::spawn_dextra().await?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let connecting = configured_dextra(fake.local_addr()?)?
        .connect(Instant::now())
        .map_err(|failed| failed.error)?;

    let connected = drive_connecting(connecting, &socket, Duration::from_secs(2)).await?;
    drop(connected);
    Ok(())
}

#[tokio::test]
async fn drive_connecting_times_out_against_silence() -> TestResult {
    // A bound socket that never answers: the pump must report the timeout
    // instead of spinning forever.
    let silent = UdpSocket::bind("127.0.0.1:0").await?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let connecting = configured_dextra(silent.local_addr()?)?
        .connect(Instant::now())
        .map_err(|failed| failed.error)?;

    let result = drive_connecting(connecting, &socket, Duration::from_millis(300)).await;
    assert!(
        matches!(result, Err(ConnectError::TimedOut(_))),
        "silence must time out: {result:?}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn sub_tick_deadline_expires_at_the_exact_total_budget() -> TestResult {
    let silent = UdpSocket::bind("127.0.0.1:0").await?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let connecting = configured_dextra(silent.local_addr()?)?.connect(Instant::now())?;
    let budget = Duration::from_millis(7);
    let start = tokio::time::Instant::now();
    let result = drive_connecting(connecting, &socket, budget).await;
    assert!(
        matches!(result, Err(ConnectError::TimedOut(got)) if got == budget),
        "{result:?}"
    );
    assert_eq!(
        start.elapsed(),
        budget,
        "the old polling tick must not extend the deadline"
    );
    Ok(())
}

#[tokio::test]
async fn unrepresentable_deadline_fails_before_udp_send() -> TestResult {
    let silent = UdpSocket::bind("127.0.0.1:0").await?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let connecting = configured_dextra(silent.local_addr()?)?.connect(Instant::now())?;
    let result = drive_connecting(connecting, &socket, Duration::MAX).await;
    assert!(
        matches!(result, Err(ConnectError::DeadlineOverflow(_))),
        "{result:?}"
    );
    let mut bytes = [0_u8; 32];
    assert!(
        matches!(silent.try_recv_from(&mut bytes), Err(error)
        if error.kind() == std::io::ErrorKind::WouldBlock),
        "invalid deadline sent a datagram"
    );
    Ok(())
}

#[tokio::test]
async fn acknowledgement_from_a_foreign_socket_cannot_finish_the_handshake() -> TestResult {
    let silent = UdpSocket::bind("127.0.0.1:0").await?;
    let foreign = UdpSocket::bind("127.0.0.1:0").await?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let connecting = configured_dextra(silent.local_addr()?)?.connect(Instant::now())?;
    let destination = socket.local_addr()?;
    let sender = async {
        let mut bytes = [0_u8; 32];
        let _request = silent.recv_from(&mut bytes).await?;
        let _sent = foreign.send_to(b"W1AW    BCACK\0", destination).await?;
        Ok::<_, std::io::Error>(())
    };
    let (result, sent) = tokio::join!(
        drive_connecting(connecting, &socket, Duration::from_millis(40)),
        sender,
    );
    sent?;
    assert!(
        matches!(result, Err(ConnectError::TimedOut(_))),
        "{result:?}"
    );
    Ok(())
}
