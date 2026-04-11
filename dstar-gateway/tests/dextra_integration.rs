//! `DExtra` end-to-end integration tests using the `FakeReflector` harness.

mod fake_reflector;

use dstar_gateway::protocol::dextra::DExtraClient;
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
async fn dextra_connect_and_wait_succeeds_against_fake() {
    let fake = FakeReflector::spawn_dextra("XRF001", 'A').await;
    let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(1))
        .await
        .unwrap();
    // Give the fake task time to drain any packet still in flight after the
    // client returns from connect_and_wait (the second retransmission may not
    // have reached the fake's recv loop yet).
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.connects_received().await, 2); // C10 retx
}

#[tokio::test]
async fn dextra_connect_retransmission_survives_one_drop() {
    let fake = FakeReflector::spawn_dextra("XRF001", 'A').await;
    fake.drop_next_n(1);
    let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn dextra_disconnect_timeout_transitions_to_disconnected() {
    let fake = FakeReflector::spawn_dextra("XRF001", 'A').await;
    let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(1))
        .await
        .unwrap();
    // Instruct the fake to drop every future packet so the disconnect
    // ACK never comes back. The client must time out on its own and
    // surface a Disconnected event.
    fake.drop_next_n(100);
    client.disconnect().await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if matches!(
            client.poll().await,
            Ok(Some(dstar_gateway::ReflectorEvent::Disconnected))
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("expected Disconnected event within timeout");
}

#[tokio::test]
async fn dextra_send_header_retransmits_five_times() {
    let fake = FakeReflector::spawn_dextra("XRF001", 'A').await;
    let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(1))
        .await
        .unwrap();

    let hdr = DStarHeader {
        flag1: 0x00,
        flag2: 0x00,
        flag3: 0x00,
        rpt2: cs("XRF001 G"),
        rpt1: cs("XRF001 A"),
        ur_call: cs("CQCQCQ"),
        my_call: cs("W1AW"),
        my_suffix: Suffix::EMPTY,
    };
    client.send_header(&hdr, sid(0x1234)).await.unwrap();

    tokio::time::sleep(Duration::from_millis(400)).await;
    let frames = fake.voice_frames().await;
    assert_eq!(frames.len(), 5, "header should be sent 5x per C10");
}
