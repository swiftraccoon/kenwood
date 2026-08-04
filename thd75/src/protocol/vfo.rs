//! VFO (Variable Frequency Oscillator) commands: AG, SQ, SM, MD, FS, FT, SH, UP, RA.
//!
//! These commands control per-band settings including AF (Audio Frequency)
//! gain, squelch level, S-meter reading, operating mode, fine step,
//! filter width, and attenuator.

use crate::error::ProtocolError;
use crate::types::Band;
use crate::types::channel::FineStep;
use crate::types::mode::OperatingMode;
use crate::types::radio_params::{
    AfGainLevel, FilterMode, FilterWidthIndex, SMeterReading, SquelchLevel,
};

use super::Response;
use super::fields::{boolean, decimal_u8, fixed_decimal_u8, split_exact};

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
        "RA" => Some(parse_ra(payload)),
        _ => None,
    }
}

/// Split a `"band,value"` payload into (band, `value_str`).
fn split_band_value<'a>(payload: &'a str, cmd: &str) -> Result<(Band, &'a str), ProtocolError> {
    let [band_str, value] = split_exact::<2>(payload, cmd)?;
    let band_val = decimal_u8(band_str, cmd, "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    Ok((band, value))
}

// ---------------------------------------------------------------------------
// Individual parsers
// ---------------------------------------------------------------------------

/// Parse AG (AF gain): bare `"level"` format (no band).
///
/// Hardware observation: bare `AG\r` returns a global gain level (e.g., `091`).
/// Band-indexed `AG 0\r` returns `?`.
fn parse_ag(payload: &str) -> Result<Response, ProtocolError> {
    let raw = fixed_decimal_u8::<3>(payload, "AG", "level")?;
    let level = AfGainLevel::try_from(raw).map_err(|error| ProtocolError::FieldParse {
        command: "AG".to_owned(),
        field: "level".to_owned(),
        detail: error.to_string(),
    })?;
    Ok(Response::AfGain { level })
}

/// Parse SQ (squelch): `band,level`.
///
/// Hardware returns both the canonical unpadded form (`3`) and the older
/// two-digit form (`03`). Longer spellings are not part of the wire grammar.
fn parse_sq(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "SQ")?;
    if !(1..=2).contains(&val_str.len()) {
        return Err(ProtocolError::FieldParse {
            command: "SQ".to_owned(),
            field: "level".to_owned(),
            detail: format!("expected one or two decimal digits, got {val_str:?}"),
        });
    }
    let raw = decimal_u8(val_str, "SQ", "level")?;
    let level = SquelchLevel::try_from(raw).map_err(|e| ProtocolError::FieldParse {
        command: "SQ".to_owned(),
        field: "level".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::Squelch { band, level })
}

/// Parse SM (S-meter): "band,level" (hardware may return 1-4 digits).
fn parse_sm(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "SM")?;
    let raw = decimal_u8(val_str, "SM", "level")?;
    let level = SMeterReading::try_from(raw).map_err(|e| ProtocolError::FieldParse {
        command: "SM".to_owned(),
        field: "level".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::Smeter { band, level })
}

/// Parse MD (mode): "band,mode".
fn parse_md(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "MD")?;
    let mode_val = decimal_u8(val_str, "MD", "mode")?;
    let mode = OperatingMode::try_from(mode_val).map_err(|e| ProtocolError::FieldParse {
        command: "MD".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::OperatingMode { band, mode })
}

/// Parse FS (fine step): bare `"value"` format (no band).
///
/// Firmware-verified: bare `FS\r` returns `FS value` (single value, no comma).
/// Value is a fine step index 0-3.
fn parse_fs(payload: &str) -> Result<Response, ProtocolError> {
    let step_val = fixed_decimal_u8::<1>(payload, "FS", "step")?;
    let step = FineStep::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: "FS".to_owned(),
        field: "step".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::FineStep { step })
}

/// Parse FT (Fine Tune state): bare data (no band).
///
/// Response to `FT\r` is exactly one Boolean digit with no band prefix.
fn parse_ft(payload: &str) -> Result<Response, ProtocolError> {
    let enabled = boolean(payload, "FT", "value")?;
    Ok(Response::FineTune { enabled })
}

/// Parse SH (filter width): `mode_index,width`.
///
/// The response to `SH N\r` includes the mode index and filter width.
fn parse_sh(payload: &str) -> Result<Response, ProtocolError> {
    let [mode_str, width_str] = split_exact::<2>(payload, "SH")?;
    let mode_raw = decimal_u8(mode_str, "SH", "mode")?;
    let mode = FilterMode::try_from(mode_raw).map_err(|e| ProtocolError::FieldParse {
        command: "SH".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    let width_raw = decimal_u8(width_str, "SH", "width")?;
    let width = FilterWidthIndex::new(mode, width_raw).map_err(|e| ProtocolError::FieldParse {
        command: "SH".into(),
        field: "width".into(),
        detail: e.to_string(),
    })?;
    Ok(Response::FilterWidth { width })
}

/// Parse RA (attenuator): "band,enabled".
fn parse_ra(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "RA")?;
    let enabled = boolean(val_str, "RA", "enabled")?;
    Ok(Response::Attenuator { band, enabled })
}
