//! Integration tests for TN, DC, RT protocol commands.
//!
//! Hardware-verified on D75:
//! - TN: TNC mode (bare read only, returns mode,setting)
//! - DC: D-STAR callsign slots 1-6 (slot-indexed read)
//! - RT: Real-time clock (bare read, returns `YYMMDDHHmmss`)
//!
//! The D75 RE originally identified these as tone commands, but hardware
//! testing confirmed the actual semantics documented here.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::{DstarSlot, TncBaud, TncMode};

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
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
    let Response::TncMode { mode, setting } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Off);
    assert_eq!(setting, TncBaud::Bps1200);
    Ok(())
}

#[test]
fn parse_tn_kiss_mode() -> TestResult {
    let r = protocol::parse(b"TN 2,0")?;
    let Response::TncMode { mode, setting } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Kiss);
    assert_eq!(setting, TncBaud::Bps1200);
    Ok(())
}

#[test]
fn parse_tn_aprs_9600() -> TestResult {
    // TN 1 is the firmware APRS mode ("APRS 12"/"APRS 96" on the
    // display); setting 1 = 9600 bps.
    let r = protocol::parse(b"TN 1,1")?;
    let Response::TncMode { mode, setting } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Aprs);
    assert_eq!(setting, TncBaud::Bps9600);
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
    assert_eq!(callsign, "KQ4NIT  ");
    assert_eq!(suffix, "D75A");
    Ok(())
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
