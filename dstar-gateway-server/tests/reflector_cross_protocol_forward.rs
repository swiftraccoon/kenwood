//! Loopback forwarding through all three public reflector endpoints.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use dstar_gateway_core::codec::{dcs, dextra, dplus};
use dstar_gateway_core::header::{DstarHeader, HeaderFlags};
use dstar_gateway_core::types::{
    Callsign, DcsReflector, FrameSequence, Module, RepeaterIdentity, RoutingCallsign, StreamId,
    Suffix,
};
use dstar_gateway_core::voice::VoiceFrame;
use dstar_gateway_server::{AllowAllAuthorizer, ProtocolSockets, Reflector, ReflectorConfig};
use tokio::net::UdpSocket;

use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

const CLIENT: RepeaterIdentity = RepeaterIdentity {
    callsign: match RoutingCallsign::from_padded_bytes(*b"W1AW   ") {
        Ok(value) => value,
        Err(_) => unreachable!(),
    },
    module: Module::B,
};
const REFLECTOR: DcsReflector =
    match DcsReflector::try_from_routing(match RoutingCallsign::from_padded_bytes(*b"DCS030 ") {
        Ok(value) => value,
        Err(_) => unreachable!(),
    }) {
        Ok(value) => value,
        Err(_) => unreachable!(),
    };
const STREAM: StreamId = StreamId::new(0x4242).unwrap();

async fn receive(socket: &UdpSocket) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = [0; 128];
    let length = tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut bytes)).await??;
    Ok(bytes.get(..length).ok_or("received length")?.to_vec())
}

async fn send(
    socket: &UdpSocket,
    bytes: &[u8],
    destination: std::net::SocketAddr,
) -> Result<(), std::io::Error> {
    let _length = socket.send_to(bytes, destination).await?;
    Ok(())
}

#[tokio::test]
async fn three_protocol_cross_protocol_forwarding() -> Result<(), Box<dyn std::error::Error>> {
    let dextra_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    let dplus_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    let dcs_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    let dextra_address = dextra_socket.local_addr()?;
    let dplus_address = dplus_socket.local_addr()?;
    let dcs_address = dcs_socket.local_addr()?;
    let config = ReflectorConfig::builder()
        .callsign(REFLECTOR.routing())
        .module_set(HashSet::from([Module::C]))
        .bind(dextra_address)
        .keepalive_interval(Duration::from_secs(3600))
        .cross_protocol_forwarding(true)
        .build()?;
    let reflector = Reflector::new_with_sockets(
        config,
        AllowAllAuthorizer,
        ProtocolSockets {
            dextra: Some(dextra_socket),
            dplus: Some(dplus_socket),
            dcs: Some(dcs_socket),
        },
    );
    let shutdown = reflector.shutdown_handle();
    let task = tokio::spawn(reflector.run());
    let dextra_client = UdpSocket::bind("127.0.0.1:0").await?;
    let dplus_client = UdpSocket::bind("127.0.0.1:0").await?;
    let dcs_client = UdpSocket::bind("127.0.0.1:0").await?;
    let mut bytes = [0; 600];
    let length = dextra::encode_connect_link(&mut bytes, CLIENT, Module::C)?;
    send(
        &dextra_client,
        bytes.get(..length).ok_or("DExtra LINK")?,
        dextra_address,
    )
    .await?;
    assert_eq!(
        receive(&dextra_client).await?.get(10..13),
        Some(b"ACK".as_slice())
    );
    let length = dplus::encode_link1(&mut bytes)?;
    send(
        &dplus_client,
        bytes.get(..length).ok_or("DPlus LINK1")?,
        dplus_address,
    )
    .await?;
    assert_eq!(receive(&dplus_client).await?.len(), 5);
    let length = dplus::encode_link2(&mut bytes, &Callsign::from_wire_bytes(*b"N0CALL  "))?;
    send(
        &dplus_client,
        bytes.get(..length).ok_or("DPlus LINK2")?,
        dplus_address,
    )
    .await?;
    assert_eq!(
        receive(&dplus_client).await?.get(4..8),
        Some(b"OKRW".as_slice())
    );
    let length = dcs::encode_connect_link(
        &mut bytes,
        CLIENT,
        Module::C,
        REFLECTOR,
        dcs::GatewayType::Repeater,
    )?;
    send(
        &dcs_client,
        bytes.get(..length).ok_or("DCS LINK")?,
        dcs_address,
    )
    .await?;
    assert_eq!(
        receive(&dcs_client).await?.get(10..13),
        Some(b"ACK".as_slice())
    );
    forward_burst(&dextra_client, dextra_address, &dplus_client, &dcs_client).await?;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    Ok(())
}

async fn forward_burst(
    source: &UdpSocket,
    destination: std::net::SocketAddr,
    dplus: &UdpSocket,
    dcs: &UdpSocket,
) -> Result<(), Box<dyn std::error::Error>> {
    let header = DstarHeader {
        flags: HeaderFlags::from_bytes([0; 3]),
        rpt2: Callsign::from_wire_bytes(*b"DCS030 G"),
        rpt1: Callsign::from_wire_bytes(*b"DCS030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"W1AW    "),
        my_suffix: Suffix::EMPTY,
    };
    let mut bytes = [0; 128];
    let length = dextra::encode_voice_header(&mut bytes, STREAM, &header)?;
    send(source, bytes.get(..length).ok_or("header")?, destination).await?;
    assert_eq!(receive(dplus).await?.len(), 58, "DPlus header framing");
    let mut scratch = [0; 128];
    assert!(
        tokio::time::timeout(Duration::from_millis(20), dcs.recv(&mut scratch))
            .await
            .is_err(),
        "DCS caches the header until actual voice arrives"
    );
    let frame = VoiceFrame {
        ambe: [0x11; 9],
        slow_data: [0x22; 3],
    };
    for sequence in [
        FrameSequence::ZERO,
        const { FrameSequence::new(1).unwrap() },
    ] {
        let length = dextra::encode_voice_data(&mut bytes, STREAM, sequence, &frame)?;
        send(source, bytes.get(..length).ok_or("voice")?, destination).await?;
        let dplus_frame = receive(dplus).await?;
        let dcs_frame = receive(dcs).await?;
        assert_eq!(dplus_frame.len(), 29, "DPlus data framing");
        assert_eq!(dcs_frame.len(), 100, "DCS data framing");
        assert_eq!(
            dplus_frame.get(17..26),
            Some(frame.ambe.as_slice()),
            "DPlus preserves AMBE"
        );
        assert_eq!(
            dcs_frame.get(46..55),
            Some(frame.ambe.as_slice()),
            "DCS preserves AMBE"
        );
    }
    let length =
        dextra::encode_voice_eot(&mut bytes, STREAM, const { FrameSequence::new(2).unwrap() })?;
    send(source, bytes.get(..length).ok_or("EOT")?, destination).await?;
    let dplus_end = receive(dplus).await?;
    let dcs_end = receive(dcs).await?;
    assert_eq!(dplus_end.len(), 32, "DPlus EOT framing");
    assert_eq!(dcs_end.len(), 100, "DCS EOT framing");
    assert_eq!(
        dcs_end.get(45),
        Some(&0x42),
        "DCS preserves final sequence and sets its EOT bit"
    );
    let mut scratch = [0; 128];
    assert!(
        tokio::time::timeout(Duration::from_millis(20), source.recv(&mut scratch))
            .await
            .is_err(),
        "source receives no echo"
    );
    Ok(())
}
