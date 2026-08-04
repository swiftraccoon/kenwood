//! Control commands: AI, BY, DL, DW, RX, TX, LC, IO, BL, VD, VG, VX.
//!
//! These commands control radio-wide functions including auto-info
//! notifications, transmit/receive switching, lock control, battery level,
//! frequency stepping, battery status, and VOX (Voice-Operated Exchange)
//! settings for hands-free operation.

use crate::error::ProtocolError;
use crate::types::BacklightControl;
use crate::types::radio_params::{BatteryLevel, UsbAudioOutput, VoxDelay, VoxGain};
use crate::types::{Band, BandMode};

use super::Response;
use super::fields::{boolean, decimal_u8, empty_payload, split_exact};

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
            _ => boolean(payload, "AI", "enabled").map(|enabled| Response::AutoInfo { enabled }),
        }),
        "BY" => Some(parse_by(payload)),
        "DL" => Some(decimal_u8(payload, "DL", "mode").and_then(|raw| {
            BandMode::try_from(raw)
                .map(|mode| Response::BandMode { mode })
                .map_err(|error| ProtocolError::FieldParse {
                    command: "DL".to_owned(),
                    field: "mode".to_owned(),
                    detail: error.to_string(),
                })
        })),
        "DW" => Some(empty_payload(payload, "DW").map(|()| Response::FrequencyDownAck)),
        "UP" => Some(empty_payload(payload, "UP").map(|()| Response::FrequencyUpAck)),
        "RX" => Some(empty_payload(payload, "RX").map(|()| Response::ReceiveAck)),
        "TX" => Some(empty_payload(payload, "TX").map(|()| Response::TransmitAck)),
        "LC" => Some(decimal_u8(payload, "LC", "mode").and_then(|raw| {
            BacklightControl::try_from(raw)
                .map(|mode| Response::BacklightControl { mode })
                .map_err(|error| ProtocolError::FieldParse {
                    command: "LC".to_owned(),
                    field: "mode".to_owned(),
                    detail: error.to_string(),
                })
        })),
        "IO" => Some(decimal_u8(payload, "IO", "value").and_then(|raw| {
            UsbAudioOutput::try_from(raw)
                .map(|output| Response::UsbAudioOutput { output })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "IO".into(),
                    field: "value".into(),
                    detail: e.to_string(),
                })
        })),
        "BL" => Some(parse_bl(payload)),
        "VD" => Some(decimal_u8(payload, "VD", "delay").and_then(|raw| {
            VoxDelay::try_from(raw)
                .map(|delay| Response::VoxDelay { delay })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "VD".into(),
                    field: "delay".into(),
                    detail: e.to_string(),
                })
        })),
        "VG" => Some(decimal_u8(payload, "VG", "gain").and_then(|raw| {
            VoxGain::try_from(raw)
                .map(|gain| Response::VoxGain { gain })
                .map_err(|e| ProtocolError::FieldParse {
                    command: "VG".into(),
                    field: "gain".into(),
                    detail: e.to_string(),
                })
        })),
        "VX" => Some(boolean(payload, "VX", "enabled").map(|enabled| Response::Vox { enabled })),
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

/// Parse BL (battery level): bare `"level"` response.
///
/// 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green),
/// 4=Charging (USB power connected), 5=semantically unidentified runtime
/// state 5.
///
/// The radio sends `BL 3` for a polled read, but AI-mode unsolicited
/// notifications may push `BL 0,3` (band-prefixed). The optional prefix is
/// parsed as a real [`Band`], rather than discarded as arbitrary text.
fn parse_bl(payload: &str) -> Result<Response, ProtocolError> {
    let level_str = if payload.contains(',') {
        let (_band, level) = split_band_value(payload, "BL")?;
        level
    } else {
        payload
    };
    let raw = decimal_u8(level_str, "BL", "level")?;
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
    let busy = boolean(val_str, "BY", "busy")?;
    Ok(Response::Busy { band, busy })
}
