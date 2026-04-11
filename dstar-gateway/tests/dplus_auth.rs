//! Integration tests for [`DPlusClient::authenticate`] against a
//! hand-built TCP fake.
//!
//! The real `DPlus` auth server lives at `auth.dstargateway.org:20001`
//! and is only reachable from hosts registered on the D-STAR network.
//! These tests spawn a local `TcpListener` bound to `127.0.0.1:0` and
//! use [`DPlusClient::set_auth_endpoint`] to redirect the next
//! `authenticate()` call at that fake, so the TCP read-loop, chunk
//! framing, and 26-byte host-record decoding can all be exercised
//! hermetically (no network, no DNS).
//!
//! Wire format, per `ref/ircDDBGateway/Common/DPlusAuthenticator.cpp`
//! (lines 151–192 in the reference tree) — see also
//! [`dstar_gateway::parse_auth_response`]:
//!
//! ```text
//! Each chunk:
//!   [0]      low byte of chunk length
//!   [1]      high nibble = flags (top two bits must be 0b11);
//!            low nibble = high byte of chunk length
//!   [2]      packet type, must be 0x01
//!   [3..8]   5 bytes of chunk-header filler (ignored)
//!   [8..len] repeated 26-byte host records
//!
//!   chunk_len = (buf[1] & 0x0F) * 256 + buf[0]
//!
//! Each host record (26 bytes):
//!   [ 0..16] IPv4 address as ASCII text, SPACE-padded
//!   [16..24] reflector callsign, SPACE-padded ASCII (LONG_CALLSIGN_LENGTH = 8)
//!   [24]     module/id byte (ignored)
//!   [25]     top bit (0x80) = active flag; records with the bit
//!            clear are filtered out by the parser
//!
//! The server may emit multiple chunks back-to-back and closes the
//! TCP connection once the full list has been transmitted.
//! ```

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use dstar_gateway::protocol::dplus::DPlusClient;
use dstar_gateway::{Callsign, Error, Module};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 56-byte auth request length the real server expects from the
/// client, per `DPlusAuthenticator.cpp`. Our fake consumes and
/// discards exactly this many bytes before writing its response.
const AUTH_REQUEST_LEN: usize = 56;

/// Build a single 26-byte host record in the on-wire layout.
///
/// * `[0..16]`  — IPv4 dotted-quad as ASCII, space-padded to 16 bytes
/// * `[16..24]` — 8-byte callsign, space-padded on the right
/// * `[24]`     — module/id byte, left zero
/// * `[25]`     — active flag; `0x80` bit set for active records
fn build_host_record(callsign: &str, ip: Ipv4Addr) -> [u8; 26] {
    // Space-fill everything first to mirror the reference server's
    // `::memset(buffer, ' ', ...)` pattern. The parser trims spaces
    // and nulls from both fields.
    let mut rec = [b' '; 26];

    // IP field: ASCII dotted-quad, space-padded.
    let ip_str = ip.to_string();
    let ip_bytes = ip_str.as_bytes();
    assert!(ip_bytes.len() <= 16, "IPv4 text must fit in 16 bytes");
    rec[..ip_bytes.len()].copy_from_slice(ip_bytes);
    for slot in rec.iter_mut().take(16).skip(ip_bytes.len()) {
        *slot = b' ';
    }

    // Callsign field: ASCII, space-padded to 8 bytes.
    let cs_bytes = callsign.as_bytes();
    assert!(cs_bytes.len() <= 8, "callsign must fit in 8 bytes");
    rec[16..16 + cs_bytes.len()].copy_from_slice(cs_bytes);

    // Module/id byte (index 24) is ignored by the parser.
    rec[24] = 0;
    // Active flag (top bit of index 25).
    rec[25] = 0x80;

    rec
}

/// Wrap a set of host records in a single `DPlus` auth chunk header.
///
/// Matches [`CDPlusAuthenticator::authenticate`] on the server side:
/// `len = (buf[1] & 0x0F) * 256 + buf[0]`, byte 1's top two bits must
/// be `0b11`, byte 2 must be `0x01`, and records start at offset 8.
fn build_chunk(records: &[[u8; 26]]) -> Vec<u8> {
    let len = 8 + records.len() * 26;
    assert!(len <= 0x0FFF, "test chunk too large to encode");
    let mut out = Vec::with_capacity(len);
    #[allow(clippy::cast_possible_truncation)]
    let lo = (len & 0xFF) as u8;
    #[allow(clippy::cast_possible_truncation)]
    let hi = ((len >> 8) & 0x0F) as u8;
    out.push(lo);
    out.push(0xC0 | hi);
    out.push(0x01);
    // Five bytes of chunk-header filler (ignored by the parser).
    out.extend_from_slice(&[0u8; 5]);
    for r in records {
        out.extend_from_slice(r);
    }
    assert_eq!(out.len(), len);
    out
}

/// Spawn a one-shot TCP fake that accepts a single client, consumes
/// the 56-byte auth request, then streams `payload` back and closes.
///
/// Returns the `SocketAddr` of the fake and a join handle so tests
/// can await the server task (useful when diagnosing flakes but not
/// required for test correctness).
async fn spawn_auth_fake(payload: Vec<u8>) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0 for auth fake");
    let addr = listener.local_addr().expect("local_addr on bound listener");

    let task = tokio::spawn(async move {
        let Ok((mut sock, _peer)) = listener.accept().await else {
            return;
        };
        // Read and discard the client's auth request. We don't
        // validate its contents — the point of this test is the
        // client-side read/parse path, not the request builder
        // (which `parse_auth_response` unit tests cover separately).
        let mut req = [0u8; AUTH_REQUEST_LEN];
        let _ = sock.read_exact(&mut req).await;

        // Stream the synthetic response.
        let _ = sock.write_all(&payload).await;
        let _ = sock.shutdown().await;
    });

    (addr, task)
}

/// Spawn a TCP fake that accepts a connection and then immediately
/// closes without sending anything. Used to exercise the "truncated
/// header" error path.
async fn spawn_immediate_close_fake() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0 for immediate-close fake");
    let addr = listener.local_addr().expect("local_addr on bound listener");

    let task = tokio::spawn(async move {
        if let Ok((mut sock, _peer)) = listener.accept().await {
            // Drain (but don't require) the client's auth request,
            // then close. Some tests may send the full 56 bytes
            // before the close is observed; either way the server
            // writes nothing.
            let mut req = [0u8; AUTH_REQUEST_LEN];
            let _ = sock.read_exact(&mut req).await;
            let _ = sock.shutdown().await;
        }
    });

    (addr, task)
}

fn test_callsign() -> Callsign {
    Callsign::try_from_str("W1AW").expect("W1AW is a valid callsign")
}

fn test_module() -> Module {
    Module::try_from_char('C').expect("'C' is a valid module")
}

/// Build a new client pointed at a throwaway UDP remote (the
/// reflector remote address is not exercised by `authenticate()` —
/// auth is TCP and independent of the UDP socket). Returns the
/// client with `set_auth_endpoint(Some(fake))` already applied.
async fn client_for_fake(fake: SocketAddr) -> DPlusClient {
    // Any routable-looking address works for the UDP remote; the
    // auth path never sends UDP.
    let dummy_remote: SocketAddr = "127.0.0.1:20001"
        .parse()
        .expect("literal SocketAddr parses");
    let mut client = DPlusClient::new(test_callsign(), test_module(), dummy_remote)
        .await
        .expect("bind local UDP socket for DPlusClient");
    client.set_auth_endpoint(Some(fake));
    client
}

#[tokio::test]
async fn dplus_authenticate_parses_three_hosts() {
    // Single chunk carrying three active REF host records.
    let payload = build_chunk(&[
        build_host_record("REF001", Ipv4Addr::new(10, 0, 0, 1)),
        build_host_record("REF002", Ipv4Addr::new(10, 0, 0, 2)),
        build_host_record("REF003", Ipv4Addr::new(10, 0, 0, 3)),
    ]);

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    client
        .authenticate()
        .await
        .expect("authenticate should succeed against fake");

    let hosts = client.auth_hosts();
    assert_eq!(hosts.len(), 3, "expected three parsed hosts");

    let ref001 = hosts.find("REF001").expect("REF001 present");
    assert_eq!(ref001.callsign, "REF001");
    assert_eq!(
        ref001.address,
        std::net::IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
    );

    let ref002 = hosts.find("REF002").expect("REF002 present");
    assert_eq!(
        ref002.address,
        std::net::IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))
    );

    let ref003 = hosts.find("REF003").expect("REF003 present");
    assert_eq!(
        ref003.address,
        std::net::IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3))
    );
}

#[tokio::test]
async fn dplus_authenticate_accepts_empty_response() {
    // Fake accepts, consumes the request, and immediately closes.
    // The real server does the same thing when it has nothing to
    // report (or for unregistered callsigns): a zero-byte response.
    // The reference C++ parser tolerates this and returns an empty
    // host list, so we do the same.
    let (addr, _task) = spawn_immediate_close_fake().await;
    let mut client = client_for_fake(addr).await;

    client
        .authenticate()
        .await
        .expect("empty response should parse as zero hosts");
    assert!(client.auth_hosts().is_empty());
}

#[tokio::test]
async fn dplus_authenticate_rejects_malformed_chunk_type() {
    // A single chunk header with a bogus type byte (expected 0x01)
    // should be rejected rather than silently returning an empty
    // list — the client is talking to something that isn't the
    // DPlus auth server.
    let mut payload = build_chunk(&[build_host_record("REF001", Ipv4Addr::new(10, 0, 0, 1))]);
    payload[2] = 0x02;

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    let result = client.authenticate().await;
    assert!(
        matches!(result, Err(Error::AuthResponseInvalid(_))),
        "expected AuthResponseInvalid, got {result:?}"
    );
}

#[tokio::test]
async fn dplus_authenticate_handles_zero_records() {
    // A framed chunk with no records is valid and yields zero hosts.
    let payload = build_chunk(&[]);

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    client
        .authenticate()
        .await
        .expect("authenticate should succeed with empty host list");

    let hosts = client.auth_hosts();
    assert_eq!(hosts.len(), 0, "expected zero parsed hosts");
    assert!(hosts.is_empty());
}

#[tokio::test]
async fn dplus_authenticate_case_insensitive_lookup() {
    let payload = build_chunk(&[build_host_record("REF001", Ipv4Addr::new(10, 0, 0, 1))]);

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    client
        .authenticate()
        .await
        .expect("authenticate should succeed");

    let hosts = client.auth_hosts();
    // Parser stores the callsign as sent (uppercase) but `find` is
    // documented to be case-insensitive, so all of these must hit.
    assert!(hosts.find("REF001").is_some(), "uppercase lookup");
    assert!(hosts.find("ref001").is_some(), "lowercase lookup");
    assert!(hosts.find("Ref001").is_some(), "mixed-case lookup");
    assert!(hosts.find("REF999").is_none(), "missing callsign is None");
}

/// Regression test for the per-read timeout in the auth read loop.
/// The real server closes the TCP connection once it has flushed the
/// host list; this test verifies the client correctly terminates its
/// read loop on the resulting EOF instead of hanging.
#[tokio::test]
async fn dplus_authenticate_terminates_cleanly_on_eof() {
    let payload = build_chunk(&[build_host_record("REF030", Ipv4Addr::new(192, 0, 2, 30))]);

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    // If the read loop were to hang we'd exhaust this outer timeout
    // and the test would fail with a timeout error rather than the
    // authenticate result.
    let result = tokio::time::timeout(Duration::from_secs(5), client.authenticate()).await;
    assert!(result.is_ok(), "authenticate did not terminate on EOF");
    result
        .expect("outer timeout")
        .expect("authenticate returned Err");
    assert_eq!(client.auth_hosts().len(), 1);
}

/// Multi-chunk response: real servers split large host lists across
/// several framed chunks, and the client must consume the entire
/// stream, not just the first chunk.
#[tokio::test]
async fn dplus_authenticate_parses_multiple_chunks() {
    let mut payload = build_chunk(&[build_host_record("REF001", Ipv4Addr::new(10, 0, 0, 1))]);
    payload.extend_from_slice(&build_chunk(&[
        build_host_record("REF002", Ipv4Addr::new(10, 0, 0, 2)),
        build_host_record("REF003", Ipv4Addr::new(10, 0, 0, 3)),
    ]));

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    client
        .authenticate()
        .await
        .expect("multi-chunk response should parse");

    let hosts = client.auth_hosts();
    assert_eq!(hosts.len(), 3);
    assert!(hosts.find("REF001").is_some());
    assert!(hosts.find("REF002").is_some());
    assert!(hosts.find("REF003").is_some());
}

/// Sanity check against a hand-built record that mirrors the exact
/// wire layout `CDPlusAuthenticator::authenticate` reads: space-padded
/// ASCII IP in `[0..16]`, space-padded ASCII callsign in `[16..24]`,
/// module byte in `[24]`, active flag in the top bit of `[25]`.
#[tokio::test]
async fn dplus_authenticate_matches_reference_byte_layout() {
    // Build a record the way the C++ reference emits it on the
    // server side: ::memset(buffer, ' ', ...) then fill fields.
    let mut rec = [b' '; 26];
    rec[..11].copy_from_slice(b"172.16.0.30"); // 11-char dotted quad
    // bytes 11..16 left as spaces (padding to 16)
    rec[16..22].copy_from_slice(b"REF030"); // 6-char callsign
    // bytes 22..24 left as spaces (padding to 24)
    rec[24] = 0; // module/id byte
    rec[25] = 0x80; // active

    let payload = build_chunk(&[rec]);

    let (addr, _task) = spawn_auth_fake(payload).await;
    let mut client = client_for_fake(addr).await;

    client
        .authenticate()
        .await
        .expect("reference-layout record should parse");

    let hosts = client.auth_hosts();
    assert_eq!(hosts.len(), 1);
    let ref030 = hosts.find("REF030").expect("REF030 present");
    assert_eq!(ref030.callsign, "REF030");
    assert_eq!(
        ref030.address,
        std::net::IpAddr::V4(Ipv4Addr::new(172, 16, 0, 30))
    );
}
