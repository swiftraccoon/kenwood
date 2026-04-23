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
    let (gps_str, pc_str) = payload
        .split_once(',')
        .ok_or_else(|| ProtocolError::FieldParse {
            command: "GP".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected 2 fields (gps_enabled,pc_output), got {payload:?}"),
        })?;
    // Reject any extra comma — matches the old `split(',').len() != 2` check.
    if pc_str.contains(',') {
        return Err(ProtocolError::FieldParse {
            command: "GP".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected 2 fields (gps_enabled,pc_output), got {payload:?}"),
        });
    }
    let gps_val = parse_u8_field(gps_str, "GP", "gps_enabled")?;
    let pc_val = parse_u8_field(pc_str, "GP", "pc_output")?;
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
#[expect(
    clippy::similar_names,
    reason = "NMEA 0183 sentence type codes (gga/gll/gsa/gsv/rmc/vtg) are 3-char \
              identifiers fixed by the standard; several share character pairs by design \
              (gga ↔ gsa, gsv ↔ gga, etc.). Renaming would diverge from the wire-protocol \
              vocabulary the GS command speaks."
)]
fn parse_gs(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.split(',').collect();
    let actual = parts.len();
    let &[raw_gga, raw_gll, raw_gsa, raw_gsv, raw_rmc, raw_vtg] = parts.as_slice() else {
        return Err(ProtocolError::FieldParse {
            command: "GS".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected 6 fields, got {actual}"),
        });
    };
    let gga = parse_u8_field(raw_gga, "GS", "gga")? != 0;
    let gll = parse_u8_field(raw_gll, "GS", "gll")? != 0;
    let gsa = parse_u8_field(raw_gsa, "GS", "gsa")? != 0;
    let gsv = parse_u8_field(raw_gsv, "GS", "gsv")? != 0;
    let rmc = parse_u8_field(raw_rmc, "GS", "rmc")? != 0;
    let vtg = parse_u8_field(raw_vtg, "GS", "vtg")? != 0;
    Ok(Response::GpsSentences {
        gga,
        gll,
        gsa,
        gsv,
        rmc,
        vtg,
    })
}
