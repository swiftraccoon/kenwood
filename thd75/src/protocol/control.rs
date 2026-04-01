//! Control commands: AI, BY, DL, DW, RX, TX, LC, IO, BL, BE, VD, VG, VX.
//!
//! These commands control radio-wide functions including auto-info
//! notifications, transmit/receive switching, lock/backlight control,
//! dual watch, beep setting, and VOX (Voice-Operated Exchange) settings
//! for hands-free operation.

use crate::error::ProtocolError;
use crate::types::Band;

use super::Response;

/// Parse a control command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a control command.
pub(crate) fn parse_control(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "AI" => Some(parse_bool(payload, "AI").map(|enabled| Response::AutoInfo { enabled })),
        "BY" => Some(parse_by(payload)),
        "DL" => Some(parse_bool(payload, "DL").map(|enabled| Response::DualBand { enabled })),
        "DW" => Some(parse_bool(payload, "DW").map(|enabled| Response::DualWatch { enabled })),
        "BE" => Some(parse_bool(payload, "BE").map(|enabled| Response::Beep { enabled })),
        "RX" | "TX" => Some(Ok(Response::Ok)),
        "LC" => Some(parse_bool(payload, "LC").map(|locked| Response::Lock { locked })),
        "IO" => {
            Some(parse_u8_field(payload, "IO", "value").map(|value| Response::IoPort { value }))
        }
        "BL" => Some(parse_bl(payload)),
        "VD" => {
            Some(parse_u8_field(payload, "VD", "delay").map(|delay| Response::VoxDelay { delay }))
        }
        "VG" => Some(parse_u8_field(payload, "VG", "gain").map(|gain| Response::VoxGain { gain })),
        "VX" => Some(parse_bool(payload, "VX").map(|enabled| Response::Vox { enabled })),
        _ => None,
    }
}

/// Parse a boolean field ("0" or "1").
///
/// Empty/missing value is treated as `false` (observed on DW, BE).
fn parse_bool(payload: &str, cmd: &str) -> Result<bool, ProtocolError> {
    match payload.trim() {
        "" | "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "value".to_owned(),
            detail: format!("expected 0 or 1, got {payload:?}"),
        }),
    }
}

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

/// Parse BL (backlight): bare `"level"` for read, `"display,level"` for write echo.
fn parse_bl(payload: &str) -> Result<Response, ProtocolError> {
    // Write echo: "0,3" -> take second field
    // Read response: "3" -> take the only field
    let level_str = if let Some((_display, level)) = payload.split_once(',') {
        level
    } else {
        payload
    };
    let level = parse_u8_field(level_str.trim(), "BL", "level")?;
    Ok(Response::Backlight { level })
}

/// Parse BY (busy): "band,busy".
fn parse_by(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "BY")?;
    let val = parse_u8_field(val_str, "BY", "busy")?;
    Ok(Response::Busy {
        band,
        busy: val != 0,
    })
}
