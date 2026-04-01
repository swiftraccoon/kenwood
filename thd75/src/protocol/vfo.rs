//! VFO (Variable Frequency Oscillator) commands: AG, SQ, SM, MD, FS, FT, SH, UP, RA.
//!
//! These commands control per-band settings including AF (Audio Frequency)
//! gain, squelch level, S-meter reading, operating mode, frequency step
//! size, filter width, and attenuator.

use crate::error::ProtocolError;
use crate::types::Band;
use crate::types::mode::{Mode, StepSize};

use super::Response;

/// Parse a VFO command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a VFO command.
pub(crate) fn parse_vfo(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "AG" => Some(parse_ag(payload)),
        "SQ" => Some(parse_sq(payload)),
        "SM" => Some(parse_sm(payload)),
        "MD" => Some(parse_md(payload)),
        "FS" => Some(parse_fs(payload)),
        "FT" => Some(parse_ft(payload)),
        "SH" => Some(parse_sh(payload)),
        "UP" => Some(Ok(Response::Ok)),
        "RA" => Some(parse_ra(payload)),
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

/// Split a `"band,value"` payload into (band, `value_str`).
fn split_band_value<'a>(payload: &'a str, cmd: &str) -> Result<(Band, &'a str), ProtocolError> {
    let parts: Vec<&str> = payload.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "all".to_owned(),
            detail: format!("expected band,value, got {payload:?}"),
        });
    }
    let band_val = parse_u8_field(parts[0], cmd, "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    Ok((band, parts[1]))
}

// ---------------------------------------------------------------------------
// Individual parsers
// ---------------------------------------------------------------------------

/// Parse AG (AF gain): bare `"level"` format (no band).
///
/// Hardware observation: bare `AG\r` returns a global gain level (e.g., `091`).
/// Band-indexed `AG 0\r` returns `?`.
fn parse_ag(payload: &str) -> Result<Response, ProtocolError> {
    let level = parse_u8_field(payload.trim(), "AG", "level")?;
    Ok(Response::AfGain { level })
}

/// Parse SQ (squelch): "band,ll" (zero-padded 2 digits).
fn parse_sq(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "SQ")?;
    let level = parse_u8_field(val_str, "SQ", "level")?;
    Ok(Response::Squelch { band, level })
}

/// Parse SM (S-meter): "band,level" (hardware may return 1-4 digits).
fn parse_sm(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "SM")?;
    let level = parse_u8_field(val_str, "SM", "level")?;
    Ok(Response::Smeter { band, level })
}

/// Parse MD (mode): "band,mode".
fn parse_md(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "MD")?;
    let mode_val = parse_u8_field(val_str, "MD", "mode")?;
    let mode = Mode::try_from(mode_val).map_err(|e| ProtocolError::FieldParse {
        command: "MD".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::Mode { band, mode })
}

/// Parse FS (frequency step): `"band,step"` format.
///
/// D75 RE: `FS x,y` (x: band, y: step index 0-11).
fn parse_fs(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "FS")?;
    let step_val = parse_u8_field(val_str, "FS", "step")?;
    let step = StepSize::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: "FS".to_owned(),
        field: "step".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::FrequencyStep { band, step })
}

/// Parse FT (function type): bare data (no band).
///
/// Response to `FT\r` is a data value, possibly prefixed by band
/// in "band,data" format for backward compatibility.
fn parse_ft(payload: &str) -> Result<Response, ProtocolError> {
    // Handle both bare "N" and "band,N" formats
    let data_str = if let Some((_prefix, val)) = payload.split_once(',') {
        val
    } else {
        payload
    };
    let value = parse_u8_field(data_str, "FT", "value")?;
    Ok(Response::FunctionType { value })
}

/// Parse SH (filter width): `mode_index,width`.
///
/// The response to `SH N\r` includes the mode index and filter width.
fn parse_sh(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.splitn(2, ',').collect();
    if parts.len() == 2 {
        let mode_index = parse_u8_field(parts[0], "SH", "mode_index")?;
        let width = parse_u8_field(parts[1], "SH", "width")?;
        Ok(Response::FilterWidth { mode_index, width })
    } else {
        // Bare response - treat payload as width with mode_index 0
        let width = parse_u8_field(payload, "SH", "width")?;
        Ok(Response::FilterWidth {
            mode_index: 0,
            width,
        })
    }
}

/// Parse RA (attenuator): "band,enabled".
fn parse_ra(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "RA")?;
    let val = parse_u8_field(val_str, "RA", "enabled")?;
    Ok(Response::Attenuator {
        band,
        enabled: val != 0,
    })
}
