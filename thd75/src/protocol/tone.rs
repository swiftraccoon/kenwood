//! TNC, D-STAR callsign, and real-time clock commands: TN, DC, RT.
//!
//! Hardware-verified command behavior:
//! - TN: TNC mode (bare read, returns `mode,setting`)
//! - DC: D-STAR callsign slots 1-6 (slot-indexed, returns `slot,callsign,suffix`)
//! - RT: Real-time clock (bare read, returns `YYMMDDHHmmss`)
//!
//! The D75 firmware RE misidentified these as tone-related commands.
//! Hardware testing confirmed the actual semantics documented here.

use crate::error::ProtocolError;
use crate::types::DstarSlot;
use crate::types::radio_params::{TncBaud, TncMode};

use super::Response;

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a `u8` from a string field.
fn parse_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    s.parse::<u8>().map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid u8: {s:?}"),
    })
}

// ---------------------------------------------------------------------------
// Individual parsers
// ---------------------------------------------------------------------------

/// Parse TN (TNC mode): `"mode,setting"` format.
///
/// Hardware-verified: bare `TN\r` returns `TN mode,setting` (e.g., `TN 0,0`).
fn parse_tn(payload: &str) -> Result<Response, ProtocolError> {
    let (mode_str, setting_str) =
        payload
            .split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "TN".to_owned(),
                field: "all".to_owned(),
                detail: format!("expected mode,setting, got {payload:?}"),
            })?;
    let mode_raw = parse_u8_field(mode_str, "TN", "mode")?;
    let mode = TncMode::try_from(mode_raw).map_err(|e| ProtocolError::FieldParse {
        command: "TN".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    let setting_raw = parse_u8_field(setting_str, "TN", "setting")?;
    let setting = TncBaud::try_from(setting_raw).map_err(|e| ProtocolError::FieldParse {
        command: "TN".to_owned(),
        field: "setting".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::TncMode { mode, setting })
}

/// Parse DC (D-STAR callsign): `"slot,callsign,suffix"` format.
///
/// Hardware-verified: `DC slot\r` returns `DC slot,callsign,suffix`.
/// Example: `DC 1,KQ4NIT  ,D75A`.
fn parse_dc(payload: &str) -> Result<Response, ProtocolError> {
    let (slot_str, rest) = payload
        .split_once(',')
        .ok_or_else(|| ProtocolError::FieldParse {
            command: "DC".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected slot,callsign,suffix, got {payload:?}"),
        })?;
    let (callsign_str, suffix_str) =
        rest.split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "DC".to_owned(),
                field: "all".to_owned(),
                detail: format!("expected slot,callsign,suffix, got {payload:?}"),
            })?;
    let raw_slot = parse_u8_field(slot_str, "DC", "slot")?;
    let slot = DstarSlot::new(raw_slot).map_err(|e| ProtocolError::FieldParse {
        command: "DC".into(),
        field: "slot".into(),
        detail: e.to_string(),
    })?;
    let callsign = callsign_str.to_owned();
    let suffix = suffix_str.to_owned();
    Ok(Response::DstarCallsign {
        slot,
        callsign,
        suffix,
    })
}

/// Parse RT (real-time clock): bare datetime string.
///
/// Hardware-verified: bare `RT\r` returns `RT YYMMDDHHmmss`.
/// Example: `RT 240104095700`.
fn parse_rt(payload: &str) -> Result<Response, ProtocolError> {
    if payload.is_empty() {
        return Err(ProtocolError::FieldParse {
            command: "RT".to_owned(),
            field: "datetime".to_owned(),
            detail: "empty datetime payload".to_owned(),
        });
    }
    Ok(Response::RealTimeClock {
        datetime: payload.to_owned(),
    })
}
