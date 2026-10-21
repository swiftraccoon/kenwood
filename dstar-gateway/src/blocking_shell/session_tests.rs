//! Loopback tests for the synchronous driver.

use super::*;
use dstar_gateway_core::codec::dextra::{self, ClientPacket};
use dstar_gateway_core::{Callsign, DExtra, HeaderFlags, Module, RoutingCallsign, Suffix};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn connected()
-> Result<(BlockingSession<DExtra>, UdpSocket), Box<dyn std::error::Error + Send + Sync>> {
    let peer = UdpSocket::bind("127.0.0.1:0")?;
    peer.set_read_timeout(Some(Duration::from_secs(1)))?;
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    let now = Instant::now();
    let mut session = Session::<DExtra, _>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer.local_addr()?)
        .build()
        .connect(now)?;
    let _link = session.poll_transmit(now).ok_or("missing LINK")?;
    session.handle_input(now, peer.local_addr()?, b"W1AW    BCACK\0")?;
    let session = session.promote()?;
    Ok((BlockingSession::new(session, socket)?, peer))
}

#[test]
fn queued_events_precede_reads_and_idle_is_bounded() -> TestResult {
    let (mut client, _peer) = connected()?;
    let start = Instant::now();
    let event = client.run_until_event()?;
    assert!(matches!(event, Some(Event::Connected { .. })), "{event:?}");
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "queued event waited for socket I/O"
    );
    let start = Instant::now();
    assert!(
        client.run_until_event()?.is_none(),
        "a quiet link should be idle"
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "idle read waited for a distant protocol timer"
    );
    assert!(!client.is_closed(), "idle does not mean closure");
    Ok(())
}

#[test]
fn transmission_and_extraction_preserve_header_frame_and_eot() -> TestResult {
    let (mut client, peer) = connected()?;
    let header = DstarHeader {
        flags: HeaderFlags::NONE,
        rpt2: Callsign::from_wire_bytes(*b"XRF030 G"),
        rpt1: Callsign::from_wire_bytes(*b"XRF030 C"),
        ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
        my_call: Callsign::from_wire_bytes(*b"W1AW    "),
        my_suffix: Suffix::EMPTY,
    };
    let stream = StreamId::try_from(0x1234_u16)?;
    client.send_header(&header, stream)?;
    client.send_voice(stream, FrameSequence::ZERO, &VoiceFrame::silence())?;
    client.send_eot(stream, FrameSequence::ZERO.next())?;
    let mut packets = Vec::new();
    for _ in 0..3 {
        let mut bytes = [0_u8; 100];
        let (length, _) = peer.recv_from(&mut bytes)?;
        packets.push(dextra::decode_client_to_server(
            bytes.get(..length).ok_or("invalid datagram length")?,
            &mut dstar_gateway_core::validator::NullSink,
        )?);
    }
    assert!(
        matches!(packets.as_slice(), [
        ClientPacket::VoiceHeader { header: received, .. },
        ClientPacket::VoiceData { seq: 0, .. },
        ClientPacket::VoiceEot { seq: 0x41, .. },
    ] if received == &header),
        "{packets:?}"
    );
    let (session, socket) = client.into_parts();
    assert_eq!(session.state_kind(), ClientStateKind::Connected);
    assert_eq!(socket.local_addr()?.ip(), peer.local_addr()?.ip());
    Ok(())
}

#[test]
fn disconnect_observes_the_correlated_terminal_reply() -> TestResult {
    let (mut client, peer) = connected()?;
    let responder = std::thread::spawn(move || -> std::io::Result<()> {
        let mut request = [0_u8; 32];
        let (length, source) = peer.recv_from(&mut request)?;
        assert_eq!(request.get(..length), Some(b"W1AW    B \0".as_slice()));
        let _sent = peer.send_to(b"W1AW    B NAK\0", source)?;
        Ok(())
    });
    client.disconnect()?;
    assert!(client.is_closed());
    responder
        .join()
        .map_err(|_| "responder thread panicked")??;
    client.disconnect()?;
    Ok(())
}
