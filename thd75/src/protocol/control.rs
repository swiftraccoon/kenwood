//! Control commands: AI, BY, DL, DW, RX, TX, LC, IO, BL, VD, VG, VX.
//!
//! These commands control radio-wide functions including auto-info
//! notifications, transmit/receive switching, lock control, battery level,
//! frequency stepping, beep setting, and VOX (Voice-Operated Exchange)
//! settings for hands-free operation.

use crate::error::ProtocolError;
use crate::types::BacklightControl;
use crate::types::Band;
use crate::types::radio_params::{BatteryLevel, DetectOutputMode, VoxDelay, VoxGain};

use super::Response;

/// Parse a control command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a control command.
pub(crate) fn parse_control(
    mnemonic: &str,
    payload: &str,
) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "AI" => Some(match payload {
            "" => Ok(Response::AutoInfoAck),
            _ => parse_required_bool(payload, "AI", "enabled")
                .map(|enabled| Response::AutoInfo { enabled }),
        }),
        "BY" => Some(parse_by(payload)),
        "DL" => Some(
            parse_required_bool(payload, "DL", "enabled")
                .map(|enabled| Response::DualBand { enabled }),
        ),
        "DW" => Some(parse_empty_action(payload, "DW").map(|()| Response::FrequencyDown)),
        "UP" => Some(parse_empty_action(payload, "UP").map(|()| Response::FrequencyUp)),
        "RX" | "TX" => Some(parse_empty_action(payload, mnemonic).map(|()| Response::Ok)),
        "LC" => Some(parse_u8_field(payload, "LC", "mode").and_then(|raw| {
            BacklightControl::try_from(raw)
                .map(|mode| Response::BacklightControl { mode })
                .map_err(|error| ProtocolError::FieldParse {
                    command: "LC".to_owned(),
                    field: "mode".to_owned(),
                    detail: error.to_string(),
                })
        })),
        "IO" => Some(parse_u8_field(payload, "IO", "value").and_then(|raw| {
            DetectOutputMode::try_from(raw)
                .map(|value| Response::IoPort { value })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "IO".into(),
                    field: "value".into(),
                    detail: e.to_string(),
                })
        })),
        "BL" => Some(parse_bl(payload)),
        "VD" => Some(parse_u8_field(payload, "VD", "delay").and_then(|raw| {
            VoxDelay::try_from(raw)
                .map(|delay| Response::VoxDelay { delay })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "VD".into(),
                    field: "delay".into(),
                    detail: e.to_string(),
                })
        })),
        "VG" => Some(parse_u8_field(payload, "VG", "gain").and_then(|raw| {
            VoxGain::try_from(raw)
                .map(|gain| Response::VoxGain { gain })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "VG".into(),
                    field: "gain".into(),
                    detail: e.to_string(),
                })
        })),
        "VX" => Some(
            parse_required_bool(payload, "VX", "enabled").map(|enabled| Response::Vox { enabled }),
        ),
        _ => None,
    }
}

/// Parse a required Boolean digit.
fn parse_required_bool(payload: &str, cmd: &str, field: &str) -> Result<bool, ProtocolError> {
    match payload {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: field.to_owned(),
            detail: format!("expected 0 or 1, got {payload:?}"),
        }),
    }
}

/// Require the empty payload used by a bare action acknowledgment.
fn parse_empty_action(payload: &str, cmd: &str) -> Result<(), ProtocolError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "payload".to_owned(),
            detail: format!("expected an empty payload, got {payload:?}"),
        })
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
    let (band_str, value) = payload
        .split_once(',')
        .ok_or_else(|| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "all".to_owned(),
            detail: format!("expected band,value, got {payload:?}"),
        })?;
    let band_val = parse_u8_field(band_str, cmd, "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    Ok((band, value))
}

/// Parse BL (battery level): bare `"level"` response.
///
/// 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green),
/// 4=Charging (USB power connected).
///
/// The radio sends `BL 3` for a polled read, but AI-mode unsolicited
/// notifications may push `BL 0,3` (band-prefixed). Taking the last
/// comma-separated field handles both formats.
fn parse_bl(payload: &str) -> Result<Response, ProtocolError> {
    let level_str = if let Some((_prefix, level)) = payload.split_once(',') {
        level
    } else {
        payload
    };
    let raw = parse_u8_field(level_str.trim(), "BL", "level")?;
    let level = BatteryLevel::try_from(raw).map_err(|e| ProtocolError::FieldParse {
        command: "BL".to_owned(),
        field: "level".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::BatteryLevel { level })
}

/// Parse BY (busy): "band,busy".
fn parse_by(payload: &str) -> Result<Response, ProtocolError> {
    let (band, val_str) = split_band_value(payload, "BY")?;
    let busy = parse_required_bool(val_str, "BY", "busy")?;
    Ok(Response::Busy { band, busy })
}
