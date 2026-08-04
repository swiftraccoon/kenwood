//! GPS commands: GP, GM, GS.
//!
//! Provides parsing of responses for the 3 GPS-related CAT protocol
//! commands:
//! - GP: GPS settings (enabled + PC output)
//! - GM: GPS/Radio mode (single value)
//! - GS: validated GPS NMEA sentence selection

use crate::error::ProtocolError;
use crate::types::{GpsRadioMode, GpsSettings, NmeaSentences};

use super::Response;
use super::fields::{boolean, decimal_u8, split_exact};

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

/// Parse GP (GPS settings): `gps_enabled,pc_output`.
///
/// Two comma-separated values, each 0 or 1.
fn parse_gp(payload: &str) -> Result<Response, ProtocolError> {
    let [gps_str, pc_str] = split_exact::<2>(payload, "GP")?;
    let gps_enabled = boolean(gps_str, "GP", "gps_enabled")?;
    let pc_output = boolean(pc_str, "GP", "pc_output")?;
    Ok(Response::GpsSettings {
        settings: GpsSettings::new(gps_enabled, pc_output),
    })
}

/// Parse GM (GPS mode): single value (0=Normal, 1=GPS Receiver).
///
/// The radio accepts only wire values 0 and 1.
fn parse_gm(payload: &str) -> Result<Response, ProtocolError> {
    let raw = decimal_u8(payload, "GM", "mode")?;
    let mode = GpsRadioMode::try_from(raw).map_err(|e| ProtocolError::FieldParse {
        command: "GM".into(),
        field: "mode".into(),
        detail: e.to_string(),
    })?;
    Ok(Response::GpsMode { mode })
}

/// Parse GS (GPS NMEA sentences): `gga,gll,gsa,gsv,rmc,vtg`.
///
/// Six comma-separated values, each 0 or 1. The radio's documented invariant
/// requires at least one sentence to remain selected.
#[expect(
    clippy::similar_names,
    reason = "NMEA 0183 sentence type codes (gga/gll/gsa/gsv/rmc/vtg) are 3-char \
              identifiers fixed by the standard; several share character pairs by design \
              (gga ↔ gsa, gsv ↔ gga, etc.). Renaming would diverge from the wire-protocol \
              vocabulary the GS command speaks."
)]
fn parse_gs(payload: &str) -> Result<Response, ProtocolError> {
    let [raw_gga, raw_gll, raw_gsa, raw_gsv, raw_rmc, raw_vtg] = split_exact::<6>(payload, "GS")?;
    let sentences = NmeaSentences::try_from_flags([
        boolean(raw_gga, "GS", "gga")?,
        boolean(raw_gll, "GS", "gll")?,
        boolean(raw_gsa, "GS", "gsa")?,
        boolean(raw_gsv, "GS", "gsv")?,
        boolean(raw_rmc, "GS", "rmc")?,
        boolean(raw_vtg, "GS", "vtg")?,
    ])
    .map_err(|error| ProtocolError::FieldParse {
        command: "GS".to_owned(),
        field: "all".to_owned(),
        detail: error.to_string(),
    })?;
    Ok(Response::GpsSentences { sentences })
}
