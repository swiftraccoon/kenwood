//! Integration tests for TN, DC, RT protocol commands.
//!
//! Hardware-verified on D75:
//! - TN: TNC mode (bare read only, returns mode and packet data rate)
//! - DC: D-STAR callsign slots 1-6 (slot-indexed read)
//! - RT: Real-time clock (bare read, returns `YYMMDDHHmmss`)
//!
//! The D75 RE originally identified these as tone commands, but hardware
//! testing confirmed the actual semantics documented here.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::{DstarCallsign, DstarSlot, DstarSuffix, PacketDataRate, TncMode};

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// TN -- TNC Mode (bare read only)
// ============================================================================

#[test]
fn serialize_tn_read() {
    assert_eq!(protocol::serialize(&Command::GetTncMode), b"TN\r");
}

#[test]
fn parse_tn_response() -> TestResult {
    // TN 0 is TNC OFF, hardware-verified 2026-07-18 (display shows no
    // packet-mode indicator). An earlier generation mapped 0 to APRS.
    let r = protocol::parse(b"TN 0,0")?;
    let Response::TncMode { mode, data_rate } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Off);
    assert_eq!(data_rate, PacketDataRate::Bps1200);
    Ok(())
}

#[test]
fn parse_tn_kiss_mode() -> TestResult {
    let r = protocol::parse(b"TN 2,0")?;
    let Response::TncMode { mode, data_rate } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Kiss);
    assert_eq!(data_rate, PacketDataRate::Bps1200);
    Ok(())
}

#[test]
fn parse_tn_aprs_9600() -> TestResult {
    // TN 1 is the firmware APRS mode ("APRS 12"/"APRS 96" on the
    // display); data-rate value 1 = 9600 bps.
    let r = protocol::parse(b"TN 1,1")?;
    let Response::TncMode { mode, data_rate } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Aprs);
    assert_eq!(data_rate, PacketDataRate::Bps9600);
    Ok(())
}

// ============================================================================
// DC -- D-STAR Callsign (slot-indexed read)
// ============================================================================

#[test]
fn serialize_dc_read_slot_1() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetDstarCallsign {
            slot: DstarSlot::new(1)?
        }),
        b"DC 1\r"
    );
    Ok(())
}

#[test]
fn serialize_dc_read_slot_6() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetDstarCallsign {
            slot: DstarSlot::new(6)?
        }),
        b"DC 6\r"
    );
    Ok(())
}

#[test]
fn parse_dc_response() -> TestResult {
    let r = protocol::parse(b"DC 1,KQ4NIT  ,D75A")?;
    let Response::DstarCallsign {
        slot,
        callsign,
        suffix,
    } = r
    else {
        return Err(format!("expected DstarCallsign, got {r:?}").into());
    };
    assert_eq!(slot, DstarSlot::new(1)?);
    assert_eq!(callsign, DstarCallsign::new("KQ4NIT")?);
    assert_eq!(suffix, DstarSuffix::new("D75A")?);
    assert_eq!(callsign.to_wire_bytes(), *b"KQ4NIT  ");
    assert_eq!(suffix.to_wire_bytes(), *b"D75A");
    Ok(())
}

#[test]
fn parse_dc_accepts_hardware_observed_empty_fields() -> TestResult {
    let response = protocol::parse(b"DC 3,,")?;
    let Response::DstarCallsign {
        slot,
        callsign,
        suffix,
    } = response
    else {
        return Err(format!("expected DstarCallsign, got {response:?}").into());
    };
    assert_eq!(slot, DstarSlot::new(3)?);
    assert!(callsign.is_empty());
    assert_eq!(suffix, DstarSuffix::default());
    Ok(())
}

#[test]
fn serialize_dc_write_uses_exact_fixed_width_fields() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetDstarCallsign {
            slot: DstarSlot::new(1)?,
            callsign: DstarCallsign::new("KQ4NIT")?,
            suffix: DstarSuffix::new("D75A")?,
        }),
        b"DC 1,KQ4NIT  ,D75A\r"
    );
    assert_eq!(
        protocol::serialize(&Command::SetDstarCallsign {
            slot: DstarSlot::new(2)?,
            callsign: DstarCallsign::default(),
            suffix: DstarSuffix::default(),
        }),
        b"DC 2,        ,    \r"
    );
    Ok(())
}

#[test]
fn parse_dc_rejects_wrong_field_count() {
    assert!(protocol::parse(b"DC 1,KQ4NIT").is_err());
    assert!(protocol::parse(b"DC 1,KQ4NIT,D75A,EXTRA").is_err());
}

#[test]
fn parse_dc_rejects_invalid_identity_fields() {
    assert!(protocol::parse(b"DC 1,123456789,D75A").is_err());
    assert!(protocol::parse(b"DC 1,KQ4NIT,12345").is_err());
    assert!(protocol::parse("DC 1,NØCALL,D75A".as_bytes()).is_err());
    assert!(protocol::parse(b"DC 1,N0\rCALL,D75A").is_err());
}

// ============================================================================
// RT -- Real-Time Clock (bare read only)
// ============================================================================

#[test]
fn serialize_rt_read() {
    assert_eq!(protocol::serialize(&Command::GetRealTimeClock), b"RT\r");
}

#[test]
fn parse_rt_response() -> TestResult {
    let r = protocol::parse(b"RT 240104095700")?;
    let Response::RealTimeClock { clock } = r else {
        return Err(format!("expected RealTimeClock, got {r:?}").into());
    };
    let Some(datetime) = clock.date_time() else {
        return Err("expected available radio clock".into());
    };
    assert_eq!(datetime.to_wire_string(), "240104095700");
    assert_eq!(datetime.to_string(), "2024-01-04 09:57:00");
    Ok(())
}

#[test]
fn parse_rt_unavailable_response() -> TestResult {
    let r = protocol::parse(b"RT ------------")?;
    assert!(matches!(
        r,
        Response::RealTimeClock {
            clock: kenwood_thd75::types::RadioClock::Unavailable
        }
    ));
    Ok(())
}

#[test]
fn reject_malformed_rt_responses() {
    for response in [
        b"RT".as_slice(),
        b"RT 230229235959",
        b"RT 241332000000",
        b"RT 240101240000",
        b"RT -----------",
        b"RT -------------",
    ] {
        assert!(protocol::parse(response).is_err(), "accepted {response:?}");
    }
}
