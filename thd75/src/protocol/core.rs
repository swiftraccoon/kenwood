//! Core commands: FQ, FO, FV, PS, ID, PC, BC, VM, FR (FM radio).
//!
//! Provides serialization of write commands and parsing of responses for
//! the 9 core CAT protocol commands.

use crate::error::ProtocolError;
use crate::types::channel::{ChannelMemory, ChannelName};
use crate::types::frequency::Frequency;
use crate::types::mode::{PowerLevel, ShiftDirection, StepSize};
use crate::types::tone::{CtcssMode, DataSpeed, DcsCode, LockoutMode, ToneCode};
use crate::types::Band;

use super::{Command, Response};

/// Number of comma-separated fields in an FO/FQ response (including band).
const FO_FIELD_COUNT: usize = 21;

/// Number of channel-data fields (everything after the band/channel prefix).
pub(crate) const CHANNEL_FIELD_COUNT: usize = 20;

/// Serialize a core write command into its wire-format body (without trailing `\r`).
///
/// Returns `None` if the command is not a core write command that needs
/// special serialization (i.e. read commands already handled by the
/// main dispatcher).
pub(crate) fn serialize_core_write(cmd: &Command) -> Option<String> {
    match cmd {
        Command::SetFrequencyFull { band, channel } => {
            Some(format!("FO {}", format_fo_fields(*band, channel)))
        }
        Command::SetFrequency { band, channel } => {
            Some(format!("FQ {}", format_fo_fields(*band, channel)))
        }
        _ => None,
    }
}

/// Parse a core command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a core command.
pub(crate) fn parse_core(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "ID" => Some(Ok(Response::RadioId {
            model: payload.to_owned(),
        })),
        "FV" => Some(Ok(Response::FirmwareVersion {
            version: payload.to_owned(),
        })),
        "PS" => Some(parse_bool_field(payload, "PS").map(|on| Response::PowerStatus { on })),
        "PC" => Some(parse_pc(payload)),
        "BC" => Some(parse_bc(payload)),
        "VM" => Some(parse_vm(payload)),
        "FR" => Some(parse_bool_field(payload, "FR").map(|enabled| Response::FmRadio { enabled })),
        "FO" => Some(parse_fo_fq(payload, "FO")),
        "FQ" => Some(parse_fq(payload)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serialize just the 20 channel-data fields (no band/channel prefix).
///
/// Used by both FO/FQ (with a band prefix) and ME (with a channel prefix).
pub(crate) fn serialize_channel_fields(ch: &ChannelMemory) -> String {
    // Extract x2..x5 from flags_0a_raw
    let x2 = (ch.flags_0a_raw >> 5) & 1;
    let x3 = (ch.flags_0a_raw >> 4) & 1;
    let x4 = (ch.flags_0a_raw >> 3) & 1;
    let x5 = ch.flags_0a_raw & 0x07;

    format!(
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{:02},{:02},{:03},{},{},{},\
         {:02}",
        ch.rx_frequency.to_wire_string(),
        ch.tx_offset.to_wire_string(),
        u8::from(ch.step_size),
        u8::from(ch.shift),
        u8::from(ch.reverse),
        u8::from(ch.tone_enable),
        u8::from(ch.ctcss_mode),
        u8::from(ch.dcs_enable),
        u8::from(ch.cross_tone_reverse),
        x2,
        x3,
        x4,
        x5,
        ch.tone_code.index(),
        ch.ctcss_code.index(),
        ch.dcs_code.index(),
        u8::from(ch.data_speed),
        ch.urcall.as_str(),
        u8::from(ch.lockout),
        ch.data_mode,
    )
}

/// Format a `ChannelMemory` into the 21 comma-separated FO/FQ wire fields.
fn format_fo_fields(band: Band, ch: &ChannelMemory) -> String {
    format!("{},{}", u8::from(band), serialize_channel_fields(ch))
}

/// Parse a boolean field ("0" or "1").
fn parse_bool_field(payload: &str, cmd: &str) -> Result<bool, ProtocolError> {
    match payload {
        "0" => Ok(false),
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

/// Parse a PC (power level) response: "band,level".
fn parse_pc(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err(ProtocolError::FieldParse {
            command: "PC".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected band,level, got {payload:?}"),
        });
    }
    let band_val = parse_u8_field(parts[0], "PC", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "PC".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let level_val = parse_u8_field(parts[1], "PC", "level")?;
    let level = PowerLevel::try_from(level_val).map_err(|e| ProtocolError::FieldParse {
        command: "PC".to_owned(),
        field: "level".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::PowerLevel { band, level })
}

/// Parse a VM (VFO/Memory mode) response: "band,mode".
///
/// Mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
fn parse_vm(payload: &str) -> Result<Response, ProtocolError> {
    let parts: Vec<&str> = payload.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err(ProtocolError::FieldParse {
            command: "VM".to_owned(),
            field: "all".to_owned(),
            detail: format!("expected band,mode, got {payload:?}"),
        });
    }
    let band_val = parse_u8_field(parts[0], "VM", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "VM".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let mode = parse_u8_field(parts[1], "VM", "mode")?;
    Ok(Response::VfoMemoryMode { band, mode })
}

/// Parse a BC (band) response: single band number.
fn parse_bc(payload: &str) -> Result<Response, ProtocolError> {
    let band_val = parse_u8_field(payload, "BC", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "BC".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::BandResponse { band })
}

/// Parse 20 channel-data fields into a [`ChannelMemory`].
///
/// `fields` must contain exactly 20 elements (the data fields after the
/// band or channel prefix). `cmd` is used for error attribution.
#[allow(clippy::too_many_lines, clippy::similar_names)]
pub(crate) fn parse_channel_fields(fields: &[&str], cmd: &str) -> Result<ChannelMemory, ProtocolError> {
    if fields.len() != CHANNEL_FIELD_COUNT {
        return Err(ProtocolError::FieldCount {
            command: cmd.to_owned(),
            expected: CHANNEL_FIELD_COUNT,
            actual: fields.len(),
        });
    }

    // field 0: RX frequency (10 digits)
    let rx_frequency = Frequency::from_wire_string(fields[0])?;

    // field 1: TX offset (10 digits)
    let tx_offset = Frequency::from_wire_string(fields[1])?;

    // field 2: step size index
    let step_val = parse_u8_field(fields[2], cmd, "step_size")?;
    let step_size = StepSize::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "step_size".to_owned(),
        detail: e.to_string(),
    })?;

    // field 3: shift direction
    let shift_val = parse_u8_field(fields[3], cmd, "shift")?;
    let shift = ShiftDirection::try_from(shift_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "shift".to_owned(),
        detail: e.to_string(),
    })?;

    // field 4: reverse
    let reverse = parse_u8_field(fields[4], cmd, "reverse")? != 0;

    // field 5: tone enable
    let tone_enable = parse_u8_field(fields[5], cmd, "tone_enable")? != 0;

    // field 6: CTCSS mode
    let cm_val = parse_u8_field(fields[6], cmd, "ctcss_mode")?;
    let ctcss_mode = CtcssMode::try_from(cm_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "ctcss_mode".to_owned(),
        detail: e.to_string(),
    })?;

    // field 7: DCS enable
    let dcs_enable = parse_u8_field(fields[7], cmd, "dcs_enable")? != 0;

    // field 8: cross-tone reverse
    let cross_tone_reverse = parse_u8_field(fields[8], cmd, "cross_tone_reverse")? != 0;

    // fields 9-12: unknown flags -> reconstruct flags_0a_raw
    let x2 = parse_u8_field(fields[9], cmd, "x2")?;
    let x3 = parse_u8_field(fields[10], cmd, "x3")?;
    let x4 = parse_u8_field(fields[11], cmd, "x4")?;
    let x5 = parse_u8_field(fields[12], cmd, "x5")?;
    let flags_0a_raw = (x2 << 5) | (x3 << 4) | (x4 << 3) | (x5 & 0x07);

    // field 13: tone code (2 digits)
    let tone_val = parse_u8_field(fields[13], cmd, "tone_code")?;
    let tone_code = ToneCode::new(tone_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "tone_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 14: CTCSS code (2 digits)
    let ct_code_val = parse_u8_field(fields[14], cmd, "ctcss_code")?;
    let ctcss_code = ToneCode::new(ct_code_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "ctcss_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 15: DCS code (3 digits)
    let dcs_val = parse_u8_field(fields[15], cmd, "dcs_code")?;
    let dcs_code = DcsCode::new(dcs_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "dcs_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 16: data speed
    let speed_val = parse_u8_field(fields[16], cmd, "data_speed")?;
    let data_speed = DataSpeed::try_from(speed_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "data_speed".to_owned(),
        detail: e.to_string(),
    })?;

    // field 17: D-STAR URCALL callsign (may be empty)
    let urcall = ChannelName::new(fields[17]).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "urcall".to_owned(),
        detail: e.to_string(),
    })?;

    // field 18: lockout mode
    let lo_val = parse_u8_field(fields[18], cmd, "lockout")?;
    let lockout = LockoutMode::try_from(lo_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "lockout".to_owned(),
        detail: e.to_string(),
    })?;

    // field 19: data mode (2 digits)
    let data_mode = parse_u8_field(fields[19], cmd, "data_mode")?;

    Ok(ChannelMemory {
        rx_frequency,
        tx_offset,
        step_size,
        shift,
        reverse,
        tone_enable,
        ctcss_mode,
        dcs_enable,
        cross_tone_reverse,
        flags_0a_raw,
        tone_code,
        ctcss_code,
        dcs_code,
        data_speed,
        lockout,
        urcall,
        data_mode,
    })
}

/// Parse an FQ response.
///
/// The radio may return either a short 2-field response (`band,frequency`)
/// or a full 21-field response (same format as FO). Both are handled.
fn parse_fq(payload: &str) -> Result<Response, ProtocolError> {
    let fields: Vec<&str> = payload.split(',').collect();
    if fields.len() == 2 {
        // Short format: band, frequency
        let band_val = parse_u8_field(fields[0], "FQ", "band")?;
        let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
            command: "FQ".to_owned(),
            field: "band".to_owned(),
            detail: e.to_string(),
        })?;
        let rx_frequency = Frequency::from_wire_string(fields[1])?;
        let channel = ChannelMemory {
            rx_frequency,
            ..ChannelMemory::default()
        };
        return Ok(Response::Frequency { band, channel });
    }
    // Fall back to full 21-field FO-style parsing.
    parse_fo_fq(payload, "FQ")
}

/// Parse the 21 comma-separated fields of an FO or FQ response.
fn parse_fo_fq(payload: &str, cmd: &str) -> Result<Response, ProtocolError> {
    let fields: Vec<&str> = payload.split(',').collect();
    if fields.len() != FO_FIELD_COUNT {
        return Err(ProtocolError::FieldCount {
            command: cmd.to_owned(),
            expected: FO_FIELD_COUNT,
            actual: fields.len(),
        });
    }

    // field 0: band
    let band_val = parse_u8_field(fields[0], cmd, "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;

    // Remaining 20 fields are channel data
    let channel = parse_channel_fields(&fields[1..], cmd)?;

    match cmd {
        "FO" => Ok(Response::FrequencyFull { band, channel }),
        "FQ" => Ok(Response::Frequency { band, channel }),
        _ => Err(ProtocolError::UnknownCommand(cmd.to_owned())),
    }
}
