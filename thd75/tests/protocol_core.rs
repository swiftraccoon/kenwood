//! Integration tests for the 10 core protocol commands:
//! FQ, FO, FV, PS, ID, BE, PC, BC, VM, FR.

use kenwood_thd75::error::ProtocolError;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssMode, DcsCode, ToneCode};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint. (`::aprs` is spelled that way because the
// `types::*` glob shadows the bare crate name.)
use ::aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
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
    assert_eq!(model, "TH-D75");
    Ok(())
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
fn parse_fv_response() -> TestResult {
    let r = protocol::parse(b"FV 1.03.000")?;
    let Response::FirmwareVersion { version } = &r else {
        return Err(format!("expected FirmwareVersion, got {r:?}").into());
    };
    assert_eq!(version, "1.03.000");
    Ok(())
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
// VM: VFO/Memory mode
// ============================================================================

#[test]
fn serialize_vm_memory_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::A,
        mode: VfoMemoryMode::Memory,
    });
    assert_eq!(bytes, b"VM 0,1\r");
}

#[test]
fn serialize_vm_vfo_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::B,
        mode: VfoMemoryMode::Vfo,
    });
    assert_eq!(bytes, b"VM 1,0\r");
}

#[test]
fn serialize_vm_call_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::A,
        mode: VfoMemoryMode::Call,
    });
    assert_eq!(bytes, b"VM 0,2\r");
}

#[test]
fn serialize_vm_wx_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::A,
        mode: VfoMemoryMode::Weather,
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
    assert_eq!(channel.rx_frequency, Frequency::new(145_000_000));
    assert_eq!(channel.tx_offset, Frequency::new(600_000));
    assert_eq!(channel.step_size, StepSize::Hz5000);
    assert_eq!(channel.shift, ShiftDirection::DOWN);
    assert!(!channel.reverse());
    assert!(!channel.tone_enable());
    assert_eq!(channel.ctcss_mode(), CtcssMode::Off);
    assert!(!channel.dcs_enable());
    assert!(!channel.cross_tone_reverse());
    assert_eq!(channel.flags_0a_raw(), 0x02); // shift=2 in bits 2:0
    assert_eq!(channel.tone_code, ToneCode::new(8)?);
    assert_eq!(channel.ctcss_code, ToneCode::new(8)?);
    assert_eq!(channel.dcs_code, DcsCode::new(0)?);
    assert_eq!(channel.urcall, ChannelName::new("CQCQCQ")?);
    Ok(())
}

#[test]
fn parse_fo_response_with_name() -> TestResult {
    // 440 MHz repeater: tone+ctcss+dcs enabled, shift+, URCALL=REPEATER
    // Wire fields: step=0, tx_step=0, mode=0, fine=0, fstep=0,
    //   tone=1[7], ctcss=1[8], dcs=1[9], cross=0[10], rev=0[11], shift=1[12],
    //   tone_code=14, ctcss_code=14, dcs_code=023, combo=0, ur=REPEATER, dsq=1, code=05
    let raw = b"FO 1,0440000000,0005000000,0,0,0,0,0,1,1,1,0,0,1,14,14,023,0,REPEATER,1,05";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { band, channel } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(channel.rx_frequency, Frequency::new(440_000_000));
    assert_eq!(channel.tx_offset, Frequency::new(5_000_000));
    assert!(channel.tone_enable()); // field[7]=1
    assert!(!channel.reverse()); // field[11]=0
    // flags_0a_raw encodes: tone=1(b7), ctcss=1(b6), dcs=1(b5), shift=1(b0)
    assert_eq!(channel.flags_0a_raw(), 0xE1);
    assert_eq!(channel.tone_code, ToneCode::new(14)?);
    assert_eq!(channel.ctcss_code, ToneCode::new(14)?);
    assert_eq!(channel.dcs_code, DcsCode::new(23)?);
    assert_eq!(channel.urcall, ChannelName::new("REPEATER")?);
    Ok(())
}

#[test]
fn parse_fo_preserves_tx_step_mode_and_fine_fields() -> TestResult {
    let raw = b"FO 1,0118000000,0000000000,A,B,3,1,2,0,0,0,0,0,0,08,08,000,0,,0,00";
    let r = protocol::parse(raw)?;
    let Response::FrequencyFull { channel, .. } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(channel.step_size, StepSize::Hz50000);
    assert_eq!(channel.tx_step_size, StepSize::Hz100000);
    assert_eq!(channel.cat_mode()?, CatChannelMode::Am);
    assert!(channel.fine_tuning_enabled());
    assert_eq!(channel.fine_step()?, FineStep::Hz500);
    Ok(())
}

#[test]
fn parse_fo_rejects_noncanonical_or_out_of_domain_fields() {
    let invalid = [
        b"FO 0,0145000000,0000600000,a,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,00,0,0,0,0,0,0,0,0,08,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,2,0,0,0,0,0,08,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,8,08,000,0,,0,00".as_slice(),
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,08,08,000,4,,0,00".as_slice(),
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
    // Only 10 fields instead of 21
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
    // Parser counts channel fields (after the band argument), so the 10 raw
    // comma-separated tokens `0,...,0` collapse to `actual = 9` channel
    // fields, and `expected = 20` is the channel-field count parse_channel_fields
    // requires.
    assert_eq!(expected, 20);
    assert_eq!(actual, 9);
    Ok(())
}

#[test]
fn parse_fq_rejects_fo_shaped_response() {
    let raw = b"FQ 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    assert!(protocol::parse(raw).is_err());
}

// ============================================================================
// FO: VFO mode extended values (shift=8, etc.)
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
    assert_eq!(channel.rx_frequency, Frequency::new(145_190_000));
    // field[12]=2 → shift direction in flags_0a_raw bits 2:0
    assert_eq!(channel.flags_0a_raw() & 0x07, 2);
    assert_eq!(channel.urcall, ChannelName::new("CQCQCQ")?);
    Ok(())
}

#[test]
fn parse_fo_vfo_mode_all_extended_shift_values() {
    // Verify shift values 0-7 at field[12] parse successfully.
    for shift_val in 0u8..=7 {
        let raw = format!(
            "FO 0,0145190000,0000600000,0,0,0,0,0,0,0,0,0,0,{shift_val},08,08,000,0,CQCQCQ,0,00"
        );
        let r = protocol::parse(raw.as_bytes());
        assert!(r.is_ok(), "FO parse failed for shift={shift_val}: {r:?}");
    }
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
    let Response::VfoMemoryMode { band, mode } = r else {
        return Err(format!("expected VfoMemoryMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, VfoMemoryMode::Memory);
    Ok(())
}

#[test]
fn parse_vm_response_vfo() -> TestResult {
    let r = protocol::parse(b"VM 1,0")?;
    let Response::VfoMemoryMode { band, mode } = r else {
        return Err(format!("expected VfoMemoryMode, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(mode, VfoMemoryMode::Vfo);
    Ok(())
}

#[test]
fn parse_vm_response_call() -> TestResult {
    let r = protocol::parse(b"VM 0,2")?;
    let Response::VfoMemoryMode { band, mode } = r else {
        return Err(format!("expected VfoMemoryMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, VfoMemoryMode::Call);
    Ok(())
}

#[test]
fn parse_vm_response_wx() -> TestResult {
    let r = protocol::parse(b"VM 0,3")?;
    let Response::VfoMemoryMode { band, mode } = r else {
        return Err(format!("expected VfoMemoryMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, VfoMemoryMode::Weather);
    Ok(())
}

// ============================================================================
// BC: Parse response
// ============================================================================

#[test]
fn parse_bc_response() -> TestResult {
    let r = protocol::parse(b"BC 0")?;
    let Response::BandResponse { band } = r else {
        return Err(format!("expected BandResponse, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    Ok(())
}

#[test]
fn parse_bc_response_band_b() -> TestResult {
    let r = protocol::parse(b"BC 1")?;
    let Response::BandResponse { band } = r else {
        return Err(format!("expected BandResponse, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    Ok(())
}
