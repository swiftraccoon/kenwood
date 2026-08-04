//! Core commands: FQ, FO, FV, PS, ID, PC, BC, VM, FR (FM radio).
//!
//! Provides serialization of write commands and parsing of responses for
//! the 9 core CAT protocol commands.

use crate::error::ProtocolError;
use crate::types::channel::{
    CatChannelRecord, ChannelCodeUnidentifiedBits, CrossToneField, FineStep,
};
use crate::types::dstar::{DigitalSquelchCode, DigitalSquelchType, DstarCallsign};
use crate::types::frequency::Frequency;
use crate::types::mode::{ChannelMode, PowerLevel, ShiftDirection, StepSize};
use crate::types::radio_params::TuningMode;
use crate::types::tone::{CtcssCode, DcsCode, ToneCode, ToneMode};
use crate::types::{Band, FirmwareIdentity, RadioModel};

use super::Response;
use super::fields::{
    boolean, decimal_u8, fixed_decimal_u8, split_exact, upper_hex_nibble, zero_padded_decimal_u8,
};

/// Number of comma-separated fields in an FO response (including band).
const FO_FIELD_COUNT: usize = 21;

/// Number of channel-data fields (everything after the band/channel prefix).
pub(crate) const CHANNEL_FIELD_COUNT: usize = 20;

/// Parse a core command response from mnemonic and payload.
///
/// Returns `None` if the mnemonic is not a core command.
pub(crate) fn parse_core(mnemonic: &str, payload: &str) -> Option<Result<Response, ProtocolError>> {
    match mnemonic {
        "ID" => Some(parse_radio_id(payload)),
        "FV" => Some(parse_firmware_version(payload)),
        "PS" => Some(boolean(payload, "PS", "value").map(|on| Response::PowerStatus { on })),
        "PC" => Some(parse_pc(payload)),
        "BC" => Some(parse_bc(payload)),
        "VM" => Some(parse_vm(payload)),
        "FR" => Some(boolean(payload, "FR", "value").map(|enabled| Response::FmRadio { enabled })),
        "FO" => Some(parse_fo(payload)),
        "FQ" => Some(parse_fq(payload)),
        _ => None,
    }
}

fn parse_radio_id(payload: &str) -> Result<Response, ProtocolError> {
    RadioModel::try_from(payload)
        .map(|model| Response::RadioId { model })
        .map_err(|error| ProtocolError::FieldParse {
            command: "ID".to_owned(),
            field: "model".to_owned(),
            detail: error.to_string(),
        })
}

fn parse_firmware_version(payload: &str) -> Result<Response, ProtocolError> {
    FirmwareIdentity::new(payload)
        .map(|version| Response::FirmwareVersion { version })
        .map_err(|error| ProtocolError::FieldParse {
            command: "FV".to_owned(),
            field: "version".to_owned(),
            detail: error.to_string(),
        })
}

/// Parse a PC (power level) response: "band,level".
fn parse_pc(payload: &str) -> Result<Response, ProtocolError> {
    let [band_str, level_str] = split_exact::<2>(payload, "PC")?;
    let band_val = decimal_u8(band_str, "PC", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "PC".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let level_val = decimal_u8(level_str, "PC", "level")?;
    let level = PowerLevel::try_from(level_val).map_err(|e| ProtocolError::FieldParse {
        command: "PC".to_owned(),
        field: "level".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::PowerLevel { band, level })
}

/// Parse a VM tuning-mode response: "band,mode".
///
/// Tuning mode values: 0 = VFO, 1 = Memory, 2 = Call, 3 = WX.
fn parse_vm(payload: &str) -> Result<Response, ProtocolError> {
    let [band_str, mode_str] = split_exact::<2>(payload, "VM")?;
    let band_val = decimal_u8(band_str, "VM", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "VM".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    let mode_raw = decimal_u8(mode_str, "VM", "mode")?;
    let mode = TuningMode::try_from(mode_raw).map_err(|e| ProtocolError::FieldParse {
        command: "VM".to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::TuningMode { band, mode })
}

/// Parse a BC (band) response: single band number.
fn parse_bc(payload: &str) -> Result<Response, ProtocolError> {
    let band_val = decimal_u8(payload, "BC", "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: "BC".to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;
    Ok(Response::Band { band })
}

/// Parse the 20 shared FO/ME channel-data fields.
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
              established names of three distinct wire fields."
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
    //  [4]  Mode                             → byte[9] bits 6:4
    //  [5]  Fine tuning (0/1)                → byte[9] bit 2
    //  [6]  Fine step size                   → byte[9] bits 1:0
    //  [7]  Tone encode enable (0/1)         → byte[10] bit 7
    //  [8]  CTCSS enable (0/1)               → byte[10] bit 6
    //  [9]  DCS enable (0/1)                 → byte[10] bit 5
    // [10]  Cross-tone enable (0/1)          → byte[10] bit 4
    // [11]  Reverse (0/1)                    → byte[10] bit 3
    // [12]  Shift direction                  → byte[10] bits 1:0
    //       (0=simplex, 1=+, 2=-, 3=-7.6 MHz; ME carries split separately)
    // [13]  Tone frequency code (2 digits)   → byte[11]
    // [14]  CTCSS code byte (min. 2 digits)  → byte[12]
    // [15]  DCS code byte (min. 3 digits)    → byte[13]
    // [16]  Cross-tone field (hex 0-F)        → byte[14] high nibble
    // [17]  URCALL callsign                  → byte[15..39]
    // [18]  Digital squelch (0-2)
    // [19]  Digital squelch byte (min. 2 digits)
    //
    // Verified across 20 real channels with zero mismatches between MCP binary
    // and ME CAT response. See probes/fo_field_map.rs.

    // field 0: RX frequency (10 digits)
    let rx_frequency = Frequency::from_wire_field(f_rx_freq, cmd, "rx_frequency")?;

    // field 1: TX offset or split TX frequency (10 digits)
    let tx_offset = Frequency::from_wire_field(f_tx_offset, cmd, "tx_offset")?;

    // field 2: RX step-size index, encoded as one hexadecimal digit.
    let step_val = upper_hex_nibble(f_step, cmd, "step_size")?;
    let step_size = StepSize::try_from(step_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "step_size".to_owned(),
        detail: e.to_string(),
    })?;

    // fields 3-6: byte[8] low nibble and byte[9] components.
    let tx_step_raw = upper_hex_nibble(f_tx_step, cmd, "tx_step")?;
    let tx_step_size = StepSize::try_from(tx_step_raw).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "tx_step".to_owned(),
        detail: e.to_string(),
    })?;
    let mode_val = fixed_decimal_u8::<1>(f_mode, cmd, "mode")?;
    let mode = ChannelMode::try_from(mode_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "mode".to_owned(),
        detail: e.to_string(),
    })?;
    let fine_tuning = boolean(f_fine_tuning, cmd, "fine_tuning")?;
    let fine_step_raw = fixed_decimal_u8::<1>(f_fine_step, cmd, "fine_step")?;
    let fine_step = FineStep::try_from(fine_step_raw).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "fine_step".to_owned(),
        detail: e.to_string(),
    })?;
    // fields 7-10 are a one-hot tone-mode selector. The radio exposes the
    // values in flash-nibble order: Tone=8, CTCSS=4, DCS=2, Cross=1.
    let tone_enable = boolean(f_tone_en, cmd, "tone_enable")?;
    let ctcss_enable = boolean(f_ctcss_en, cmd, "ctcss_enable")?;
    let dcs_enable = boolean(f_dcs_en, cmd, "dcs_enable")?;
    let cross_tone = boolean(f_cross_tone, cmd, "cross_tone")?;
    let tone_mode_raw = (u8::from(tone_enable) << 3)
        | (u8::from(ctcss_enable) << 2)
        | (u8::from(dcs_enable) << 1)
        | u8::from(cross_tone);
    let tone_mode =
        ToneMode::try_from(tone_mode_raw).map_err(|error| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "tone_mode".to_owned(),
            detail: error.to_string(),
        })?;

    // fields 11-12: repeater reverse and shift direction.
    let reverse = boolean(f_reverse, cmd, "reverse")?;
    let shift_val = fixed_decimal_u8::<1>(f_shift, cmd, "shift")?;
    let shift = ShiftDirection::try_from(shift_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "shift".to_owned(),
        detail: e.to_string(),
    })?;

    // field 13: tone frequency code (2 digits)
    let tone_val = fixed_decimal_u8::<2>(f_tone_code, cmd, "tone_code")?;
    let tone_code = ToneCode::new(tone_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "tone_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 14: complete CTCSS code byte, padded to at least 2 digits.
    let ctcss_wire = zero_padded_decimal_u8::<2>(f_ctcss_code, cmd, "ctcss_code")?;
    let ctcss_code = CtcssCode::new(ctcss_wire & 0x3F).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "ctcss_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 15: complete DCS code byte, padded to at least 3 digits.
    let dcs_wire = zero_padded_decimal_u8::<3>(f_dcs_code, cmd, "dcs_code")?;
    let dcs_code = DcsCode::new(dcs_wire & 0x7F).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "dcs_code".to_owned(),
        detail: e.to_string(),
    })?;

    // field 16: exact cross-tone nibble. Its low two bits select the
    // documented combination; the high two bits remain preserved and unnamed.
    let cross_tone_raw = upper_hex_nibble(f_cross_tone_type, cmd, "cross_tone_type")?;
    let cross_tone =
        CrossToneField::new(cross_tone_raw).map_err(|e| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "cross_tone_field".to_owned(),
            detail: e.to_string(),
        })?;

    // field 17: URCALL callsign (may be empty)
    let ur_call = DstarCallsign::new(f_urcall).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "urcall".to_owned(),
        detail: e.to_string(),
    })?;

    // field 18: digital squelch (0=Off, 1=Code Squelch, 2=Callsign Squelch)
    let ds_val = fixed_decimal_u8::<1>(f_digital_squelch, cmd, "digital_squelch")?;
    let digital_squelch =
        DigitalSquelchType::try_from(ds_val).map_err(|e| ProtocolError::FieldParse {
            command: cmd.to_owned(),
            field: "digital_squelch".to_owned(),
            detail: e.to_string(),
        })?;

    // field 19: complete digital-squelch code byte, padded to at least 2 digits.
    let digital_squelch_wire = zero_padded_decimal_u8::<2>(f_digital_code, cmd, "digital_code")?;
    let digital_squelch_code =
        DigitalSquelchCode::new(digital_squelch_wire & 0x7F).map_err(|e| {
            ProtocolError::FieldParse {
                command: cmd.to_owned(),
                field: "digital_code".to_owned(),
                detail: e.to_string(),
            }
        })?;
    let unidentified_code_bits = ChannelCodeUnidentifiedBits::new(
        ctcss_wire >> 6,
        dcs_wire & 0x80 != 0,
        digital_squelch_wire & 0x80 != 0,
    )
    .map_err(|error| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "unidentified_code_bits".to_owned(),
        detail: error.to_string(),
    })?;

    Ok(CatChannelRecord {
        receive_frequency: rx_frequency,
        transmit_offset_or_frequency: tx_offset,
        receive_step: step_size,
        transmit_step: tx_step_size,
        mode,
        fine_tuning,
        fine_step,
        tone_mode,
        reverse,
        shift,
        tone_code,
        ctcss_code,
        dcs_code,
        cross_tone,
        ur_call,
        digital_squelch,
        digital_squelch_code,
        unidentified_code_bits,
    })
}

/// Parse an FQ response.
///
/// The response contains exactly `band,frequency`.
fn parse_fq(payload: &str) -> Result<Response, ProtocolError> {
    let [band_str, freq_str] = split_exact::<2>(payload, "FQ")?;
    let band_val = decimal_u8(band_str, "FQ", "band")?;
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
    let fields = split_exact::<FO_FIELD_COUNT>(payload, cmd)?;
    let band_str = fields[0];

    // field 0: band
    let band_val = decimal_u8(band_str, cmd, "band")?;
    let band = Band::try_from(band_val).map_err(|e| ProtocolError::FieldParse {
        command: cmd.to_owned(),
        field: "band".to_owned(),
        detail: e.to_string(),
    })?;

    // Remaining 20 fields are channel data; the exact splitter already
    // established the complete FO shape without allocating.
    let channel = parse_channel_fields(&fields[1..], cmd)?;

    Ok(Response::FrequencyFull { band, channel })
}
