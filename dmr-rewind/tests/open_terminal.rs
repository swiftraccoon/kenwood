// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Loopback tests for the Tokio Open DMR Terminal client.

use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use dmr_rewind::{Client, ClientConfig, Error as ClientError, Event, EventMetadata, Subscription};
use dmr_rewind_core::{
    Callsign, FullLinkControl, Packet, PacketFlags, PacketType, Payload, SERVICE_OPEN_TERMINAL,
    SessionType, Subscription as WireSubscription, SuperHeader, VersionData, authentication_digest,
    decode, encode,
};
use thiserror as _;
use tokio::net::UdpSocket;
use tokio::time::{sleep, timeout};

type TestError = Box<dyn Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;

const DMR_ID: u32 = 1_234_567;
const PASSWORD: &str = "correct horse battery staple";
const CHALLENGE: [u8; 4] = [0x10, 0x32, 0x54, 0x76];
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const VOICE_HEADER: [u8; 12] = [
    0x00, 0x01, 0x02, 0x00, 0x00, 0x5b, 0x12, 0xd6, 0x87, 0xaa, 0xbb, 0xcc,
];
const TERMINATOR: [u8; 12] = [
    0x00, 0x01, 0x82, 0x00, 0x00, 0x5b, 0x12, 0xd6, 0x87, 0xdd, 0xee, 0xff,
];
const AUDIO: [u8; 27] = [0xa5; 27];
const EMBEDDED: [u8; 10] = [0x5a; 10];

#[derive(Debug)]
struct EventDatagrams {
    voice_header: Vec<u8>,
    audio: Vec<u8>,
    embedded: Vec<u8>,
    super_header: Vec<u8>,
    report: Vec<u8>,
    failure: Vec<u8>,
    busy: Vec<u8>,
    unknown: Vec<u8>,
    terminator: Vec<u8>,
    empty_terminator: Vec<u8>,
    close: Vec<u8>,
}

#[derive(Debug)]
struct CallStartDatagrams {
    voice_header: Vec<u8>,
    audio: Vec<u8>,
    embedded: Vec<u8>,
    super_header: Vec<u8>,
}

#[derive(Debug)]
struct NoticeDatagrams {
    report: Vec<u8>,
    failure: Vec<u8>,
    busy: Vec<u8>,
    unknown: Vec<u8>,
}

#[derive(Debug)]
struct CallEndDatagrams {
    terminator: Vec<u8>,
    empty_terminator: Vec<u8>,
    close: Vec<u8>,
}

async fn receive_packet(socket: &UdpSocket) -> TestResult<(Packet, SocketAddr)> {
    let mut buffer = [0_u8; 2_048];
    let (received, peer) = timeout(IO_TIMEOUT, socket.recv_from(&mut buffer)).await??;
    let datagram = buffer
        .get(..received)
        .ok_or_else(|| io::Error::other("fake-server receive overflow"))?;
    Ok((decode(datagram)?, peer))
}

async fn send_packet(
    socket: &UdpSocket,
    peer: SocketAddr,
    packet_type: PacketType,
    flags: PacketFlags,
    sequence: u32,
    payload: Payload,
) -> TestResult<Vec<u8>> {
    let packet = Packet::new(packet_type, flags, sequence, payload)?;
    let datagram = encode(&packet)?;
    let sent = socket.send_to(&datagram, peer).await?;
    if sent != datagram.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "fake server sent a partial UDP datagram",
        )
        .into());
    }
    Ok(datagram)
}

async fn accept_handshake(
    socket: &UdpSocket,
    subscriptions: &[WireSubscription],
    version_ack: bool,
    echo_subscription: bool,
) -> TestResult<SocketAddr> {
    let peer = receive_initial_keepalive(socket).await?;
    accept_authentication(socket, peer, version_ack).await?;
    accept_subscriptions(socket, peer, subscriptions, echo_subscription).await?;
    Ok(peer)
}

async fn receive_initial_keepalive(socket: &UdpSocket) -> TestResult<SocketAddr> {
    let (keepalive, peer) = receive_packet(socket).await?;
    assert_eq!(
        keepalive.header.packet_type,
        PacketType::KeepAlive,
        "Open Terminal must start with a keepalive"
    );
    assert_eq!(
        keepalive.header.sequence, 0,
        "first client sequence must be zero"
    );
    let Payload::KeepAlive(Some(version)) = keepalive.payload else {
        return Err(io::Error::other("initial keepalive did not contain version data").into());
    };
    assert_eq!(
        version.remote_id, DMR_ID,
        "keepalive must use the configured DMR ID"
    );
    assert_eq!(
        version.service, SERVICE_OPEN_TERMINAL,
        "client must select Open DMR Terminal service 0x21"
    );
    assert!(
        !version.description.is_empty(),
        "client description must be present"
    );
    Ok(peer)
}

async fn accept_authentication(
    socket: &UdpSocket,
    peer: SocketAddr,
    version_ack: bool,
) -> TestResult {
    let _challenge_datagram = send_packet(
        socket,
        peer,
        PacketType::Challenge,
        PacketFlags::NONE,
        100,
        Payload::Challenge(CHALLENGE.to_vec()),
    )
    .await?;

    let (authentication, authentication_peer) = receive_packet(socket).await?;
    assert_eq!(
        authentication_peer, peer,
        "authentication peer must remain stable"
    );
    assert_eq!(
        authentication.header.packet_type,
        PacketType::Authentication,
        "challenge must be followed by authentication"
    );
    assert_eq!(
        authentication.header.sequence, 1,
        "authentication must use the next routine sequence"
    );
    assert_eq!(
        authentication.payload,
        Payload::Authentication(authentication_digest(&CHALLENGE, PASSWORD.as_bytes(),)),
        "authentication digest must be SHA-256(challenge || password)"
    );

    let acknowledgement = if version_ack {
        Payload::KeepAlive(Some(VersionData {
            remote_id: 9_999_999,
            service: SERVICE_OPEN_TERMINAL,
            description: b"version-bearing acknowledgement".to_vec(),
        }))
    } else {
        Payload::KeepAlive(None)
    };
    let _acknowledgement_datagram = send_packet(
        socket,
        peer,
        PacketType::KeepAlive,
        PacketFlags::NONE,
        101,
        acknowledgement,
    )
    .await?;
    Ok(())
}

async fn accept_subscriptions(
    socket: &UdpSocket,
    peer: SocketAddr,
    subscriptions: &[WireSubscription],
    echo_subscription: bool,
) -> TestResult {
    for (position, expected) in subscriptions.iter().enumerate() {
        let (subscription, subscription_peer) = receive_packet(socket).await?;
        assert_eq!(
            subscription_peer, peer,
            "subscription peer must remain stable"
        );
        assert_eq!(
            subscription.header.packet_type,
            PacketType::Subscription,
            "Open Terminal must subscribe directly without Configuration"
        );
        assert_eq!(
            subscription.payload,
            Payload::Subscription(Some(*expected)),
            "subscription order and payload must be preserved"
        );
        let sequence_offset = u32::try_from(position)?;
        assert_eq!(
            subscription.header.sequence,
            2_u32.saturating_add(sequence_offset),
            "client control sequence must increase"
        );
        let acknowledgement = if echo_subscription {
            Payload::Subscription(Some(*expected))
        } else {
            Payload::Subscription(None)
        };
        let _acknowledgement_datagram = send_packet(
            socket,
            peer,
            PacketType::Subscription,
            PacketFlags::NONE,
            102_u32.saturating_add(sequence_offset),
            acknowledgement,
        )
        .await?;
    }
    Ok(())
}

async fn send_call_start(socket: &UdpSocket, peer: SocketAddr) -> TestResult<CallStartDatagrams> {
    let realtime = PacketFlags::from_bits(0x8001);
    let voice_header = send_packet(
        socket,
        peer,
        PacketType::DmrVoiceHeader,
        realtime,
        500,
        Payload::DmrVoiceHeader(FullLinkControl::from_bytes(VOICE_HEADER)),
    )
    .await?;
    let audio = send_packet(
        socket,
        peer,
        PacketType::DmrAudio(4),
        realtime,
        501,
        Payload::DmrAudio(AUDIO),
    )
    .await?;
    let embedded = send_packet(
        socket,
        peer,
        PacketType::DmrEmbeddedData,
        realtime,
        502,
        Payload::DmrEmbeddedData(EMBEDDED),
    )
    .await?;
    let super_header_value = SuperHeader {
        session_type: SessionType::GroupVoice,
        source_id: DMR_ID,
        target_id: 91,
        source_call: Callsign::from_raw(*b"N0CALL\0\0\0\0"),
        target_call: Callsign::from_raw(*b"TG91\0\0\0\0\0\0"),
    };
    let super_header = send_packet(
        socket,
        peer,
        PacketType::SuperHeader,
        PacketFlags::NONE,
        503,
        Payload::SuperHeader(super_header_value),
    )
    .await?;
    Ok(CallStartDatagrams {
        voice_header,
        audio,
        embedded,
        super_header,
    })
}

async fn send_notices(socket: &UdpSocket, peer: SocketAddr) -> TestResult<NoticeDatagrams> {
    let report = send_packet(
        socket,
        peer,
        PacketType::Report,
        PacketFlags::NONE,
        504,
        Payload::Report(b"connected".to_vec()),
    )
    .await?;
    let failure = send_packet(
        socket,
        peer,
        PacketType::Failure,
        PacketFlags::NONE,
        505,
        Payload::Failure(b"late warning".to_vec()),
    )
    .await?;
    let busy = send_packet(
        socket,
        peer,
        PacketType::BusyNotice,
        PacketFlags::NONE,
        506,
        Payload::Busy(b"busy".to_vec()),
    )
    .await?;
    let unknown = send_packet(
        socket,
        peer,
        PacketType::Unknown(0x7f00),
        PacketFlags::from_bits(0x4000),
        507,
        Payload::Opaque(vec![1, 3, 3, 7]),
    )
    .await?;
    Ok(NoticeDatagrams {
        report,
        failure,
        busy,
        unknown,
    })
}

async fn send_call_end(socket: &UdpSocket, peer: SocketAddr) -> TestResult<CallEndDatagrams> {
    let realtime = PacketFlags::from_bits(0x8001);
    let terminator = send_packet(
        socket,
        peer,
        PacketType::DmrTerminator,
        realtime,
        508,
        Payload::DmrTerminator(Some(FullLinkControl::from_bytes(TERMINATOR))),
    )
    .await?;
    let empty_terminator = send_packet(
        socket,
        peer,
        PacketType::DmrTerminator,
        realtime,
        509,
        Payload::DmrTerminator(None),
    )
    .await?;
    let close = send_packet(
        socket,
        peer,
        PacketType::Close,
        PacketFlags::NONE,
        510,
        Payload::Close,
    )
    .await?;
    Ok(CallEndDatagrams {
        terminator,
        empty_terminator,
        close,
    })
}

async fn serve_event_stream(socket: UdpSocket) -> TestResult<EventDatagrams> {
    let subscriptions = [
        WireSubscription {
            session_type: SessionType::GroupVoice,
            target: 91,
        },
        WireSubscription {
            session_type: SessionType::PrivateVoice,
            target: DMR_ID,
        },
    ];
    let peer = accept_handshake(&socket, &subscriptions, true, true).await?;
    let start = send_call_start(&socket, peer).await?;
    let notices = send_notices(&socket, peer).await?;
    let end = send_call_end(&socket, peer).await?;
    Ok(EventDatagrams {
        voice_header: start.voice_header,
        audio: start.audio,
        embedded: start.embedded,
        super_header: start.super_header,
        report: notices.report,
        failure: notices.failure,
        busy: notices.busy,
        unknown: notices.unknown,
        terminator: end.terminator,
        empty_terminator: end.empty_terminator,
        close: end.close,
    })
}

async fn connect(server: SocketAddr, subscriptions: Vec<Subscription>) -> TestResult<Client> {
    let config =
        ClientConfig::new(server, DMR_ID, PASSWORD.to_owned()).with_subscriptions(subscriptions);
    Ok(timeout(IO_TIMEOUT, Client::connect(config)).await??)
}

async fn next_event(client: &mut Client) -> TestResult<Event> {
    Ok(timeout(IO_TIMEOUT, client.next_event()).await??)
}

fn assert_metadata(
    metadata: &EventMetadata,
    sequence: u32,
    flags: PacketFlags,
    raw_datagram: &[u8],
) {
    assert_eq!(
        metadata.sequence, sequence,
        "event must retain transport sequence"
    );
    assert_eq!(metadata.flags, flags, "event must retain all flag bits");
    assert_eq!(
        metadata.raw_datagram, raw_datagram,
        "event must retain the exact UDP datagram"
    );
}

async fn assert_call_start_events(client: &mut Client, datagrams: &EventDatagrams) -> TestResult {
    let realtime = PacketFlags::from_bits(0x8001);
    let Event::VoiceHeader(event) = next_event(client).await? else {
        return Err(io::Error::other("expected voice-header event").into());
    };
    assert_eq!(
        event.data, VOICE_HEADER,
        "voice header must retain exact bytes"
    );
    assert_eq!(
        event.link_control,
        FullLinkControl::from_bytes(VOICE_HEADER),
        "voice header must expose parsed Full Link Control"
    );
    assert_eq!(event.subtype, 1, "voice-header subtype must be one");
    assert_metadata(&event.metadata, 500, realtime, &datagrams.voice_header);

    let Event::Audio(event) = next_event(client).await? else {
        return Err(io::Error::other("expected audio event").into());
    };
    assert_eq!(event.data, AUDIO, "audio must retain all 27 bytes");
    assert_eq!(event.subtype, 4, "audio subtype must be retained");
    assert_metadata(&event.metadata, 501, realtime, &datagrams.audio);

    let Event::EmbeddedData(event) = next_event(client).await? else {
        return Err(io::Error::other("expected embedded-data event").into());
    };
    assert_eq!(
        event.data, EMBEDDED,
        "embedded data must retain all ten bytes"
    );
    assert_eq!(event.subtype, 7, "embedded-data subtype must be seven");
    assert_metadata(&event.metadata, 502, realtime, &datagrams.embedded);

    let Event::SuperHeader(event) = next_event(client).await? else {
        return Err(io::Error::other("expected superheader event").into());
    };
    assert_eq!(
        event.data.source_id, DMR_ID,
        "superheader source must be parsed"
    );
    assert_eq!(
        event.data.target_id, 91,
        "superheader destination must be parsed"
    );
    assert_metadata(
        &event.metadata,
        503,
        PacketFlags::NONE,
        &datagrams.super_header,
    );
    Ok(())
}

async fn assert_notice_events(client: &mut Client, datagrams: &EventDatagrams) -> TestResult {
    let Event::Report(event) = next_event(client).await? else {
        return Err(io::Error::other("expected report event").into());
    };
    assert_eq!(event.data, b"connected", "report bytes must be retained");
    assert_metadata(&event.metadata, 504, PacketFlags::NONE, &datagrams.report);

    let Event::Failure(event) = next_event(client).await? else {
        return Err(io::Error::other("expected runtime failure event").into());
    };
    assert_eq!(
        event.data, b"late warning",
        "runtime failure must be delivered instead of failing the client"
    );
    assert_metadata(&event.metadata, 505, PacketFlags::NONE, &datagrams.failure);

    let Event::BusyNotice(event) = next_event(client).await? else {
        return Err(io::Error::other("expected busy-notice event").into());
    };
    assert_eq!(event.data, b"busy", "busy-notice bytes must be retained");
    assert_metadata(&event.metadata, 506, PacketFlags::NONE, &datagrams.busy);

    let Event::Other(event) = next_event(client).await? else {
        return Err(io::Error::other("expected unknown event").into());
    };
    assert_eq!(
        event.packet_type,
        PacketType::Unknown(0x7f00),
        "unknown packet type must be retained"
    );
    assert_eq!(
        event.payload,
        Payload::Opaque(vec![1, 3, 3, 7]),
        "unknown packet payload must be retained"
    );
    assert_metadata(
        &event.metadata,
        507,
        PacketFlags::from_bits(0x4000),
        &datagrams.unknown,
    );
    Ok(())
}

async fn assert_call_end_events(client: &mut Client, datagrams: &EventDatagrams) -> TestResult {
    let realtime = PacketFlags::from_bits(0x8001);
    let Event::Terminator(event) = next_event(client).await? else {
        return Err(io::Error::other("expected terminator event").into());
    };
    assert_eq!(
        event.data,
        Some(TERMINATOR),
        "terminator must retain exact bytes"
    );
    assert_eq!(
        event.link_control,
        Some(FullLinkControl::from_bytes(TERMINATOR)),
        "terminator must expose parsed Full Link Control"
    );
    assert_eq!(event.subtype, 2, "terminator subtype must be two");
    assert_metadata(&event.metadata, 508, realtime, &datagrams.terminator);

    let Event::Terminator(event) = next_event(client).await? else {
        return Err(io::Error::other("expected empty terminator event").into());
    };
    assert_eq!(
        event.data, None,
        "empty terminator must not synthesize Full Link Control bytes"
    );
    assert_eq!(
        event.link_control, None,
        "empty terminator must not synthesize parsed fields"
    );
    assert_eq!(
        event.metadata.payload_len, 0,
        "empty terminator must retain its zero payload length"
    );
    assert_metadata(&event.metadata, 509, realtime, &datagrams.empty_terminator);

    let Event::Close(metadata) = next_event(client).await? else {
        return Err(io::Error::other("expected remote close event").into());
    };
    assert_metadata(&metadata, 510, PacketFlags::NONE, &datagrams.close);
    assert!(
        matches!(client.next_event().await, Err(ClientError::ClientClosed)),
        "receive after remote close must report ClientClosed"
    );
    Ok(())
}

#[tokio::test]
async fn authenticates_subscribes_and_delivers_typed_events() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_event_stream(socket));
    let subscriptions = vec![Subscription::group(91), Subscription::private(DMR_ID)];
    let config = ClientConfig::new(server_address, DMR_ID, PASSWORD.to_owned())
        .with_subscriptions(subscriptions);
    let config_debug = format!("{config:?}");
    assert!(
        !config_debug.contains(PASSWORD),
        "configuration Debug must redact credentials"
    );
    assert!(
        config_debug.contains("[REDACTED]"),
        "configuration Debug must identify the redaction"
    );

    let mut client = timeout(IO_TIMEOUT, Client::connect(config)).await??;
    let client_debug = format!("{client:?}");
    assert!(
        !client_debug.contains(PASSWORD),
        "client Debug must redact credentials"
    );
    let datagrams = server_task.await??;
    assert_call_start_events(&mut client, &datagrams).await?;
    assert_notice_events(&mut client, &datagrams).await?;
    assert_call_end_events(&mut client, &datagrams).await?;
    Ok(())
}

async fn serve_invalid_challenge(socket: UdpSocket) -> TestResult {
    let (keepalive, peer) = receive_packet(&socket).await?;
    assert_eq!(
        keepalive.header.packet_type,
        PacketType::KeepAlive,
        "client must start with keepalive"
    );
    let _datagram = send_packet(
        &socket,
        peer,
        PacketType::Challenge,
        PacketFlags::NONE,
        1,
        Payload::Challenge(vec![1, 2, 3]),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn rejects_non_four_byte_challenge() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_invalid_challenge(socket));
    let result = timeout(
        IO_TIMEOUT,
        Client::connect(ClientConfig::new(
            server_address,
            DMR_ID,
            PASSWORD.to_owned(),
        )),
    )
    .await?;
    assert!(
        matches!(
            result,
            Err(ClientError::InvalidChallengeLength { actual: 3 })
        ),
        "three-byte challenge must be rejected clearly"
    );
    server_task.await??;
    Ok(())
}

async fn serve_mismatched_initial_subscription_ack(socket: UdpSocket) -> TestResult {
    let first = WireSubscription {
        session_type: SessionType::GroupVoice,
        target: 91,
    };
    let second = WireSubscription {
        session_type: SessionType::PrivateVoice,
        target: DMR_ID,
    };
    let peer = receive_initial_keepalive(&socket).await?;
    accept_authentication(&socket, peer, false).await?;

    let (first_request, first_peer) = receive_packet(&socket).await?;
    assert_eq!(
        first_peer, peer,
        "first subscription peer must remain stable"
    );
    assert_eq!(
        first_request.payload,
        Payload::Subscription(Some(first)),
        "client must request the first configured subscription"
    );
    let _first_ack = send_packet(
        &socket,
        peer,
        PacketType::Subscription,
        PacketFlags::NONE,
        110,
        Payload::Subscription(Some(first)),
    )
    .await?;

    let (second_request, second_peer) = receive_packet(&socket).await?;
    assert_eq!(
        second_peer, peer,
        "second subscription peer must remain stable"
    );
    assert_eq!(
        second_request.payload,
        Payload::Subscription(Some(second)),
        "client must request the second configured subscription"
    );
    let _duplicate_first_ack = send_packet(
        &socket,
        peer,
        PacketType::Subscription,
        PacketFlags::NONE,
        111,
        Payload::Subscription(Some(first)),
    )
    .await?;
    let _failure = send_packet(
        &socket,
        peer,
        PacketType::Failure,
        PacketFlags::NONE,
        112,
        Payload::Failure(b"second subscription missing".to_vec()),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn ignores_mismatched_initial_subscription_ack() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_mismatched_initial_subscription_ack(socket));
    let config = ClientConfig::new(server_address, DMR_ID, PASSWORD.to_owned())
        .with_subscriptions(vec![Subscription::group(91), Subscription::private(DMR_ID)]);

    let result = timeout(IO_TIMEOUT, Client::connect(config)).await?;
    assert!(
        matches!(
            result,
            Err(ClientError::HandshakeRejected { message })
                if message == "second subscription missing"
        ),
        "a duplicate first acknowledgement must not complete the second subscription"
    );
    server_task.await??;
    Ok(())
}

async fn serve_mismatched_runtime_subscription_ack(socket: UdpSocket) -> TestResult {
    let first = WireSubscription {
        session_type: SessionType::GroupVoice,
        target: 91,
    };
    let second = WireSubscription {
        session_type: SessionType::PrivateVoice,
        target: DMR_ID,
    };
    let peer = accept_handshake(&socket, &[first, second], false, true).await?;
    let runtime_challenge = [0xde, 0xad, 0xbe, 0xef];
    let _challenge = send_packet(
        &socket,
        peer,
        PacketType::Challenge,
        PacketFlags::NONE,
        210,
        Payload::Challenge(runtime_challenge.to_vec()),
    )
    .await?;
    let (authentication, authentication_peer) = receive_packet(&socket).await?;
    assert_eq!(
        authentication_peer, peer,
        "runtime authentication peer must remain stable"
    );
    assert_eq!(
        authentication.payload,
        Payload::Authentication(authentication_digest(
            &runtime_challenge,
            PASSWORD.as_bytes(),
        )),
        "runtime authentication must answer the fresh challenge"
    );
    let _authentication_ack = send_packet(
        &socket,
        peer,
        PacketType::KeepAlive,
        PacketFlags::NONE,
        211,
        Payload::KeepAlive(None),
    )
    .await?;

    let (first_request, first_peer) = receive_packet(&socket).await?;
    assert_eq!(
        first_peer, peer,
        "first restored subscription peer must remain stable"
    );
    assert_eq!(
        first_request.payload,
        Payload::Subscription(Some(first)),
        "runtime recovery must reinstall the first subscription"
    );
    let _first_ack = send_packet(
        &socket,
        peer,
        PacketType::Subscription,
        PacketFlags::NONE,
        212,
        Payload::Subscription(Some(first)),
    )
    .await?;

    let (second_request, second_peer) = receive_packet(&socket).await?;
    assert_eq!(
        second_peer, peer,
        "second restored subscription peer must remain stable"
    );
    assert_eq!(
        second_request.payload,
        Payload::Subscription(Some(second)),
        "runtime recovery must reinstall the second subscription"
    );
    let _duplicate_first_ack = send_packet(
        &socket,
        peer,
        PacketType::Subscription,
        PacketFlags::NONE,
        213,
        Payload::Subscription(Some(first)),
    )
    .await?;
    let _failure = send_packet(
        &socket,
        peer,
        PacketType::Failure,
        PacketFlags::NONE,
        214,
        Payload::Failure(b"runtime subscription missing".to_vec()),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn ignores_mismatched_runtime_subscription_ack() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_mismatched_runtime_subscription_ack(socket));
    let mut client = connect(
        server_address,
        vec![Subscription::group(91), Subscription::private(DMR_ID)],
    )
    .await?;

    let result = timeout(IO_TIMEOUT, client.next_event()).await?;
    assert!(
        matches!(
            result,
            Err(ClientError::HandshakeRejected { message })
                if message == "runtime subscription missing"
        ),
        "a stale runtime acknowledgement must not complete the outstanding subscription"
    );
    server_task.await??;
    Ok(())
}

async fn serve_reauthentication(socket: UdpSocket) -> TestResult {
    let subscription = WireSubscription {
        session_type: SessionType::GroupVoice,
        target: 91,
    };
    let peer = accept_handshake(&socket, &[subscription], false, false).await?;
    let second_challenge = [0xde, 0xad, 0xbe, 0xef];
    let _challenge = send_packet(
        &socket,
        peer,
        PacketType::Challenge,
        PacketFlags::NONE,
        200,
        Payload::Challenge(second_challenge.to_vec()),
    )
    .await?;
    let (authentication, _) = receive_packet(&socket).await?;
    assert_eq!(
        authentication.payload,
        Payload::Authentication(authentication_digest(
            &second_challenge,
            PASSWORD.as_bytes(),
        )),
        "runtime challenge must use a fresh digest"
    );
    sleep(Duration::from_millis(100)).await;
    let _authentication_ack = send_packet(
        &socket,
        peer,
        PacketType::KeepAlive,
        PacketFlags::NONE,
        201,
        Payload::KeepAlive(None),
    )
    .await?;
    let (reinstalled, _) = receive_packet(&socket).await?;
    assert_eq!(
        reinstalled.payload,
        Payload::Subscription(Some(subscription)),
        "reauthentication must reinstall subscriptions"
    );
    let _subscription_ack = send_packet(
        &socket,
        peer,
        PacketType::Subscription,
        PacketFlags::NONE,
        202,
        Payload::Subscription(Some(subscription)),
    )
    .await?;
    let _audio = send_packet(
        &socket,
        peer,
        PacketType::DmrAudio(0),
        PacketFlags::REAL_TIME_1,
        203,
        Payload::DmrAudio(AUDIO),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn reauthenticates_and_reinstalls_subscriptions() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_reauthentication(socket));
    let mut client = connect(server_address, vec![Subscription::group(91)]).await?;
    let interrupted_reauthentication = client.next_event_for(Duration::from_millis(50)).await?;
    assert!(
        interrupted_reauthentication.is_none(),
        "bounded wait must persist an in-progress authentication phase"
    );
    let event = client.next_event_for(IO_TIMEOUT).await?;
    let Some(Event::Audio(audio)) = event else {
        return Err(io::Error::other("expected audio after transparent reauthentication").into());
    };
    assert_eq!(audio.data, AUDIO, "audio after reauth must be delivered");
    server_task.await??;
    Ok(())
}

async fn serve_bounded_wait(socket: UdpSocket) -> TestResult {
    let peer = accept_handshake(&socket, &[], false, false).await?;
    let (keepalive, keepalive_peer) = receive_packet(&socket).await?;
    assert_eq!(
        keepalive_peer, peer,
        "periodic keepalive peer must remain stable"
    );
    assert!(
        matches!(keepalive.payload, Payload::KeepAlive(Some(_))),
        "periodic keepalive must carry Open Terminal version data"
    );
    sleep(Duration::from_millis(80)).await;
    let _report = send_packet(
        &socket,
        peer,
        PacketType::Report,
        PacketFlags::NONE,
        300,
        Payload::Report(b"after deadline".to_vec()),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn bounded_wait_services_keepalive_and_preserves_next_event() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_bounded_wait(socket));
    let config = ClientConfig::new(server_address, DMR_ID, PASSWORD.to_owned())
        .with_keepalive_interval(Duration::from_millis(20))
        .with_timeout(Duration::from_millis(500));
    let mut client = timeout(IO_TIMEOUT, Client::connect(config)).await??;
    let before_report = client.next_event_for(Duration::from_millis(50)).await?;
    assert!(
        before_report.is_none(),
        "bounded wait must return None at its own deadline"
    );
    let Some(Event::Report(report)) = client.next_event_for(Duration::from_millis(200)).await?
    else {
        return Err(io::Error::other("subsequent wait must deliver the report").into());
    };
    assert_eq!(
        report.data, b"after deadline",
        "event after a bounded timeout must not be lost"
    );
    server_task.await??;
    Ok(())
}

async fn serve_silent_session(socket: UdpSocket) -> TestResult {
    let _peer = accept_handshake(&socket, &[], false, false).await?;
    sleep(Duration::from_millis(150)).await;
    Ok(())
}

#[tokio::test]
async fn reports_session_timeout_despite_periodic_keepalives() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_silent_session(socket));
    let session_timeout = Duration::from_millis(80);
    let config = ClientConfig::new(server_address, DMR_ID, PASSWORD.to_owned())
        .with_keepalive_interval(Duration::from_millis(20))
        .with_timeout(session_timeout);
    let mut client = timeout(IO_TIMEOUT, Client::connect(config)).await??;
    let result = timeout(IO_TIMEOUT, client.next_event()).await?;
    assert!(
        matches!(
            result,
            Err(ClientError::SessionTimeout { timeout })
                if timeout == session_timeout
        ),
        "outbound keepalives must not conceal a silent server"
    );
    server_task.await??;
    Ok(())
}

async fn serve_graceful_close(socket: UdpSocket) -> TestResult {
    let peer = accept_handshake(&socket, &[], false, false).await?;
    let (close, close_peer) = receive_packet(&socket).await?;
    assert_eq!(close_peer, peer, "close peer must remain stable");
    assert_eq!(
        close.header.packet_type,
        PacketType::Close,
        "graceful shutdown must send Close"
    );
    assert_eq!(
        close.payload,
        Payload::Close,
        "Close must have an empty payload"
    );
    Ok(())
}

#[tokio::test]
async fn graceful_close_is_idempotent() -> TestResult {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let server_address = socket.local_addr()?;
    let server_task = tokio::spawn(serve_graceful_close(socket));
    let mut client = connect(server_address, Vec::new()).await?;
    client.close().await?;
    client.close().await?;
    assert!(
        matches!(client.next_event().await, Err(ClientError::ClientClosed)),
        "closed client must reject further receive calls"
    );
    server_task.await??;
    Ok(())
}
