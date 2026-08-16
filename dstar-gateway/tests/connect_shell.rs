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

mod common;

use std::time::{Duration, Instant};

use common::fake_reflector::FakeReflector;
use dstar_gateway::tokio_shell::{ConnectError, drive_connecting};
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn configured_dextra(peer: std::net::SocketAddr) -> Session<DExtra, Configured> {
    Session::<DExtra, Configured>::builder()
        .callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build()
}

#[tokio::test]
async fn drive_connecting_reaches_connected_against_the_fake_reflector() -> TestResult {
    let fake = FakeReflector::spawn_dextra().await?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let connecting = configured_dextra(fake.local_addr()?)
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
    let connecting = configured_dextra(silent.local_addr()?)
        .connect(Instant::now())
        .map_err(|failed| failed.error)?;

    let result = drive_connecting(connecting, &socket, Duration::from_millis(300)).await;
    assert!(
        matches!(result, Err(ConnectError::TimedOut(_))),
        "silence must time out: {result:?}"
    );
    Ok(())
}
