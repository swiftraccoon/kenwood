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

// ============================================================================
// TN -- TNC Mode (bare read only)
// ============================================================================

#[test]
fn serialize_tn_read() {
    assert_eq!(protocol::serialize(&Command::GetTncMode), b"TN\r");
}

#[test]
fn parse_tn_response() {
    match protocol::parse(b"TN 0,0").unwrap() {
        Response::TncMode { mode, setting } => {
            assert_eq!(mode, 0);
            assert_eq!(setting, 0);
        }
        other => panic!("expected TncMode, got {other:?}"),
    }
}

#[test]
fn parse_tn_nonzero() {
    match protocol::parse(b"TN 1,2").unwrap() {
        Response::TncMode { mode, setting } => {
            assert_eq!(mode, 1);
            assert_eq!(setting, 2);
        }
        other => panic!("expected TncMode, got {other:?}"),
    }
}

// ============================================================================
// DC -- D-STAR Callsign (slot-indexed read)
// ============================================================================

#[test]
fn serialize_dc_read_slot_1() {
    assert_eq!(
        protocol::serialize(&Command::GetDstarCallsign { slot: 1 }),
        b"DC 1\r"
    );
}

#[test]
fn serialize_dc_read_slot_6() {
    assert_eq!(
        protocol::serialize(&Command::GetDstarCallsign { slot: 6 }),
        b"DC 6\r"
    );
}

#[test]
fn parse_dc_response() {
    match protocol::parse(b"DC 1,KQ4NIT  ,D75A").unwrap() {
        Response::DstarCallsign {
            slot,
            callsign,
            suffix,
        } => {
            assert_eq!(slot, 1);
            assert_eq!(callsign, "KQ4NIT  ");
            assert_eq!(suffix, "D75A");
        }
        other => panic!("expected DstarCallsign, got {other:?}"),
    }
}

// ============================================================================
// RT -- Real-Time Clock (bare read only)
// ============================================================================

#[test]
fn serialize_rt_read() {
    assert_eq!(protocol::serialize(&Command::GetRealTimeClock), b"RT\r");
}

#[test]
fn parse_rt_response() {
    match protocol::parse(b"RT 240104095700").unwrap() {
        Response::RealTimeClock { datetime } => {
            assert_eq!(datetime, "240104095700");
        }
        other => panic!("expected RealTimeClock, got {other:?}"),
    }
}
