//! Scan commands: SR, SF, BS.
//!
//! Provides parsing of responses for scan-related CAT protocol commands.
//!
//! Hardware-verified:
//! - SR has no read form (bare `SR\r` returns `?`). Write-only.
//! - SF is band-indexed (`SF band\r` returns `SF band,value`).
//! - BS is band-indexed (`BS band\r` returns `BS band`).

use crate::error::ProtocolError;
use crate::types::Band;

use super::Response;

/// Parse a scan command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a scan command.
///
/// Note: SR has no read form on the TH-D75 (bare `SR\r` returns `?`).
/// When a write echo `SR value` is received, it is treated as a write
/// acknowledgment (`Ok`).
pub(crate) fn parse_scan(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "SR" => Some(Ok(Response::Ok)),
        "SF" => Some(parse_sf(payload)),
        "BS" => Some(parse_bs(payload)),
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

/// Parse SF (scan function): `band,value`.
///
/// Hardware-verified: `SF band\r` returns `SF band,value`.
fn parse_sf(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err(ProtocolError::FieldParse {
            command: "SF".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected band,value, got {payload:?}"),
        });
    }
    let band_val = parse_u8_field(parts[0], "SF", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "SF".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let value = parse_u8_field(parts[1], "SF", "value")?;
    Ok(Response::ScanRange { band, value })
}

/// Parse BS (band scope): just a band number echoed back.
fn parse_bs(payload: &str) -> Result<Response, ProtocolError> {
    let band_val = parse_u8_field(payload.trim(), "BS", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "BS".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::BandScope { band })
}
