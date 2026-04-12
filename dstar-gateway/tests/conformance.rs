//! Replay captured reflector traffic through the parsers.
//!
//! Catches regressions in parser strictness. The real pcap corpus
//! lives in an external git submodule (see
//! `tests/conformance/corpus/README.md`). When the corpus directory is
//! empty or missing, each replay test is a no-op — this lets the test
//! suite stay green in CI without the corpus checked out.
//!
//! # Packet framing
//!
//! Each file in the corpus is a libpcap-format `.pcap` capture. The
//! replay walks every packet, strips the link-layer / IP / UDP
//! headers, and hands the UDP payload to the matching protocol's
//! lenient decoder. The decoder is allowed to fail — malformed
//! packets are expected — what matters is that it doesn't panic.
//!
//! Supported link layers:
//!
//! - Ethernet II (EN10MB) — IPv4 payload starts at offset 14
//! - BSD loopback (NULL) — IPv4 payload starts at offset 4
//! - Raw IPv4 — payload starts at offset 0
//!
//! IPv6 is not supported (reflector captures are IPv4 in practice).

use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::Path;

use dstar_gateway_core::codec::{dcs, dextra, dplus};
use dstar_gateway_core::validator::VecSink;
use pcap_parser::traits::PcapReaderIterator;
use pcap_parser::{LegacyPcapReader, PcapBlockOwned, PcapError};

use dstar_gateway as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

/// Maximum pcap block we'll buffer at once. `65_536` is enough for
/// any single Ethernet frame (max 1500 bytes of payload), with
/// headroom.
const PCAP_BUFFER_BYTES: usize = 65_536;

/// Walk every `.pcap` file under `dir` and hand each captured UDP
/// payload to `decoder`. Missing directories are silently skipped so
/// CI without the corpus stays green.
fn replay_dir(dir: &Path, mut decoder: impl FnMut(&[u8], &mut VecSink)) {
    if !dir.exists() {
        eprintln!(
            "conformance corpus not present at {}; skipping",
            dir.display()
        );
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot read corpus dir {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension() != Some(OsStr::new("pcap")) {
            continue;
        }
        let mut sink = VecSink::default();
        let count = replay_pcap_file(&path, |bytes| decoder(bytes, &mut sink));
        eprintln!(
            "replayed {} packets from {} ({} diagnostics)",
            count,
            path.display(),
            sink.len()
        );
    }
}

/// Open `path` as a legacy libpcap file and invoke `decoder` once per
/// UDP payload. Returns the number of UDP payloads successfully
/// extracted. Errors open/read the file, non-UDP packets, and packets
/// that don't parse as IPv4 are silently skipped.
fn replay_pcap_file<F>(path: &Path, mut decoder: F) -> usize
where
    F: FnMut(&[u8]),
{
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return 0;
        }
    };
    let mut reader = match LegacyPcapReader::new(PCAP_BUFFER_BYTES, file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot init pcap reader for {}: {e}", path.display());
            return 0;
        }
    };
    let mut count: usize = 0;
    loop {
        match reader.next() {
            Ok((offset, block)) => {
                if let PcapBlockOwned::Legacy(packet) = block
                    && let Some(udp_payload) = strip_to_udp_payload(packet.data)
                {
                    decoder(udp_payload);
                    count = count.saturating_add(1);
                }
                reader.consume(offset);
            }
            Err(PcapError::Eof) => break,
            Err(PcapError::Incomplete(_)) => {
                if reader.refill().is_err() {
                    break;
                }
            }
            Err(e) => {
                eprintln!("pcap read error in {}: {e}", path.display());
                break;
            }
        }
    }
    count
}

/// Strip Ethernet / BSD-loopback / raw link layers + IPv4 + UDP to
/// return a borrowed slice of just the UDP payload. Returns `None` for
/// any frame that isn't IPv4/UDP or is too short to parse.
fn strip_to_udp_payload(frame: &[u8]) -> Option<&[u8]> {
    // Heuristically detect where IPv4 starts by peeking at the version
    // nibble at the three most common offsets. The version nibble of
    // an IPv4 header is always 0x4, so we look for the first offset
    // that has `bytes[offset] >> 4 == 4`.
    let ip_offset = if frame.get(14).is_some_and(|b| b >> 4 == 4) {
        14 // Ethernet II + IPv4
    } else if frame.get(4).is_some_and(|b| b >> 4 == 4) {
        4 // BSD loopback (NULL link layer) + IPv4
    } else if frame.first().is_some_and(|b| b >> 4 == 4) {
        0 // Raw IPv4
    } else {
        return None;
    };

    let ip_header = frame.get(ip_offset..)?;

    // IP version must be 4 (we already checked, but re-verify for
    // clarity).
    if ip_header.first().map(|b| b >> 4) != Some(4) {
        return None;
    }

    // IP protocol must be UDP (17).
    if ip_header.get(9) != Some(&17) {
        return None;
    }

    // IP header length is in 32-bit words in the low nibble of byte 0.
    // Multiply by 4 to get bytes. Minimum legal header is 20 bytes (5
    // words); shorter than that is malformed.
    let ihl_words = ip_header.first()? & 0x0F;
    let ip_header_len = (ihl_words as usize).checked_mul(4)?;
    if ip_header_len < 20 {
        return None;
    }

    // UDP header is always 8 bytes. Skip it to reach the payload.
    let udp_header_offset = ip_offset.checked_add(ip_header_len)?;
    let udp_payload_offset = udp_header_offset.checked_add(8)?;
    frame.get(udp_payload_offset..)
}

#[test]
fn replay_dplus_corpus() {
    replay_dir(
        Path::new("tests/conformance/corpus/dplus"),
        |bytes, sink| {
            // A captured pcap may contain packets in either direction,
            // so try both decoders. Both are lenient — a wrong-direction
            // packet returns `Err`, which we drop.
            let _ = dplus::decode_server_to_client(bytes, sink);
            let _ = dplus::decode_client_to_server(bytes, sink);
        },
    );
}

#[test]
fn replay_dextra_corpus() {
    replay_dir(
        Path::new("tests/conformance/corpus/dextra"),
        |bytes, sink| {
            let _ = dextra::decode_server_to_client(bytes, sink);
            let _ = dextra::decode_client_to_server(bytes, sink);
        },
    );
}

#[test]
fn replay_dcs_corpus() {
    replay_dir(Path::new("tests/conformance/corpus/dcs"), |bytes, sink| {
        let _ = dcs::decode_server_to_client(bytes, sink);
        let _ = dcs::decode_client_to_server(bytes, sink);
    });
}

#[cfg(test)]
mod strip_tests {
    use super::strip_to_udp_payload;

    /// Build a minimal Ethernet + IPv4 + UDP frame with `payload` as
    /// the UDP body. Used to verify the stripper correctly unwraps
    /// each layer.
    fn ethernet_ipv4_udp(payload: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut frame = Vec::new();
        // Ethernet II: 14 bytes (dst MAC, src MAC, ethertype).
        frame.extend_from_slice(&[
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // dst
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, // src
            0x08, 0x00, // IPv4 ethertype
        ]);
        // IPv4 header: 20 bytes. Version=4, IHL=5.
        frame.push(0x45); // version + ihl
        frame.push(0x00); // tos
        let total_len = 20_u16 + 8 + u16::try_from(payload.len())?;
        frame.extend_from_slice(&total_len.to_be_bytes());
        frame.extend_from_slice(&[0, 0]); // id
        frame.extend_from_slice(&[0, 0]); // flags + frag
        frame.push(64); // ttl
        frame.push(17); // protocol = UDP
        frame.extend_from_slice(&[0, 0]); // checksum
        frame.extend_from_slice(&[10, 0, 0, 1]); // src ip
        frame.extend_from_slice(&[10, 0, 0, 2]); // dst ip
        // UDP header: 8 bytes.
        frame.extend_from_slice(&[0x4E, 0x21]); // src port 20001
        frame.extend_from_slice(&[0x4E, 0x22]); // dst port 20002
        let udp_len = 8_u16 + u16::try_from(payload.len())?;
        frame.extend_from_slice(&udp_len.to_be_bytes());
        frame.extend_from_slice(&[0, 0]); // checksum
        frame.extend_from_slice(payload);
        Ok(frame)
    }

    /// BSD loopback frame: 4-byte AF indicator + IPv4 + UDP + payload.
    fn bsd_loopback_ipv4_udp(payload: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut frame = vec![0x02, 0x00, 0x00, 0x00]; // AF_INET
        frame.push(0x45); // version + ihl
        frame.push(0x00); // tos
        let total_len = 20_u16 + 8 + u16::try_from(payload.len())?;
        frame.extend_from_slice(&total_len.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&[0, 0]);
        frame.push(64);
        frame.push(17);
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&[127, 0, 0, 1]);
        frame.extend_from_slice(&[127, 0, 0, 1]);
        frame.extend_from_slice(&[0x4E, 0x21]);
        frame.extend_from_slice(&[0x4E, 0x22]);
        let udp_len = 8_u16 + u16::try_from(payload.len())?;
        frame.extend_from_slice(&udp_len.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(payload);
        Ok(frame)
    }

    #[test]
    fn ethernet_ipv4_udp_strips_to_payload() -> Result<(), Box<dyn std::error::Error>> {
        let payload = b"hello";
        let frame = ethernet_ipv4_udp(payload)?;
        let stripped = strip_to_udp_payload(&frame).ok_or("strip returned None")?;
        assert_eq!(stripped, payload);
        Ok(())
    }

    #[test]
    fn bsd_loopback_strips_to_payload() -> Result<(), Box<dyn std::error::Error>> {
        let payload = b"world";
        let frame = bsd_loopback_ipv4_udp(payload)?;
        let stripped = strip_to_udp_payload(&frame).ok_or("strip returned None")?;
        assert_eq!(stripped, payload);
        Ok(())
    }

    #[test]
    fn too_short_frame_returns_none() {
        assert!(strip_to_udp_payload(&[]).is_none());
        assert!(strip_to_udp_payload(&[0x00, 0x01]).is_none());
    }

    #[test]
    fn non_udp_ipv4_returns_none() -> Result<(), Box<dyn std::error::Error>> {
        let mut frame = ethernet_ipv4_udp(b"x")?;
        // Flip the protocol byte from 17 (UDP) to 6 (TCP).
        if let Some(b) = frame.get_mut(14 + 9) {
            *b = 6;
        }
        assert!(strip_to_udp_payload(&frame).is_none());
        Ok(())
    }
}
