//! Integration tests for the 10 core protocol commands:
//! FQ, FO, FV, PS, ID, BE, PC, BC, VM, FR.

use kenwood_thd75::error::ProtocolError;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssMode, DcsCode, ToneCode};
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Strip the trailing `\r` from a serialized wire frame, returning an error if empty.
fn strip_cr(bytes: &[u8]) -> Result<&[u8], Box<dyn std::error::Error>> {
    bytes
        .split_last()
        .map(|(_, rest)| rest)
        .ok_or_else(|| "wire frame unexpectedly empty".into())
}

// ============================================================================
// ID — Radio model identification
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
// FV — Firmware version
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
// PS — Power status
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
// BE — Beep on/off (moved to control tests, but keep basic test here)
// ============================================================================

#[test]
fn serialize_be_read() {
    let bytes = protocol::serialize(&Command::GetBeep);
    assert_eq!(bytes, b"BE\r");
}

#[test]
fn parse_be_response() -> TestResult {
    let r = protocol::parse(b"BE 0")?;
    assert!(
        matches!(r, Response::Beep { enabled: false }),
        "expected Beep{{false}}, got {r:?}"
    );
    Ok(())
}

// ============================================================================
// PC — Power level
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
// BC — Band read/set
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
// VM — VFO/Memory mode
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
// FR — FM radio on/off
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
// FQ — Quick frequency
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
// FO — Full frequency and settings (21 comma-separated fields)
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
    assert!(!channel.reverse);
    assert!(!channel.tone_enable);
    assert_eq!(channel.ctcss_mode, CtcssMode::Off);
    assert!(!channel.dcs_enable);
    assert!(!channel.cross_tone_reverse);
    assert_eq!(channel.flags_0a_raw, 0x02); // shift=2 in bits 2:0
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
    assert!(channel.tone_enable); // field[7]=1
    assert!(!channel.reverse); // field[11]=0
    // flags_0a_raw encodes: tone=1(b7), ctcss=1(b6), dcs=1(b5), shift=1(b0)
    assert_eq!(channel.flags_0a_raw, 0xE1);
    assert_eq!(channel.tone_code, ToneCode::new(14)?);
    assert_eq!(channel.ctcss_code, ToneCode::new(14)?);
    assert_eq!(channel.dcs_code, DcsCode::new(23)?);
    assert_eq!(channel.urcall, ChannelName::new("REPEATER")?);
    Ok(())
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
fn serialize_fo_write() -> TestResult {
    // Construct a channel matching real D75 hardware output for 145 MHz simplex with shift-
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::DOWN,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0x02, // shift- = bits 1:0 = 2
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel,
    });
    assert_eq!(
        bytes,
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r"
    );
    Ok(())
}

#[test]
fn fo_write_parse_round_trip() -> TestResult {
    // Round-trip: serialize → parse → compare
    // flags_0a_raw encodes all the tone/shift wire fields
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::DOWN,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0x02, // shift- = 2
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel: channel.clone(),
    });
    let frame = strip_cr(&bytes)?;
    let r = protocol::parse(frame)?;
    let Response::FrequencyFull {
        band,
        channel: parsed,
    } = r
    else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(parsed.rx_frequency, channel.rx_frequency);
    assert_eq!(parsed.flags_0a_raw, channel.flags_0a_raw);
    assert_eq!(parsed.urcall, channel.urcall);
    Ok(())
}

#[test]
fn fo_flags_0a_raw_round_trip() -> TestResult {
    // Verify that flags_0a_raw bits are preserved through serialize/parse
    let channel = ChannelMemory {
        flags_0a_raw: 0x2B, // bits: 10_1011 => x2=1, x3=0, x4=1, x5=3 (011)
        ..ChannelMemory::default()
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel,
    });
    let frame = strip_cr(&bytes)?;
    let r = protocol::parse(frame)?;
    let Response::FrequencyFull {
        channel: parsed, ..
    } = r
    else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(parsed.flags_0a_raw, 0x2B);
    Ok(())
}

#[test]
fn fo_flags_0a_raw_clamped_on_overflow() -> TestResult {
    // Serialize a channel with flags_0a_raw = 0xFF (all bits set),
    // parse it back and verify the shift field (bits 2:0) is masked to 0x07.
    // In the new wire format, byte[10] is unpacked as 6 fields:
    // tone(7), ctcss(8), dcs(9), cross(10), reverse(11), shift(12)
    // where shift combines bits 2:0 as a single value.
    let channel = ChannelMemory {
        flags_0a_raw: 0xFF,
        ..ChannelMemory::default()
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel,
    });
    let frame = strip_cr(&bytes)?;
    let r = protocol::parse(frame)?;
    let Response::FrequencyFull {
        channel: parsed, ..
    } = r
    else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    // All boolean fields = 1, shift = 7 (0x07)
    // flags_0a_raw = (1<<7)|(1<<6)|(1<<5)|(1<<4)|(1<<3)|7 = 0xFF
    assert_eq!(parsed.flags_0a_raw, 0xFF);
    Ok(())
}

#[test]
fn serialize_fq_write() -> TestResult {
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::DOWN,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0x02, // shift- = 2
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequency {
        band: Band::A,
        channel,
    });
    assert_eq!(
        bytes,
        b"FQ 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r"
    );
    Ok(())
}

#[test]
fn parse_fq_response_21_fields() -> TestResult {
    let raw = b"FQ 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(raw)?;
    let Response::Frequency { band, channel } = r else {
        return Err(format!("expected Frequency, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel.rx_frequency, Frequency::new(145_000_000));
    Ok(())
}

// ============================================================================
// FO — VFO mode extended values (shift=8, etc.)
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
    assert_eq!(channel.flags_0a_raw & 0x07, 2);
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

#[test]
fn fo_vfo_extended_shift_round_trip() -> TestResult {
    // Serialize with shift=5 (extended value in bits 2:0), parse back, verify.
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_190_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::new(5)?,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0x05, // shift=5 in bits 2:0
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel: channel.clone(),
    });
    let frame = strip_cr(&bytes)?;
    let r = protocol::parse(frame)?;
    let Response::FrequencyFull {
        band,
        channel: parsed,
    } = r
    else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(parsed.rx_frequency, channel.rx_frequency);
    assert_eq!(parsed.flags_0a_raw & 0x07, 5);
    assert_eq!(parsed.urcall, channel.urcall);
    Ok(())
}

#[test]
fn fo_vfo_tune_frequency_simulation() -> TestResult {
    // Simulate the tune_frequency flow: read FO, modify freq, serialize, parse.
    // Start with a VFO state that has shift=2 (shift-) at field[12].
    let vfo_response = b"FO 0,0145190000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(vfo_response)?;
    let Response::FrequencyFull { mut channel, .. } = r else {
        return Err(format!("expected FrequencyFull, got {r:?}").into());
    };

    // Modify only the frequency (what tune_frequency does).
    let new_freq = Frequency::new(146_520_000);
    channel.rx_frequency = new_freq;

    // Serialize the modified channel back (FO write).
    let write_bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel,
    });

    // Parse the written data to verify round-trip.
    let frame = strip_cr(&write_bytes)?;
    let r2 = protocol::parse(frame)?;
    let Response::FrequencyFull {
        channel: written, ..
    } = r2
    else {
        return Err(format!("expected FrequencyFull, got {r2:?}").into());
    };
    // New frequency should be set.
    assert_eq!(written.rx_frequency, new_freq);
    // Shift direction should be preserved (field[12]=2 → bits 2:0 = 2).
    assert_eq!(written.flags_0a_raw & 0x07, 2);
    // All other fields should be preserved.
    assert_eq!(written.tx_offset, Frequency::new(600_000));
    assert_eq!(written.urcall, ChannelName::new("CQCQCQ")?);
    Ok(())
}

// ============================================================================
// FQ — Short (2-field) response
// ============================================================================

#[test]
fn parse_fq_short_response() -> TestResult {
    // FQ read can return a short 2-field response: band,frequency.
    let raw = b"FQ 0,0145190000";
    let r = protocol::parse(raw)?;
    let Response::Frequency { band, channel } = r else {
        return Err(format!("expected Frequency, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel.rx_frequency, Frequency::new(145_190_000));
    Ok(())
}

// ============================================================================
// VM — Parse response
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
// BC — Parse response
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
