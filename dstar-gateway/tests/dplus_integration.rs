//! `DPlus` end-to-end integration tests using the `FakeReflector` harness.

mod fake_reflector;

use dstar_gateway::protocol::dplus::DPlusClient;
use dstar_gateway::{Callsign, Module};
use fake_reflector::FakeReflector;
use std::time::Duration;

fn cs(s: &str) -> Callsign {
    Callsign::try_from_str(s).expect("valid test callsign")
}

fn m(c: char) -> Module {
    Module::try_from_char(c).expect("valid test module")
}

#[tokio::test]
async fn dplus_two_step_login_succeeds() {
    let fake = FakeReflector::spawn_dplus("REF001", 'C').await;
    let mut client = DPlusClient::new(cs("W1AW"), m('C'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
}

#[tokio::test]
async fn dplus_keepalive_echoes() {
    let fake = FakeReflector::spawn_dplus("REF001", 'C').await;
    let mut client = DPlusClient::new(cs("W1AW"), m('C'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    for _ in 0..10 {
        let _ = client.poll().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn dplus_connect_and_wait_returns_rejected_on_nak() {
    // Per ref/ircDDBGateway/Common/ConnectData.cpp:251-259 and
    // ref/xlxd/src/cdplusprotocol.cpp:535-544, a reflector that
    // refuses the LINK2 login replies with
    // `[0x08, 0xC0, 0x04, 0x00, 'B', 'U', 'S', 'Y']` instead of
    // `[..., 'O', 'K', 'R', 'W']`. Before F3 the client classified
    // every 8-byte non-DSVT reply as Connected and only surfaced
    // the rejection after the 30-second keepalive timeout as a
    // misleading ConnectTimeout. After F3 it must surface
    // Error::Rejected immediately.
    let fake = FakeReflector::spawn_dplus_rejecting("REF001", 'C').await;
    let mut client = DPlusClient::new(cs("W1AW"), m('C'), fake.local_addr())
        .await
        .unwrap();
    let result = client.connect_and_wait(Duration::from_secs(2)).await;
    assert!(
        matches!(result, Err(dstar_gateway::Error::Rejected)),
        "DPlus BUSY reply should surface as Error::Rejected, got {result:?}"
    );
}

#[tokio::test]
async fn dplus_disconnect_retransmits_three_times() {
    let fake = FakeReflector::spawn_dplus("REF001", 'C').await;
    let mut client = DPlusClient::new(cs("W1AW"), m('C'), fake.local_addr())
        .await
        .unwrap();
    client
        .connect_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    client.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.disconnects_received().await, 3, "C12 retx");
}

/// Regression for the live REF030 C disconnect bug (B3).
///
/// Before this fix [`DPlusClient::poll`] only refreshed
/// `last_poll_received` on the `PollEcho` arm, so a reflector streaming
/// voice frames for ≥30 s without interleaving poll echoes silently
/// tripped `POLL_TIMEOUT` and forced the client to
/// `ConnectionState::Disconnected` mid-transmission. ircDDBGateway
/// (`Common/DPlusHandler.cpp:603`, `:633`, `:661`) treats every inbound
/// packet as keepalive evidence; we now do the same.
///
/// The test drives the client through a full LINK1 / LINK2 / OKRW
/// handshake against a minimal hand-rolled reflector stub (the shared
/// `FakeReflector` harness only reacts to incoming packets and cannot
/// generate unsolicited voice frames), then fires a burst of `DPlus`
/// voice-data packets while wall-clock time advances past a shortened
/// `poll_timeout`. The client must stay `Connected` and must not emit
/// `ReflectorEvent::Disconnected`.
///
/// The `poll_timeout` is shortened from the 30 s default to 400 ms via
/// [`DPlusClient::set_poll_timeout`] so the test can run in real time
/// without sleeping for half a minute. The production default is
/// verified separately in [`dstar_gateway::protocol::dplus::POLL_TIMEOUT`].
#[tokio::test]
async fn dplus_voice_burst_keeps_connection_alive_past_poll_timeout() {
    use dstar_gateway::protocol::{ConnectionState, ReflectorEvent};
    use tokio::net::UdpSocket;

    // Minimal reflector stub: LINK1 → echo, LINK2 → OKRW. Runs in a
    // background task so the client's `connect_and_wait` sees replies,
    // then hands the socket + client address back to the test so the
    // test can push voice frames directly without the stub interfering.
    let refl = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let refl_addr = refl.local_addr().unwrap();
    let (client_addr_tx, client_addr_rx) = tokio::sync::oneshot::channel();

    let handshake_task = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, src)) = refl.recv_from(&mut buf).await else {
                return;
            };
            match n {
                5 if buf[4] == 0x01 => {
                    // Echo LINK1 as the step-1 ACK.
                    let _ = refl.send_to(&buf[..5], src).await;
                }
                28 => {
                    // Send OKRW accept.
                    let reply: [u8; 8] = [0x08, 0xC0, 0x04, 0x00, b'O', b'K', b'R', b'W'];
                    let _ = refl.send_to(&reply, src).await;
                    let _ = client_addr_tx.send((refl, src));
                    return;
                }
                _ => {}
            }
        }
    });

    let mut client = DPlusClient::new(cs("W1AW"), m('C'), refl_addr)
        .await
        .unwrap();
    // Shorten the keepalive timeout so the test exercises the
    // timeout-trip path in real wall-clock time. 400 ms is long enough
    // for the handshake to complete cleanly but short enough that the
    // voice burst below easily outruns the buggy behaviour.
    client.set_poll_timeout(Duration::from_millis(400));
    client
        .connect_and_wait(Duration::from_secs(2))
        .await
        .expect("handshake");
    assert_eq!(client.state(), ConnectionState::Connected);

    let (refl, client_addr) = client_addr_rx.await.expect("client addr");
    let _ = handshake_task.await;

    // Build a DPlus voice data packet (29 bytes: 0x1D 0x80 + 27-byte
    // DSVT voice frame). Stream ID = 0x1234, seq = 0 .. 0x3F (no EOT).
    let make_voice = |seq: u8| -> Vec<u8> {
        let mut pkt = vec![0u8; 29];
        pkt[0] = 0x1D;
        pkt[1] = 0x80;
        pkt[2..6].copy_from_slice(b"DSVT");
        // DSVT body starts at pkt[2]. Flag at pkt[6] = 0x20 (voice,
        // not header — header would be 0x10).
        pkt[6] = 0x20;
        // stream_id little-endian at dsvt[12..14] → pkt[14..16]. Any
        // non-zero value works; parse_packet rejects stream_id == 0.
        pkt[14] = 0x34;
        pkt[15] = 0x12;
        // seq at dsvt[14] → pkt[16]; masked to 0..=0x3F to avoid the
        // EOT flag (0x40) that would surface as VoiceEnd.
        pkt[16] = seq & 0x3F;
        // Remaining 9 bytes AMBE + 3 bytes slow data left as zeros.
        pkt
    };

    // Stream voice frames for ~1.2 s — 3 × the shortened 400 ms
    // poll_timeout. Before the fix, the lack of poll echoes while voice
    // is flowing would trip the timeout after the first 400 ms; after
    // the fix, every inbound voice frame refreshes `last_poll_received`
    // and the connection stays alive.
    let start = std::time::Instant::now();
    let mut got_disconnected = false;
    let mut voice_received = 0usize;
    let mut seq: u8 = 0;
    while start.elapsed() < Duration::from_millis(1_200) {
        let pkt = make_voice(seq);
        seq = seq.wrapping_add(1);
        let _ = refl.send_to(&pkt, client_addr).await.unwrap();
        // 20 ms is the real DPlus voice cadence.
        tokio::time::sleep(Duration::from_millis(20)).await;
        if let Ok(Some(evt)) = client.poll().await {
            if matches!(evt, ReflectorEvent::Disconnected) {
                got_disconnected = true;
                break;
            }
            if matches!(evt, ReflectorEvent::VoiceData { .. }) {
                voice_received += 1;
            }
        }
    }

    assert!(
        !got_disconnected,
        "client dropped the link mid-voice-burst — poll_timeout fired \
         despite continuous inbound traffic (regression of B3)",
    );
    assert_eq!(
        client.state(),
        ConnectionState::Connected,
        "client should still be Connected after a 1.2 s voice burst \
         with a 400 ms poll_timeout",
    );
    assert!(
        voice_received > 0,
        "should have observed at least one VoiceData event — test is \
         otherwise meaningless",
    );
}
