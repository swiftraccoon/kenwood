//! Integration tests for the 10 core protocol commands:
//! FQ, FO, FV, PS, ID, PC, BC, VM, FR.

use kenwood_thd75::error::ProtocolError;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssCode, DcsCode, ToneCode};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint. (`::aprs` is spelled that way because the
// `types::*` glob shadows the bare crate name.)
use ::aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// ID: Radio model identification
// ============================================================================

#[test]
fn serialize_id_read() {
    let bytes = protocol::serialize(&Command::GetRadioId);
    assert_eq!(bytes, b"ID\r");
}

#[test]
fn parse_id_response() -> TestResult {
    let r = protocol::parse(b"ID TH-D75")?;
    let Response::RadioId { model } = &r else {
        return Err(format!("expected RadioId, got {r:?}").into());
    };
    assert_eq!(model, &RadioModel::ThD75);
    Ok(())
}

#[test]
fn parse_id_rejects_non_exact_th_d75_identities() {
    for frame in [
        b"ID TH-D74".as_slice(),
        b"ID th-d75".as_slice(),
        b"ID TH-D75 ".as_slice(),
        b"ID".as_slice(),
    ] {
        assert!(
            protocol::parse(frame).is_err(),
            "accepted non-exact TH-D75 identity {frame:?}"
        );
    }
}

// ============================================================================
// FV: Firmware version
// ============================================================================

#[test]
fn serialize_fv_read() {
    let bytes = protocol::serialize(&Command::GetFirmwareVersion);
    assert_eq!(bytes, b"FV\r");
}

#[test]
fn parse_fv_accepts_valid_firmware_identities() -> TestResult {
    for value in ["1.03", "1.03.000", "1.03.AZM", "DEV-42"] {
        let frame = format!("FV {value}");
        let r = protocol::parse(frame.as_bytes())?;
        let Response::FirmwareVersion { version } = &r else {
            return Err(format!("expected FirmwareVersion, got {r:?}").into());
        };
        assert_eq!(version, &FirmwareIdentity::new(value)?);
    }
    Ok(())
}

#[test]
fn parse_fv_rejects_malformed_firmware_identities() {
    for frame in [
        b"FV".as_slice(),
        b"FV 123456789".as_slice(),
        b"FV  1.03".as_slice(),
        b"FV 1.03 ".as_slice(),
        b"FV 1\n03".as_slice(),
        b"FV 1.0\xC3\xA9".as_slice(),
    ] {
        assert!(
            protocol::parse(frame).is_err(),
            "accepted malformed firmware identity {frame:?}"
        );
    }
}

// ============================================================================
// PS: Power status
// ============================================================================

#[test]
fn serialize_ps_read() {
    let bytes = protocol::serialize(&Command::GetPowerStatus);
    assert_eq!(bytes, b"PS\r");
}

#[test]
fn parse_ps_on() -> TestResult {
    let r = protocol::parse(b"PS 1")?;
    let Response::PowerStatus { on } = r else {
        return Err(format!("expected PowerStatus, got {r:?}").into());
    };
    assert!(on);
    Ok(())
}

#[test]
fn parse_ps_off() -> TestResult {
    let r = protocol::parse(b"PS 0")?;
    let Response::PowerStatus { on } = r else {
        return Err(format!("expected PowerStatus, got {r:?}").into());
    };
    assert!(!on);
    Ok(())
}

// ============================================================================
// PC: Power level
// ============================================================================

#[test]
fn serialize_pc_read() {
    let bytes = protocol::serialize(&Command::GetPowerLevel { band: Band::A });
    assert_eq!(bytes, b"PC 0\r");
}

#[test]
fn serialize_pc_write() {
    let bytes = protocol::serialize(&Command::SetPowerLevel {
        band: Band::B,
        level: PowerLevel::Low,
    });
    assert_eq!(bytes, b"PC 1,2\r");
}

#[test]
fn parse_pc_response() -> TestResult {
    let r = protocol::parse(b"PC 0,2")?;
    let Response::PowerLevel { band, level } = r else {
        return Err(format!("expected PowerLevel, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(level, PowerLevel::Low);
    Ok(())
}

// ============================================================================
// BC: Band read/set
// ============================================================================

#[test]
fn serialize_bc_read() {
    let bytes = protocol::serialize(&Command::GetBand);
    assert_eq!(bytes, b"BC\r");
}

#[test]
fn serialize_bc_set() {
    let bytes = protocol::serialize(&Command::SetBand { band: Band::B });
    assert_eq!(bytes, b"BC 1\r");
}

// ============================================================================
// VM: Tuning mode
// ============================================================================

#[test]
fn serialize_vm_memory_mode() {
    let bytes = protocol::serialize(&Command::SetTuningMode {
        band: Band::A,
        mode: TuningMode::Memory,
    });
    assert_eq!(bytes, b"VM 0,1\r");
}

#[test]
fn serialize_vm_vfo_mode() {
    let bytes = protocol::serialize(&Command::SetTuningMode {
        band: Band::B,
        mode: TuningMode::Vfo,
    });
    assert_eq!(bytes, b"VM 1,0\r");
}

#[test]
fn serialize_vm_call_mode() {
    let bytes = protocol::serialize(&Command::SetTuningMode {
        band: Band::A,
        mode: TuningMode::Call,
    });
    assert_eq!(bytes, b"VM 0,2\r");
}

#[test]
fn serialize_vm_wx_mode() {
    let bytes = protocol::serialize(&Command::SetTuningMode {
        band: Band::A,
        mode: TuningMode::Weather,
    });
    assert_eq!(bytes, b"VM 0,3\r");
}

// ============================================================================
// FR: FM radio on/off
// ============================================================================

#[test]
fn serialize_fr_read() {
    let bytes = protocol::serialize(&Command::GetFmRadio);
    assert_eq!(bytes, b"FR\r");
}

#[test]
fn parse_fr_response_off() -> TestResult {
    let r = protocol::parse(b"FR 0")?;
    let Response::FmRadio { enabled } = r else {
        return Err(format!("expected FmRadio, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}

#[test]
fn parse_fr_response_on() -> TestResult {
    let r = protocol::parse(b"FR 1")?;
    let Response::FmRadio { enabled } = r else {
        return Err(format!("expected FmRadio, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

// ============================================================================
// FQ: Quick frequency
// ============================================================================

#[test]
fn serialize_fq_read_band_a() {
    let bytes = protocol::serialize(&Command::GetFrequency { band: Band::A });
    assert_eq!(bytes, b"FQ 0\r");
}

#[test]
fn serialize_fq_read_band_b() {
    let bytes = protocol::serialize(&Command::GetFrequency { band: Band::B });
    assert_eq!(bytes, b"FQ 1\r");
}

// ============================================================================
// FO: Full frequency and settings (21 comma-separated fields)
// ============================================================================

#[test]
fn serialize_fo_read() {
    let bytes = protocol::serialize(&Command::GetFrequencyFull { band: Band::A });
    assert_eq!(bytes, b"FO 0\r");
}

#[test]
fn serialize_fo_read_band_b() {
    let bytes = protocol::serialize(&Command::GetFrequencyFull { band: Band::B });
    assert_eq!(bytes, b"FO 1\r");
}

#[test]
fn parse_fo_response_21_fields() -> TestResult {
    // Real D75 FO format: all zeros except shift=2 at field[12]
    let raw = b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { band, channel } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel.receive_frequency, Frequency::new(145_000_000));
    assert_eq!(
        channel.transmit_offset_or_frequency,
        Frequency::new(600_000)
    );
    assert_eq!(channel.receive_step, StepSize::Hz5000);
    assert_eq!(channel.transmit_step, StepSize::Hz5000);
    assert_eq!(channel.mode, ChannelMode::Fm);
    assert!(!channel.fine_tuning);
    assert_eq!(channel.fine_step, FineStep::Hz20);
    assert_eq!(channel.tone_mode, ToneMode::Off);
    assert!(!channel.reverse);
    assert_eq!(channel.shift, ShiftDirection::Minus);
    assert_eq!(channel.tone_code, ToneCode::new(8)?);
    assert_eq!(channel.ctcss_code, CtcssCode::new(8)?);
    assert_eq!(channel.dcs_code, DcsCode::new(0)?);
    assert_eq!(channel.cross_tone.tone_type(), CrossToneType::DcsOff);
    assert_eq!(channel.ur_call, DstarCallsign::new("CQCQCQ")?);
    assert_eq!(channel.digital_squelch, DigitalSquelchType::Off);
    assert_eq!(channel.digital_squelch_code, DigitalSquelchCode::new(0)?);
    Ok(())
}

#[test]
fn parse_fo_response_with_name() -> TestResult {
    // 440 MHz repeater: tone enabled, shift+, URCALL=REPEATER.
    // Wire fields: step=0, tx_step=0, mode=0, fine=0, fstep=0,
    //   tone=1[7], ctcss=0[8], dcs=0[9], cross=0[10], rev=0[11], shift=1[12],
    //   tone_code=14, ctcss_code=14, dcs_code=023, combo=0, ur=REPEATER, dsq=1, code=05
    let raw = b"FO 1,0440000000,0005000000,0,0,0,0,0,1,0,0,0,0,1,14,14,023,0,REPEATER,1,05";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { band, channel } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(channel.receive_frequency, Frequency::new(440_000_000));
    assert_eq!(
        channel.transmit_offset_or_frequency,
        Frequency::new(5_000_000)
    );
    assert_eq!(channel.tone_mode, ToneMode::Tone);
    assert!(!channel.reverse);
    assert_eq!(channel.shift, ShiftDirection::Plus);
    assert_eq!(channel.tone_code, ToneCode::new(14)?);
    assert_eq!(channel.ctcss_code, CtcssCode::new(14)?);
    assert_eq!(channel.dcs_code, DcsCode::new(23)?);
    assert_eq!(channel.ur_call, DstarCallsign::new("REPEATER")?);
    assert_eq!(channel.digital_squelch, DigitalSquelchType::CodeSquelch);
    assert_eq!(channel.digital_squelch_code, DigitalSquelchCode::new(5)?);
    assert_eq!(
        channel.unidentified_code_bits,
        ChannelCodeUnidentifiedBits::new(0, false, false)?,
    );
    Ok(())
}

#[test]
fn parse_fo_preserves_unidentified_code_bits_from_complete_bytes() -> TestResult {
    // Firmware formats these complete bytes with a minimum decimal width:
    // 0xCE => 206, 0x97 => 151, and 0x85 => 133.
    let raw = b"FO 1,0440000000,0005000000,0,0,0,0,0,1,0,0,0,0,1,14,206,151,0,REPEATER,1,133";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { channel, .. } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };

    assert_eq!(channel.ctcss_code, CtcssCode::new(14)?);
    assert_eq!(channel.dcs_code, DcsCode::new(23)?);
    assert_eq!(channel.digital_squelch_code, DigitalSquelchCode::new(5)?);
    assert_eq!(
        channel.unidentified_code_bits,
        ChannelCodeUnidentifiedBits::new(3, true, true)?,
    );
    Ok(())
}

#[test]
fn parse_fo_preserves_tx_step_mode_and_fine_fields() -> TestResult {
    let raw = b"FO 1,0118000000,0000000000,A,B,2,1,2,0,0,0,0,0,0,08,08,000,0,,0,00";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { channel, .. } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(channel.receive_step, StepSize::Hz50000);
    assert_eq!(channel.transmit_step, StepSize::Hz100000);
    assert_eq!(channel.mode, ChannelMode::Am);
    assert!(channel.fine_tuning);
    assert_eq!(channel.fine_step, FineStep::Hz500);
    Ok(())
}

#[test]
fn parse_fo_rejects_noncanonical_or_out_of_domain_fields() {
    let invalid = [
        b"FO 0,0145000000,0000600000,a,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,00,0,0,0,0,0,0,0,0,08,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,2,0,0,0,0,0,08,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,8,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,08,08,000,G,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,4,00".as_slice(),
    ];

    for raw in invalid {
        assert!(
            protocol::parse(raw).is_err(),
            "noncanonical FO field was accepted: {:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn parse_fo_wrong_field_count() -> TestResult {
    // Only 10 total fields instead of the 21-field FO wire record.
    let raw = b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0";
    let r = protocol::parse(raw);
    let err = r.err().ok_or("expected FieldCount error but got Ok")?;
    let ProtocolError::FieldCount {
        command,
        expected,
        actual,
    } = err
    else {
        return Err(format!("expected FieldCount, got {err:?}").into());
    };
    assert_eq!(command, "FO");
    assert_eq!(expected, 21);
    assert_eq!(actual, 10);
    Ok(())
}

#[test]
fn parse_fq_rejects_fo_shaped_response() {
    let raw = b"FQ 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    assert!(protocol::parse(raw).is_err());
}

// ============================================================================
// FO: VFO mode extended step and typed shift values
// ============================================================================

#[test]
fn parse_fo_vfo_mode_extended_shift() -> TestResult {
    // VFO mode can return non-zero values at field[3] (tx_step) and field[12] (shift).
    // This response has tx_step=8, shift=2 as seen on real hardware.
    let raw = b"FO 0,0145190000,0000600000,0,8,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { band, channel } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel.receive_frequency, Frequency::new(145_190_000));
    assert_eq!(channel.transmit_step, StepSize::Hz25000);
    assert_eq!(channel.shift, ShiftDirection::Minus);
    assert_eq!(channel.ur_call, DstarCallsign::new("CQCQCQ")?);
    Ok(())
}

#[test]
fn parse_fo_vfo_mode_exact_shift_values() -> TestResult {
    let valid = [
        ShiftDirection::Simplex,
        ShiftDirection::Plus,
        ShiftDirection::Minus,
        ShiftDirection::Minus7Point6MHz,
    ];
    for (shift_val, expected) in (0u8..=3).zip(valid) {
        let raw = format!(
            "FO 0,0145190000,0000600000,0,0,0,0,0,0,0,0,0,0,{shift_val},08,08,000,0,CQCQCQ,0,00"
        );
        let r = protocol::parse(raw.as_bytes())?;
        let Response::FrequencyFull { channel, .. } = r else {
            return Err(format!("expected FrequencyFull, got {r:?}").into());
        };
        assert_eq!(channel.shift, expected);
    }

    for shift_val in 4u8..=9 {
        let raw = format!(
            "FO 0,0145190000,0000600000,0,0,0,0,0,0,0,0,0,0,{shift_val},08,08,000,0,CQCQCQ,0,00"
        );
        assert!(
            protocol::parse(raw.as_bytes()).is_err(),
            "out-of-domain shift {shift_val} was accepted"
        );
    }
    Ok(())
}

// ============================================================================
// FQ: Short (2-field) response
// ============================================================================

#[test]
fn parse_fq_short_response() -> TestResult {
    // FQ read can return a short 2-field response: band,frequency.
    let raw = b"FQ 0,0145190000";
    let r = protocol::parse(raw)?;
    let Response::Frequency { band, frequency } = r else {
        return Err(format!("expected Frequency, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(frequency, Frequency::new(145_190_000));
    Ok(())
}

// ============================================================================
// VM: Parse response
// ============================================================================

#[test]
fn parse_vm_response_memory() -> TestResult {
    let r = protocol::parse(b"VM 0,1")?;
    let Response::TuningMode { band, mode } = r else {
        return Err(format!("expected TuningMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, TuningMode::Memory);
    Ok(())
}

#[test]
fn parse_vm_response_vfo() -> TestResult {
    let r = protocol::parse(b"VM 1,0")?;
    let Response::TuningMode { band, mode } = r else {
        return Err(format!("expected TuningMode, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(mode, TuningMode::Vfo);
    Ok(())
}

#[test]
fn parse_vm_response_call() -> TestResult {
    let r = protocol::parse(b"VM 0,2")?;
    let Response::TuningMode { band, mode } = r else {
        return Err(format!("expected TuningMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, TuningMode::Call);
    Ok(())
}

#[test]
fn parse_vm_response_wx() -> TestResult {
    let r = protocol::parse(b"VM 0,3")?;
    let Response::TuningMode { band, mode } = r else {
        return Err(format!("expected TuningMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, TuningMode::Weather);
    Ok(())
}

// ============================================================================
// BC: Parse response
// ============================================================================

#[test]
fn parse_bc_response() -> TestResult {
    let r = protocol::parse(b"BC 0")?;
    let Response::Band { band } = r else {
        return Err(format!("expected BandResponse, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    Ok(())
}

#[test]
fn parse_bc_response_band_b() -> TestResult {
    let r = protocol::parse(b"BC 1")?;
    let Response::Band { band } = r else {
        return Err(format!("expected BandResponse, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    Ok(())
}
