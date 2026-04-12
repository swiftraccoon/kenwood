//! Multi-client `DExtra` fan-out integration test.
//!
//! Three fake UDP clients link to module `C` on a single
//! `ProtocolEndpoint<DExtra>`. Client A then transmits a small
//! voice burst (header + data frames + EOT). We assert that:
//!
//! - Both client B and client C receive every voice packet.
//! - Client A does not receive its own fan-out (no echo).
//! - All three clients received the 14-byte LINK ACK exactly once.
//!
//! The test bypasses the top-level `Reflector::run` to keep the
//! wiring minimal — the real multi-protocol orchestration is a
//! thin wrapper over the same endpoint code path. A separate
//! smoke test covers `Reflector::new_with_socket` + shutdown.

// Integration tests live in a separate compilation unit from the
// library crate. Match the library's lint opt-out so test code stays
// expressive.
#![allow(
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable,
    unused_results
)]

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::watch;

use dstar_gateway_core::codec::dextra::{
    encode_connect_link, encode_voice_data, encode_voice_eot, encode_voice_header,
};
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::client::DExtra;
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;
use dstar_gateway_server::{
    AllowAllAuthorizer, ProtocolEndpoint, Reflector, ReflectorConfig, ShellError,
};

// Workspace dev-deps used by sibling test targets. Acknowledge them
// here so the strict `unused_crate_dependencies` lint stays silent
// for this integration test.
use proptest as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

const fn sid() -> StreamId {
    match StreamId::new(0x4242) {
        Some(s) => s,
        None => unreachable!(),
    }
}

const fn header_for(my: [u8; 8]) -> DStarHeader {
    DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(my),
        my_suffix: Suffix::from_wire_bytes(*b"D75 "),
    }
}

async fn drain_one(sock: &UdpSocket, buf: &mut [u8], label: &str) -> Result<usize, std::io::Error> {
    tokio::time::timeout(Duration::from_secs(1), sock.recv_from(buf))
        .await
        .unwrap_or_else(|_| panic!("{label} recv_from timed out"))
        .map(|(n, _)| n)
}

#[tokio::test]
async fn three_clients_fan_out_voice_without_echo() -> Result<(), Box<dyn std::error::Error>> {
    // Endpoint socket — simulates the reflector binding :30001.
    let endpoint_socket = UdpSocket::bind("127.0.0.1:0").await?;
    let endpoint_addr = endpoint_socket.local_addr()?;
    let endpoint_socket = Arc::new(endpoint_socket);

    // Spawn the DExtra endpoint on a dedicated tokio task driven by
    // a watch channel shutdown signal.
    let endpoint = Arc::new(ProtocolEndpoint::<DExtra>::new(
        ProtocolKind::DExtra,
        Module::C,
        Arc::new(AllowAllAuthorizer),
    ));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let endpoint_task = {
        let endpoint = Arc::clone(&endpoint);
        let endpoint_socket = Arc::clone(&endpoint_socket);
        tokio::spawn(async move { endpoint.run(endpoint_socket, shutdown_rx).await })
    };

    // Three fake clients. Each has its own loopback UDP socket so we
    // can observe what the reflector sends back.
    let client_a = UdpSocket::bind("127.0.0.1:0").await?;
    let client_b = UdpSocket::bind("127.0.0.1:0").await?;
    let client_c = UdpSocket::bind("127.0.0.1:0").await?;

    // Link all three to module C.
    for (sock, callsign) in [
        (&client_a, Callsign::from_wire_bytes(*b"W1AW    ")),
        (&client_b, Callsign::from_wire_bytes(*b"N0CLL   ")),
        (&client_c, Callsign::from_wire_bytes(*b"K2ABC   ")),
    ] {
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(&mut link_buf, &callsign, Module::C, Module::B)?;
        sock.send_to(&link_buf[..n], endpoint_addr).await?;
    }

    // Each client receives its own LINK ACK (14 bytes, tag "ACK").
    // The ACK tag offset is asserted by the codec's own golden tests;
    // here we just verify the length and that the tag is present in
    // the payload somewhere.
    for (sock, label) in [
        (&client_a, "client_a ack"),
        (&client_b, "client_b ack"),
        (&client_c, "client_c ack"),
    ] {
        let mut ack_buf = [0u8; 64];
        let n = drain_one(sock, &mut ack_buf, label).await?;
        assert_eq!(n, 14, "{label}: DExtra ACK is 14 bytes");
        assert!(
            ack_buf[..n].windows(3).any(|w| w == b"ACK"),
            "{label}: payload must contain ACK tag"
        );
    }

    // Now client A transmits a voice burst.
    // 1 header + 2 data frames + 1 EOT = 4 fan-out packets.
    let hdr = header_for(*b"W1AW    ");
    let mut hdr_buf = [0u8; 64];
    let hdr_len = encode_voice_header(&mut hdr_buf, sid(), &hdr)?;
    client_a.send_to(&hdr_buf[..hdr_len], endpoint_addr).await?;

    let frame = VoiceFrame::silence();
    let mut data_buf = [0u8; 64];
    for seq in 0_u8..2 {
        let n = encode_voice_data(&mut data_buf, sid(), seq, &frame)?;
        client_a.send_to(&data_buf[..n], endpoint_addr).await?;
    }
    let mut eot_buf = [0u8; 64];
    let eot_len = encode_voice_eot(&mut eot_buf, sid(), 0x40 | 2)?;
    client_a.send_to(&eot_buf[..eot_len], endpoint_addr).await?;

    // Clients B and C each receive 4 fan-out packets.
    for (sock, label) in [(&client_b, "client_b voice"), (&client_c, "client_c voice")] {
        let mut buf = [0u8; 128];
        let n1 = drain_one(sock, &mut buf, &format!("{label} header")).await?;
        assert_eq!(n1, hdr_len, "{label}: header size");
        assert_eq!(&buf[..n1], &hdr_buf[..hdr_len], "{label}: header bytes");

        let n2 = drain_one(sock, &mut buf, &format!("{label} data0")).await?;
        assert_eq!(n2, 27, "{label}: data frame size");

        let n3 = drain_one(sock, &mut buf, &format!("{label} data1")).await?;
        assert_eq!(n3, 27, "{label}: data frame size");

        let n4 = drain_one(sock, &mut buf, &format!("{label} eot")).await?;
        assert_eq!(n4, 27, "{label}: EOT size");
    }

    // Client A must not have received any fan-out — it's the sender.
    // We consider "no datagram in 100 ms" sufficient proof of no echo.
    let mut scratch = [0u8; 128];
    let no_echo =
        tokio::time::timeout(Duration::from_millis(100), client_a.recv_from(&mut scratch)).await;
    assert!(
        no_echo.is_err(),
        "client A must not receive its own fan-out"
    );

    // Shut the endpoint down and confirm the task returns cleanly.
    shutdown_tx.send(true)?;
    let join = tokio::time::timeout(Duration::from_secs(2), endpoint_task).await?;
    match join {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("endpoint returned error: {e:?}").into()),
        Err(join_err) => return Err(format!("endpoint task panic: {join_err:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn reflector_new_with_socket_shutdown_smoke_test() -> Result<(), Box<dyn std::error::Error>> {
    // Bind a socket manually so the caller knows the port.
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let bound = socket.local_addr()?;

    let mut modules = std::collections::HashSet::new();
    let _ = modules.insert(Module::C);

    let config = ReflectorConfig::builder()
        .callsign(Callsign::from_wire_bytes(*b"REF030  "))
        .module_set(modules)
        .bind(bound)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .build()?;

    let reflector = Arc::new(Reflector::new_with_socket(
        config,
        AllowAllAuthorizer,
        Arc::new(socket),
    ));

    // Schedule shutdown after a short delay, then run.
    let r2 = Arc::clone(&reflector);
    let _t = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        r2.shutdown();
    });

    let result: Result<Result<(), ShellError>, tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(2), reflector.run()).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("reflector errored: {e:?}"),
        Err(elapsed) => panic!("reflector.run did not exit within 2s: {elapsed}"),
    }
    Ok(())
}
