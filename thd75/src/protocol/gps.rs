//! GPS commands: GP, GM, GS.
//!
//! Provides parsing of responses for the 3 GPS-related CAT protocol
//! commands:
//! - GP: GPS configuration (enabled + PC output)
//! - GM: GPS/Radio mode (single value)
//! - GS: GPS NMEA sentence enable flags (6 booleans)

use crate::error::ProtocolError;
use crate::types::GpsRadioMode;

use super::Response;

/// Parse a GPS command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a GPS command.
pub(crate) fn parse_gps(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "GP" => Some(parse_gp(payload)),
        "GM" => Some(parse_gm(payload)),
        "GS" => Some(parse_gs(payload)),
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

/// Parse GP (GPS config): `gps_enabled,pc_output`.
///
/// Two comma-separated values, each 0 or 1.
fn parse_gp(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.split(',').collect();
    if parts.len() != 2 {
        return Err(ProtocolError::FieldParse {
            command: "GP".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected 2 fields (gps_enabled,pc_output), got {payload:?}"),
        });
    }
    let gps_val = parse_u8_field(parts[0], "GP", "gps_enabled")?;
    let pc_val = parse_u8_field(parts[1], "GP", "pc_output")?;
    Ok(Response::GpsConfig {
        gps_enabled: gps_val != 0,
        pc_output: pc_val != 0,
    })
}

/// Parse GM (GPS mode): single value (0=Normal, 1=GPS Receiver).
///
/// Firmware-verified: `cat_gm_handler` guard `local_18 < 2`.
fn parse_gm(payload: &str) -> Result<Response, ProtocolError> {
    let raw = parse_u8_field(payload, "GM", "mode")?;
    let mode = GpsRadioMode::try_from(raw).map_err(|e| ProtocolError::FieldParse {
        command: "GM".into(),
        field: "mode".into(),
        detail: e.to_string(),
    })?;
    Ok(Response::GpsMode { mode })
}

/// Parse GS (GPS NMEA sentences): `gga,gll,gsa,gsv,rmc,vtg`.
///
/// Six comma-separated values, each 0 or 1.
#[allow(clippy::similar_names)]
fn parse_gs(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.split(',').collect();
    if parts.len() != 6 {
        return Err(ProtocolError::FieldParse {
            command: "GS".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected 6 fields, got {}", parts.len()),
        });
    }
    let gga = parse_u8_field(parts[0], "GS", "gga")? != 0;
    let gll = parse_u8_field(parts[1], "GS", "gll")? != 0;
    let gsa = parse_u8_field(parts[2], "GS", "gsa")? != 0;
    let gsv = parse_u8_field(parts[3], "GS", "gsv")? != 0;
    let rmc = parse_u8_field(parts[4], "GS", "rmc")? != 0;
    let vtg = parse_u8_field(parts[5], "GS", "vtg")? != 0;
    Ok(Response::GpsSentences {
        gga,
        gll,
        gsa,
        gsv,
        rmc,
        vtg,
    })
}
