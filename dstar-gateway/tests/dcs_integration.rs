//! DCS end-to-end integration tests using the `FakeReflector` harness.

mod fake_reflector;

use dstar_gateway::protocol::dcs::DcsClient;
use dstar_gateway::voice::VoiceFrame;
use dstar_gateway::{Callsign, DStarHeader, Module, StreamId, Suffix};
use fake_reflector::FakeReflector;
use std::time::Duration;

fn cs(s: &str) -> Callsign {
    Callsign::try_from_str(s).expect("valid test callsign")
}

fn m(c: char) -> Module {
    Module::try_from_char(c).expect("valid test module")
}

const fn sid(n: u16) -> StreamId {
    match StreamId::new(n) {
        Some(s) => s,
        None => panic!("non-zero test stream id"),
    }
}

#[tokio::test]
async fn dcs_connect_and_wait_returns_rejected_on_nak() {
    // Integration counterpart to dextra/dplus rejection tests: verify
    // that a DCS reflector sending a 14-byte NAK frame in response to
    // the 519-byte connect request surfaces as `Error::Rejected` on
    // `DcsClient::connect_and_wait`.
    let fake = FakeReflector::spawn_dcs_rejecting("DCS001", 'C').await;
    let mut client = DcsClient::new(
        Callsign::try_from_str("W1AW").unwrap(),
        Module::try_from_char('A').unwrap(),
        Callsign::try_from_str("DCS001").unwrap(),
        Module::try_from_char('C').unwrap(),
        fake.local_addr(),
    )
    .await
    .unwrap();
    let result = client.connect_and_wait(Duration::from_secs(2)).await;
    assert!(
        matches!(result, Err(dstar_gateway::Error::Rejected)),
        "DCS NAK should surface as Error::Rejected, got {result:?}"
    );
}

#[tokio::test]
async fn dcs_connect_and_wait_succeeds() {
    let fake = FakeReflector::spawn_dcs("DCS001", 'C').await;
    let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
}

#[tokio::test]
async fn dcs_rpt_seq_distinct_between_header_and_voice() {
    let fake = FakeReflector::spawn_dcs("DCS001", 'C').await;
    let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(2))
        .await
        .unwrap();

    let hdr = DStarHeader {
        flag1: 0x00,
        flag2: 0x00,
        flag3: 0x00,
        rpt2: cs("DCS001 G"),
        rpt1: cs("DCS001 C"),
        ur_call: cs("CQCQCQ"),
        my_call: cs("W1AW"),
        my_suffix: Suffix::EMPTY,
    };
    let frame = VoiceFrame::silence();

    client.send_header(&hdr, sid(0x1234)).await.unwrap();
    client.send_voice(sid(0x1234), 1, &frame).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    let frames = fake.voice_frames().await;
    assert_eq!(
        frames.len(),
        2,
        "exactly one header packet and one voice packet"
    );
    assert_eq!(frames[0].len(), 100, "DCS header packet is 100 bytes");
    assert_eq!(frames[1].len(), 100, "DCS voice packet is 100 bytes");

    // rpt_seq is a 24-bit little-endian field at bytes 58..61 of the DCS
    // voice packet. Verified by reading `build_voice` in
    // `dstar-gateway/src/protocol/dcs.rs` (lines 235-238), which writes:
    //     pkt[58] = (rpt_seq & 0xFF) as u8;
    //     pkt[59] = ((rpt_seq >> 8) & 0xFF) as u8;
    //     pkt[60] = ((rpt_seq >> 16) & 0xFF) as u8;
    // This matches xlxd `CDcsProtocol::EncodeDvFramePacket` layout.
    let header_rpt_seq = &frames[0][58..61];
    let voice_rpt_seq = &frames[1][58..61];
    assert_ne!(
        header_rpt_seq, voice_rpt_seq,
        "header and voice must use distinct rpt_seq values (C7 regression)"
    );
}
