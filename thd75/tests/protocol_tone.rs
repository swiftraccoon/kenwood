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
    let r = protocol::parse(b"TN 0,0")?;
    let Response::TncMode { mode, setting } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Aprs);
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
fn parse_tn_navitra() -> TestResult {
    // Setting 1 = 9600 bps — but TncBaud only has 0 and 1,
    // and value 2 would be out of range. Navitra mode with 9600 setting
    // is not documented; if the radio sends TN 1,2 it would be a parse error.
    // Use valid values only.
    let r = protocol::parse(b"TN 1,1")?;
    let Response::TncMode { mode, setting } = r else {
        return Err(format!("expected TncMode, got {r:?}").into());
    };
    assert_eq!(mode, TncMode::Navitra);
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
    let Response::RealTimeClock { datetime } = r else {
        return Err(format!("expected RealTimeClock, got {r:?}").into());
    };
    assert_eq!(datetime, "240104095700");
    Ok(())
}
