//! TNC, D-STAR callsign, and real-time clock commands: TN, DC, RT.
//!
//! Hardware-verified command behavior:
//! - TN: TNC mode (bare read, returns `mode,data-rate`)
//! - DC: D-STAR callsign slots 1-6 (slot-indexed, returns `slot,callsign,suffix`)
//! - RT: Real-time clock (bare read, returns `YYMMDDHHmmss`)

use crate::error::ProtocolError;
use crate::types::radio_params::{PacketDataRate, TncMode};
use crate::types::{DstarCallsign, DstarSlot, DstarSuffix, RadioClock};

use super::Response;
use super::fields::{decimal_u8, split_exact};

/// Parse a TN/DC/RT command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not one of TN, DC, RT.
pub(crate) fn parse_tone(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "TN" => Some(parse_tn(payload)),
        "DC" => Some(parse_dc(payload)),
        "RT" => Some(parse_rt(payload)),
        _ => None,
    }
}

/// Parse TN (TNC mode): `"mode,data_rate"` format.
///
/// Hardware-verified: bare `TN\r` returns `TN mode,data-rate` (e.g., `TN 0,0`).
fn parse_tn(payload: &str) -> Result<Response, ProtocolError> {
    let [mode_str, data_rate_str] = split_exact::<2>(payload, "TN")?;
    let mode_raw = decimal_u8(mode_str, "TN", "mode")?;
    let mode = TncMode::try_from(mode_raw).map_err(|e| ProtocolError::FieldParse {
        command: "TN".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    let data_rate_raw = decimal_u8(data_rate_str, "TN", "data rate")?;
    let data_rate =
        PacketDataRate::try_from(data_rate_raw).map_err(|e| ProtocolError::FieldParse {
            command: "TN".to_owned(),
            field: "data rate".to_owned(),
            detail: e.to_string(),
        })?;
    Ok(Response::TncMode { mode, data_rate })
}

/// Parse DC (D-STAR callsign): `"slot,callsign,suffix"` format.
///
/// Hardware-verified: `DC slot\r` returns `DC slot,callsign,suffix`.
/// Example: `DC 1,KQ4NIT  ,D75A`.
fn parse_dc(payload: &str) -> Result<Response, ProtocolError> {
    let [slot_str, callsign_str, suffix_str] = split_exact::<3>(payload, "DC")?;
    let raw_slot = decimal_u8(slot_str, "DC", "slot")?;
    let slot = DstarSlot::new(raw_slot).map_err(|e| ProtocolError::FieldParse {
        command: "DC".into(),
        field: "slot".into(),
        detail: e.to_string(),
    })?;
    let callsign = DstarCallsign::new(callsign_str).map_err(|error| ProtocolError::FieldParse {
        command: "DC".to_owned(),
        field: "callsign".to_owned(),
        detail: error.to_string(),
    })?;
    let suffix = DstarSuffix::new(suffix_str).map_err(|error| ProtocolError::FieldParse {
        command: "DC".to_owned(),
        field: "suffix".to_owned(),
        detail: error.to_string(),
    })?;
    Ok(Response::DstarCallsign {
        slot,
        callsign,
        suffix,
    })
}

/// Parse RT (real-time clock): strict datetime or unavailable sentinel.
///
/// Hardware-verified: bare `RT\r` returns `RT YYMMDDHHmmss`.
/// Example: `RT 240104095700`.
fn parse_rt(payload: &str) -> Result<Response, ProtocolError> {
    let clock = RadioClock::try_from(payload).map_err(|error| ProtocolError::FieldParse {
        command: "RT".to_owned(),
        field: "clock".to_owned(),
        detail: error.to_string(),
    })?;
    Ok(Response::RealTimeClock { clock })
}
