//! Core commands: FQ, FO, FV, PS, ID, PC, BC, VM, FR (FM radio).
//!
//! Provides serialization of write commands and parsing of responses for
//! the 9 core CAT protocol commands.

use crate::error::ProtocolError;
use crate::types::Band;
use crate::types::channel::{
    CatChannelMode, CatChannelRecord, ChannelMemory, ChannelName, CrossToneType, FineStep,
    FlashDigitalSquelch,
};
use crate::types::frequency::Frequency;
use crate::types::mode::{PowerLevel, ShiftDirection, StepSize};
use crate::types::radio_params::VfoMemoryMode;
use crate::types::tone::{DcsCode, ToneCode};

use super::Response;

/// Number of comma-separated fields in an FO response (including band).
const FO_FIELD_COUNT: usize = 21;

/// Number of channel-data fields (everything after the band/channel prefix).
pub(crate) const CHANNEL_FIELD_COUNT: usize = 20;

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
        "FO" => Some(parse_fo(payload)),
        "FQ" => Some(parse_fq(payload)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a boolean field ("0" or "1").
fn parse_bool_field(payload: &str, cmd: &str) -> Result<bool, ProtocolError> {
    parse_bool_field_named(payload, cmd, "value")
}

/// Parse a named boolean field ("0" or "1").
fn parse_bool_field_named(payload: &str, cmd: &str, field: &str) -> Result<bool, ProtocolError> {
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

/// Parse a `u8` from a string field (decimal).
fn parse_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    s.parse::<u8>().map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid u8: {s:?}"),
    })
}

/// Parse an exact-width decimal `u8` field.
fn parse_fixed_decimal_u8(
    s: &str,
    width: usize,
    cmd: &str,
    field: &str,
) -> Result<u8, ProtocolError> {
    if s.len() != width || !s.as_bytes().iter().all(u8::is_ascii_digit) {
        return Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: field.to_owned(),
            detail: format!("expected exactly {width} decimal digit(s), got {s:?}"),
        });
    }
    parse_u8_field(s, cmd, field)
}

/// Parse a `u8` from a hex string field (e.g., step size in FO/ME uses TABLE C hex indices).
///
/// Confirmed by KI4LAX TABLE C (indices A=10, B=11) and ARFC-D75 decompilation
/// (`NumberStyles.HexNumber` in response parsing).
fn parse_hex_u8_field(s: &str, cmd: &str, field: &str) -> Result<u8, ProtocolError> {
    if !matches!(s.as_bytes(), [b'0'..=b'9' | b'A'..=b'F']) {
        return Err(ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: field.to_owned(),
            detail: format!("expected one uppercase hexadecimal digit, got {s:?}"),
        });
    }
    u8::from_str_radix(s, 16).map_err(|_| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: field.to_owned(),
        detail: format!("invalid hex u8: {s:?}"),
    })
}

/// Parse a PC (power level) response: "band,level".
fn parse_pc(payload: &str) -> Result<Response, ProtocolError> {
    let (band_str, level_str) =
        payload
            .split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "PC".to_owned(),
                field: "all".to_owned(),
                detail: format!("expected band,level, got {payload:?}"),
            })?;
    let band_val = parse_u8_field(band_str, "PC", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "PC".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let level_val = parse_u8_field(level_str, "PC", "level")?;
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
    let (band_str, mode_str) =
        payload
            .split_once(',')
            .ok_or_else(|| ProtocolError::FieldParse {
                command: "VM".to_owned(),
                field: "all".to_owned(),
                detail: format!("expected band,mode, got {payload:?}"),
            })?;
    let band_val = parse_u8_field(band_str, "VM", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "VM".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let mode_raw = parse_u8_field(mode_str, "VM", "mode")?;
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
#[expect(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "Parser for the 20-field FO/ME CAT channel record. The single-function layout \
              mirrors the wire format 1:1 so the bit/field correspondence is visible in one \
              place; splitting it would fragment the protocol decoding across helpers and \
              obscure the mapping between wire field index and channel struct field. \
              `tone_code`/`ctcss_code`/`dtcs_code` share prefixes because they are the \
              canonical firmware RE names."
)]
pub(crate) fn parse_channel_fields(
    fields: &[&str],
    cmd: &str,
) -> Result<CatChannelRecord, ProtocolError> {
    let &[
        f_rx_freq,
        f_tx_offset,
        f_step,
        f_tx_step,
        f_mode,
        f_fine_tuning,
        f_fine_step,
        f_tone_en,
        f_ctcss_en,
        f_dcs_en,
        f_cross_tone,
        f_reverse,
        f_shift,
        f_tone_code,
        f_ctcss_code,
        f_dcs_code,
        f_cross_tone_type,
        f_urcall,
        f_digital_squelch,
        f_digital_code,
    ] = <&[&str; CHANNEL_FIELD_COUNT]>::try_from(fields).map_err(|_| {
        ProtocolError::FieldCount {
            command: cmd.to_owned(),
            expected: CHANNEL_FIELD_COUNT,
            actual: fields.len(),
        }
    })?;

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
    //  [4]  Mode, CAT WIRE encoding          → byte[9] bits 6:4
    //       (0=FM, 1=DV, 2=NFM, 3=AM, hardware-verified; NOT the
    //       MD/flash table, where 2=AM and 6=NFM)
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
    let rx_frequency = Frequency::from_wire_field(f_rx_freq, cmd, "rx_frequency")?;

    // field 1: TX offset or split TX frequency (10 digits)
    let tx_offset = Frequency::from_wire_field(f_tx_offset, cmd, "tx_offset")?;

    // field 2: RX step size (hex per KI4LAX TABLE C: A=50kHz, B=100kHz)
    let step_val = parse_hex_u8_field(f_step, cmd, "step_size")?;
    let step_size = StepSize::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "step_size".to_owned(),
        detail: e.to_string(),
    })?;

    // fields 3-6: byte[9] components (mode, fine tuning)
    // Reconstruct byte[9] from wire fields for binary round-trip.
    let tx_step_raw = parse_hex_u8_field(f_tx_step, cmd, "tx_step")?;
    let tx_step_size = StepSize::try_from(tx_step_raw).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "tx_step".to_owned(),
        detail: e.to_string(),
    })?;
    let mode_val = parse_fixed_decimal_u8(f_mode, 1, cmd, "mode")?;
    let mode = CatChannelMode::try_from(mode_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    let fine_tuning = parse_bool_field(f_fine_tuning, cmd)?;
    let fine_step_raw = parse_fixed_decimal_u8(f_fine_step, 1, cmd, "fine_step")?;
    let fine_step = FineStep::try_from(fine_step_raw).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "fine_step".to_owned(),
        detail: e.to_string(),
    })?;
    let mode_flags_raw = (u8::from(mode) << 4) | (u8::from(fine_tuning) << 3) | u8::from(fine_step);

    // fields 7-12: byte[10] bits unpacked into 6 individual wire fields
    // (verified: real D75 sends exactly 6 fields between fine_step and tone_code)
    let tone_enable = parse_bool_field_named(f_tone_en, cmd, "tone_enable")?;
    let ctcss_enable = parse_bool_field_named(f_ctcss_en, cmd, "ctcss_enable")?;
    let dcs_enable = parse_bool_field_named(f_dcs_en, cmd, "dcs_enable")?;
    let cross_tone = parse_bool_field_named(f_cross_tone, cmd, "cross_tone")?;
    let reverse = parse_bool_field_named(f_reverse, cmd, "reverse")?;
    // field[12]: shift direction, combining split + direction in one value
    // (0=simplex, 1=shift+, 2=shift-, 4=split; byte[10] bits 2:0)
    let shift_val = parse_fixed_decimal_u8(f_shift, 1, cmd, "shift")?;
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

    // field 13: tone frequency code (2 digits)
    let tone_val = parse_fixed_decimal_u8(f_tone_code, 2, cmd, "tone_code")?;
    let tone_code = ToneCode::new(tone_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "tone_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 14: CTCSS frequency code (2 digits)
    let ct_code_val = parse_fixed_decimal_u8(f_ctcss_code, 2, cmd, "ctcss_code")?;
    let ctcss_code = ToneCode::new(ct_code_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "ctcss_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 15: DCS code (3 digits)
    let dcs_val = parse_fixed_decimal_u8(f_dcs_code, 3, cmd, "dcs_code")?;
    let dcs_code = DcsCode::new(dcs_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "dcs_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 16: cross-tone combination (byte[14] bits 5:4, range 0-3)
    let ct_val = parse_fixed_decimal_u8(f_cross_tone_type, 1, cmd, "cross_tone_combo")?;
    let cross_tone_combo =
        CrossToneType::try_from(ct_val).map_err(|e| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "cross_tone_combo".to_owned(),
            detail: e.to_string(),
        })?;

    // field 17: URCALL callsign (may be empty)
    let urcall = ChannelName::new(f_urcall).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "urcall".to_owned(),
        detail: e.to_string(),
    })?;

    // field 18: digital squelch (0=Off, 1=Code Squelch, 2=Callsign Squelch)
    let ds_val = parse_fixed_decimal_u8(f_digital_squelch, 1, cmd, "digital_squelch")?;
    let digital_squelch =
        FlashDigitalSquelch::try_from(ds_val).map_err(|e| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "digital_squelch".to_owned(),
            detail: e.to_string(),
        })?;

    // field 19: digital code (2 digits)
    let data_mode = parse_fixed_decimal_u8(f_digital_code, 2, cmd, "digital_code")?;

    Ok(CatChannelRecord {
        channel: ChannelMemory {
            rx_frequency,
            tx_offset,
            step_size,
            mode_flags_raw,
            shift,
            flags_0a_raw,
            tone_code,
            ctcss_code,
            dcs_code,
            cross_tone_combo,
            digital_squelch,
            urcall,
            data_mode,
        },
        tx_step_size,
    })
}

/// Parse an FQ response.
///
/// The handler returns exactly `band,frequency`.
fn parse_fq(payload: &str) -> Result<Response, ProtocolError> {
    let (band_str, freq_str) = payload
        .split_once(',')
        .filter(|(_, frequency)| !frequency.contains(','))
        .ok_or_else(|| ProtocolError::FieldCount {
            command: "FQ".to_owned(),
            expected: 2,
            actual: payload.split(',').count(),
        })?;
    let band_val = parse_u8_field(band_str, "FQ", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "FQ".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let frequency = Frequency::from_wire_field(freq_str, "FQ", "frequency")?;
    Ok(Response::Frequency { band, frequency })
}

/// Parse the 21 comma-separated fields of an FO response.
fn parse_fo(payload: &str) -> Result<Response, ProtocolError> {
    let cmd = "FO";
    // Split off the band prefix, leaving the 20 channel-data fields untouched.
    let (band_str, rest) = payload
        .split_once(',')
        .ok_or_else(|| ProtocolError::FieldCount {
            command: cmd.to_owned(),
            expected: FO_FIELD_COUNT,
            actual: 1,
        })?;

    // field 0: band
    let band_val = parse_u8_field(band_str, cmd, "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;

    // Remaining 20 fields are channel data; parse_channel_fields validates the count.
    let channel_fields: Vec<&str> = rest.split(',').collect();
    let channel = parse_channel_fields(&channel_fields, cmd)?;

    Ok(Response::FrequencyFull { band, channel })
}
