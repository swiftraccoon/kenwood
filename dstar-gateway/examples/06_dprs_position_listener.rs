#![cfg(feature = "examples-network")]
//! Receive checked DPRS positions from complete CR-terminated slow-data lines.
//!
//! Run with `DSTAR_CALLSIGN` and `REFLECTOR_HOST` (host:port):
//! ```text
//! cargo run -p dstar-gateway --example 06_dprs_position_listener --features examples-network
//! ```
//! The listener retains one active stream and at most 300 sentence bytes.
//! Fragment loss resets partial data; malformed/oversized lines are discarded.

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::session::client::{DPlus, Event, Session};
use dstar_gateway_core::slowdata::{SlowDataAssembler, SlowDataBlock};
use dstar_gateway_core::{DprsReport, FrameSequence, Module, RoutingCallsign};
use tokio::net::UdpSocket;

use pcap_parser as _;
use thiserror as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let callsign = RoutingCallsign::new(&env::var("DSTAR_CALLSIGN")?)?;
    let host = env::var("REFLECTOR_HOST")?;
    let hosts = AuthClient::new().authenticate(callsign.callsign()).await?;
    let peer = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(host))
        .await??
        .next()
        .ok_or("reflector hostname resolved to no addresses")?;
    let bind = if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = Arc::new(UdpSocket::bind(bind).await?);
    let connecting = Session::<DPlus, _>::builder()
        .callsign(callsign)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer(peer)
        .build()
        .authenticate(hosts)?
        .connect(Instant::now())?;
    let connected = drive_connecting(connecting, &socket, Duration::from_secs(5)).await?;
    let mut client = AsyncSession::spawn(connected, socket);
    listen(&mut client, Duration::from_secs(60)).await;
    client.disconnect().await?;
    Ok(())
}

/// Receive one active stream until the requested local deadline.
async fn listen(client: &mut AsyncSession<DPlus>, duration: Duration) {
    let stop = tokio::time::sleep(duration);
    tokio::pin!(stop);
    let mut collector = GpsStream::default();
    let mut active = None;
    loop {
        tokio::select! {
            () = &mut stop => break,
            event = client.next_event() => match event {
                Some(Event::VoiceFrame { stream_id, seq, frame }) => {
                    if active != Some(stream_id) {
                        collector = GpsStream::default();
                        active = Some(stream_id);
                    }
                    if let Some(report) = collector.push(frame.slow_data, seq) {
                        tracing::info!(
                            source = %report.source(),
                            latitude_degrees = %report.latitude(),
                            longitude_degrees = %report.longitude(),
                            symbol = ?report.symbol(),
                            comment = %report.comment(),
                            "DPRS position"
                        );
                    }
                }
                Some(Event::VoiceEnd { stream_id, .. }) if active == Some(stream_id) => {
                    collector = GpsStream::default();
                    active = None;
                }
                Some(_) => {}
                None => break,
            }
        }
    }
}

/// Bounded sentence reconstruction for sequential received slow-data blocks.
#[derive(Debug, Default)]
struct GpsStream {
    assembler: SlowDataAssembler,
    sentence: Vec<u8>,
    discarding: bool,
    expected: Option<FrameSequence>,
}

impl GpsStream {
    fn push(&mut self, bytes: [u8; 3], raw_sequence: u8) -> Option<DprsReport> {
        let Some(index) = FrameSequence::new(raw_sequence) else {
            *self = Self::default();
            return None;
        };
        let missed = self.expected.is_some_and(|expected| expected != index);
        let needs_first_half = self.expected.is_none() || missed;
        if missed {
            *self = Self::default();
        }
        self.expected = Some(index.next());
        if needs_first_half && index.get().is_multiple_of(2) && index != FrameSequence::ZERO {
            return None;
        }
        let Some(SlowDataBlock::Gps(fragment)) = self.assembler.push(bytes, raw_sequence) else {
            return None;
        };
        self.append(&fragment)
    }

    fn append(&mut self, fragment: &[u8]) -> Option<DprsReport> {
        let mut completed = None;
        for &byte in fragment {
            if byte == b'\r' {
                if !self.discarding {
                    self.sentence.push(byte);
                    completed = parse_line(&self.sentence);
                }
                self.sentence.clear();
                self.discarding = false;
            } else if !self.discarding {
                if self.sentence.len() >= dstar_gateway_core::dprs::MAX_SENTENCE_BYTES - 1 {
                    self.sentence.clear();
                    self.discarding = true;
                    tracing::debug!("DPRS sentence exceeded the local byte limit");
                } else {
                    self.sentence.push(byte);
                }
            }
        }
        completed
    }
}

fn parse_line(bytes: &[u8]) -> Option<DprsReport> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, "DPRS sentence is not UTF-8");
            return None;
        }
    };
    match text.parse() {
        Ok(report) => Some(report),
        Err(error) => {
            tracing::debug!(%error, "DPRS sentence rejected");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::{
        DprsComment, DprsPosition, DprsSource, DprsSymbol, Latitude, Longitude, SymbolCode,
        SymbolTable,
    };

    #[test]
    fn fragments_form_one_checked_sentence_and_oversize_recovers_at_cr()
    -> Result<(), Box<dyn std::error::Error>> {
        let report = DprsReport::new(
            DprsSource::new("W1AW")?,
            DprsPosition {
                latitude: Latitude::new(35.5)?,
                longitude: Longitude::new(-82.55)?,
            },
            DprsSymbol::new(SymbolTable::Primary, SymbolCode::new('>')?),
            DprsComment::new("test")?,
        );
        let sentence = report.to_sentence();
        let mut collector = GpsStream::default();
        let mut completed = Vec::new();
        for fragment in sentence.as_bytes().chunks(5) {
            completed.extend(collector.append(fragment));
        }
        assert_eq!(
            completed,
            [report.clone()],
            "GPS blocks must accumulate through CR"
        );
        let mut wire_collector = GpsStream::default();
        let mut wire_reports = Vec::new();
        let mut sequence = FrameSequence::ZERO;
        for fragment in sentence.as_bytes().chunks(5) {
            let mut block = [0_u8; 6];
            *block.first_mut().ok_or("empty GPS block")? = 0x30 | u8::try_from(fragment.len())?;
            block
                .get_mut(1..1 + fragment.len())
                .ok_or("GPS payload range")?
                .copy_from_slice(fragment);
            for half in block.chunks_exact(3) {
                if sequence == FrameSequence::ZERO {
                    assert!(wire_collector.push([0x55, 0x2D, 0x16], 0).is_none());
                    sequence = sequence.next();
                }
                let plain = <[u8; 3]>::try_from(half)?;
                let scrambled = [plain[0] ^ 0x70, plain[1] ^ 0x4F, plain[2] ^ 0x93];
                wire_reports.extend(wire_collector.push(scrambled, sequence.get()));
                sequence = sequence.next();
            }
        }
        assert_eq!(
            wire_reports,
            [report.clone()],
            "paired wire fragments span sync slots"
        );
        assert!(collector.append(&vec![b'x'; 301]).is_none());
        assert!(
            collector.sentence.is_empty(),
            "oversized bytes must not remain retained"
        );
        assert!(collector.append(b"\r").is_none());
        assert_eq!(
            collector.append(sentence.as_bytes()),
            Some(report),
            "CR restores line alignment"
        );
        Ok(())
    }
}
