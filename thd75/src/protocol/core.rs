//! Core commands: FQ, FO, FV, PS, ID, PC, BC, VM, FR (FM radio).
//!
//! Provides serialization of write commands and parsing of responses for
//! the 9 core CAT protocol commands.

use crate::error::ProtocolError;
use crate::types::Band;
use crate::types::channel::{ChannelMemory, ChannelName, CrossToneType, FlashDigitalSquelch};
use crate::types::frequency::Frequency;
use crate::types::mode::{PowerLevel, ShiftDirection, StepSize};
use crate::types::radio_params::VfoMemoryMode;
use crate::types::tone::{CtcssMode, DcsCode, ToneCode};

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
    // Unpack byte[10] (flags_0a_raw) into individual wire fields [7..13]:
    //   bit 7 = tone_enable, bit 6 = ctcss, bit 5 = dcs, bit 4 = cross-tone,
    //   bit 3 = reverse, bit 2 = split, bits 1:0 = shift direction
    let tone_en = (ch.flags_0a_raw >> 7) & 1;
    let ctcss_en = (ch.flags_0a_raw >> 6) & 1;
    let dcs_en = (ch.flags_0a_raw >> 5) & 1;
    let cross_tone = (ch.flags_0a_raw >> 4) & 1;
    let reverse = (ch.flags_0a_raw >> 3) & 1;
    let shift_dir = ch.flags_0a_raw & 0x07; // bits 2:0 = split + shift combined

    // Build exactly 20 comma-separated fields matching the real D75 FO wire format.
    // Verified against hardware: see probes/fo_field_map.rs
    format!(
        "{},{},{:X},0,0,0,0,{},{},{},{},{},{},{:02},{:02},{:03},{},{},{},{:02}",
        ch.rx_frequency.to_wire_string(), // [0]  freq
        ch.tx_offset.to_wire_string(),    // [1]  offset
        u8::from(ch.step_size),           // [2]  step (hex: TABLE C A=50kHz, B=100kHz)
        //                                   [3]  tx_step=0
        //                                   [4]  mode=0
        //                                   [5]  fine=0
        //                                   [6]  fine_step=0
        tone_en,                       // [7]  tone encode
        ctcss_en,                      // [8]  CTCSS
        dcs_en,                        // [9]  DCS
        cross_tone,                    // [10] cross-tone
        reverse,                       // [11] reverse
        shift_dir,                     // [12] shift direction
        ch.tone_code.index(),          // [13] tone code
        ch.ctcss_code.index(),         // [14] CTCSS code
        ch.dcs_code.index(),           // [15] DCS code
        u8::from(ch.cross_tone_combo), // [16] cross-tone combo
        ch.urcall.as_str(),            // [17] URCALL
        u8::from(ch.digital_squelch),  // [18] digital squelch
        ch.data_mode,                  // [19] digital code
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

/// Parse a `u8` from a string field (decimal).
fn parse_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    s.parse::<u8>().map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid u8: {s:?}"),
    })
}

/// Parse a `u8` from a hex string field (e.g., step size in FO/ME uses TABLE C hex indices).
///
/// Confirmed by KI4LAX TABLE C (indices A=10, B=11) and ARFC-D75 decompilation
/// (`NumberStyles.HexNumber` in response parsing).
fn parse_hex_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    u8::from_str_radix(s, 16).map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid hex u8: {s:?}"),
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
    let mode_raw = parse_u8_field(parts[1], "VM", "mode")?;
    let mode = VfoMemoryMode::try_from(mode_raw).map_err(|e| ProtocolError::FieldParse {
        command: "VM".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
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
pub(crate) fn parse_channel_fields(
    fields: &[&str],
    cmd: &str,
) -> Result<ChannelMemory, ProtocolError> {
    if fields.len() != CHANNEL_FIELD_COUNT {
        return Err(ProtocolError::FieldCount {
            command: cmd.to_owned(),
            expected: CHANNEL_FIELD_COUNT,
            actual: fields.len(),
        });
    }

    // ── Wire field layout (hardware-verified via MCP↔ME correlation) ──
    //
    // FO wire: 21 fields total (1 band + 20 channel). CHANNEL_FIELD_COUNT = 20.
    // ME wire: 23 fields total (1 channel# + 20 channel + 2 ME-specific).
    // The 20 channel fields (shared between FO and ME) are:
    //
    //  [0]  RX frequency (10 digits)         → byte[0..4]
    //  [1]  TX offset / split TX freq        → byte[4..8]
    //  [2]  RX step size                     → byte[8] high nibble
    //  [3]  TX step size                     → byte[8] low nibble (always 0 on regular channels)
    //  [4]  Mode (0=FM,1=DV,6=NFM,...)       → byte[9] upper nibble
    //  [5]  Fine tuning (0/1)                → byte[9] bit 3 (always 0 on regular channels)
    //  [6]  Fine step size                   → byte[9] bits 2:0 (always 0 on regular channels)
    //  [7]  Tone encode enable (0/1)         → byte[10] bit 7
    //  [8]  CTCSS enable (0/1)               → byte[10] bit 6
    //  [9]  DCS enable (0/1)                 → byte[10] bit 5
    // [10]  Cross-tone enable (0/1)          → byte[10] bit 4
    // [11]  Reverse (0/1)                    → byte[10] bit 3
    // [12]  Shift direction (bits 2:0)       → byte[10] bits 2:0 (0=simplex,1=+,2=-,4=split)
    // [13]  Tone frequency code (2 digits)   → byte[11]
    // [14]  CTCSS frequency code (2 digits)  → byte[12]
    // [15]  DCS code (3 digits)              → byte[13]
    // [16]  Cross-tone combination (0-3)     → byte[14] bits 5:4
    // [17]  URCALL callsign                  → byte[15..39]
    // [18]  Digital squelch (0-2)            → separate from channel struct
    // [19]  Digital code (2 digits)          → separate from channel struct
    //
    // Verified across 20 real channels with zero mismatches between MCP binary
    // and ME CAT response. See probes/fo_field_map.rs.

    // field 0: RX frequency (10 digits)
    let rx_frequency = Frequency::from_wire_string(fields[0])?;

    // field 1: TX offset or split TX frequency (10 digits)
    let tx_offset = Frequency::from_wire_string(fields[1])?;

    // field 2: RX step size (hex per KI4LAX TABLE C: A=50kHz, B=100kHz)
    let step_val = parse_hex_u8_field(fields[2], cmd, "step_size")?;
    let step_size = StepSize::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "step_size".to_owned(),
        detail: e.to_string(),
    })?;

    // fields 3-6: byte[9] components (mode, fine tuning)
    // Reconstruct byte[9] from wire fields for binary round-trip.
    let _tx_step = parse_u8_field(fields[3], cmd, "tx_step")?;
    let mode_val = parse_u8_field(fields[4], cmd, "mode")?;
    let fine_tuning = parse_u8_field(fields[5], cmd, "fine_tuning")?;
    let fine_step = parse_u8_field(fields[6], cmd, "fine_step")?;
    let mode_flags_raw = ((mode_val & 0x07) << 4) | ((fine_tuning & 1) << 3) | (fine_step & 0x07);

    // fields 7-12: byte[10] bits unpacked into 6 individual wire fields
    // (verified: real D75 sends exactly 6 fields between fine_step and tone_code)
    let tone_enable = parse_u8_field(fields[7], cmd, "tone_enable")? != 0;
    let ctcss_enable = parse_u8_field(fields[8], cmd, "ctcss_enable")? != 0;
    let dcs_enable = parse_u8_field(fields[9], cmd, "dcs_enable")? != 0;
    let cross_tone = parse_u8_field(fields[10], cmd, "cross_tone")? != 0;
    let reverse = parse_u8_field(fields[11], cmd, "reverse")? != 0;
    // field[12]: shift direction — combines split + direction in one value
    // (0=simplex, 1=shift+, 2=shift-, 4=split — byte[10] bits 2:0)
    let shift_val = parse_u8_field(fields[12], cmd, "shift")?;
    let shift = ShiftDirection::try_from(shift_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "shift".to_owned(),
        detail: e.to_string(),
    })?;

    // Reconstruct byte[10] from the individual wire fields for flags_0a_raw
    let flags_0a_raw = (u8::from(tone_enable) << 7)
        | (u8::from(ctcss_enable) << 6)
        | (u8::from(dcs_enable) << 5)
        | (u8::from(cross_tone) << 4)
        | (u8::from(reverse) << 3)
        | (shift_val & 0x07);

    // Reconstruct the CTCSS mode from the ctcss_enable flag
    let ctcss_mode = if ctcss_enable {
        CtcssMode::try_from(1u8).unwrap_or_else(|_| CtcssMode::try_from(0u8).expect("valid"))
    } else {
        CtcssMode::try_from(0u8).expect("zero is valid")
    };

    // field 13: tone frequency code (2 digits)
    let tone_val = parse_u8_field(fields[13], cmd, "tone_code")?;
    let tone_code = ToneCode::new(tone_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "tone_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 14: CTCSS frequency code (2 digits)
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

    // field 16: cross-tone combination (byte[14] bits 5:4, range 0-3)
    let ct_val = parse_u8_field(fields[16], cmd, "cross_tone_combo")?;
    let cross_tone_combo =
        CrossToneType::try_from(ct_val & 0x03).map_err(|e| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "cross_tone_combo".to_owned(),
            detail: e.to_string(),
        })?;

    // field 17: URCALL callsign (may be empty)
    let urcall = ChannelName::new(fields[17]).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "urcall".to_owned(),
        detail: e.to_string(),
    })?;

    // field 18: digital squelch (0=Off, 1=Code Squelch, 2=Callsign Squelch)
    let ds_val = parse_u8_field(fields[18], cmd, "digital_squelch")?;
    let digital_squelch =
        FlashDigitalSquelch::try_from(ds_val & 0x03).map_err(|e| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "digital_squelch".to_owned(),
            detail: e.to_string(),
        })?;

    // field 19: digital code (2 digits)
    let data_mode = if fields.len() > 19 {
        parse_u8_field(fields[19], cmd, "digital_code")?
    } else {
        0
    };

    Ok(ChannelMemory {
        rx_frequency,
        tx_offset,
        step_size,
        mode_flags_raw,
        shift,
        reverse,
        tone_enable,
        ctcss_mode,
        dcs_enable,
        cross_tone_reverse: cross_tone,
        flags_0a_raw,
        tone_code,
        ctcss_code,
        dcs_code,
        cross_tone_combo,
        digital_squelch,
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
