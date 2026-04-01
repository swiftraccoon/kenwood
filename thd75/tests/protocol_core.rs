//! Integration tests for the 10 core protocol commands:
//! FQ, FO, FV, PS, ID, BE, PC, BC, VM, FR.

use kenwood_thd75::error::ProtocolError;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssMode, DataSpeed, DcsCode, LockoutMode, ToneCode};
use kenwood_thd75::types::*;

// ============================================================================
// ID — Radio model identification
// ============================================================================

#[test]
fn serialize_id_read() {
    let bytes = protocol::serialize(&Command::GetRadioId);
    assert_eq!(bytes, b"ID\r");
}

#[test]
fn parse_id_response() {
    let r = protocol::parse(b"ID TH-D75").unwrap();
    match r {
        Response::RadioId { model } => assert_eq!(model, "TH-D75"),
        other => panic!("expected RadioId, got {other:?}"),
    }
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
fn parse_fv_response() {
    let r = protocol::parse(b"FV 1.03.000").unwrap();
    match r {
        Response::FirmwareVersion { version } => assert_eq!(version, "1.03.000"),
        other => panic!("expected FirmwareVersion, got {other:?}"),
    }
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
fn parse_ps_on() {
    let r = protocol::parse(b"PS 1").unwrap();
    match r {
        Response::PowerStatus { on } => assert!(on),
        other => panic!("expected PowerStatus, got {other:?}"),
    }
}

#[test]
fn parse_ps_off() {
    let r = protocol::parse(b"PS 0").unwrap();
    match r {
        Response::PowerStatus { on } => assert!(!on),
        other => panic!("expected PowerStatus, got {other:?}"),
    }
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
fn parse_be_response() {
    let r = protocol::parse(b"BE 0").unwrap();
    assert!(matches!(r, Response::Beep { enabled: false }));
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
fn parse_pc_response() {
    let r = protocol::parse(b"PC 0,2").unwrap();
    match r {
        Response::PowerLevel { band, level } => {
            assert_eq!(band, Band::A);
            assert_eq!(level, PowerLevel::Low);
        }
        other => panic!("expected PowerLevel, got {other:?}"),
    }
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
        mode: 1,
    });
    assert_eq!(bytes, b"VM 0,1\r");
}

#[test]
fn serialize_vm_vfo_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::B,
        mode: 0,
    });
    assert_eq!(bytes, b"VM 1,0\r");
}

#[test]
fn serialize_vm_call_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::A,
        mode: 2,
    });
    assert_eq!(bytes, b"VM 0,2\r");
}

#[test]
fn serialize_vm_wx_mode() {
    let bytes = protocol::serialize(&Command::SetVfoMemoryMode {
        band: Band::A,
        mode: 3,
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
fn parse_fr_response_off() {
    let r = protocol::parse(b"FR 0").unwrap();
    match r {
        Response::FmRadio { enabled } => assert!(!enabled),
        other => panic!("expected FmRadio, got {other:?}"),
    }
}

#[test]
fn parse_fr_response_on() {
    let r = protocol::parse(b"FR 1").unwrap();
    match r {
        Response::FmRadio { enabled } => assert!(enabled),
        other => panic!("expected FmRadio, got {other:?}"),
    }
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
fn parse_fo_response_21_fields() {
    let raw = b"FO 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::FrequencyFull { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel.rx_frequency, Frequency::new(145_000_000));
            assert_eq!(channel.tx_offset, Frequency::new(600_000));
            assert_eq!(channel.step_size, StepSize::Hz12500);
            assert_eq!(channel.shift, ShiftDirection::UP);
            assert!(!channel.reverse);
            assert!(channel.tone_enable);
            assert_eq!(channel.ctcss_mode, CtcssMode::Off);
            assert!(!channel.dcs_enable);
            assert!(!channel.cross_tone_reverse);
            assert_eq!(channel.tone_code, ToneCode::new(8).unwrap());
            assert_eq!(channel.ctcss_code, ToneCode::new(8).unwrap());
            assert_eq!(channel.dcs_code, DcsCode::new(0).unwrap());
            assert_eq!(channel.data_speed, DataSpeed::Bps1200);
            assert_eq!(channel.urcall, ChannelName::new("").unwrap());
            assert_eq!(channel.lockout, LockoutMode::Off);
            assert_eq!(channel.data_mode, 0);
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

#[test]
fn parse_fo_response_with_name() {
    let raw = b"FO 1,0440000000,0005000000,5,2,1,0,1,1,1,0,0,0,0,14,14,023,1,REPEATER,1,05";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::FrequencyFull { band, channel } => {
            assert_eq!(band, Band::B);
            assert_eq!(channel.rx_frequency, Frequency::new(440_000_000));
            assert_eq!(channel.tx_offset, Frequency::new(5_000_000));
            assert_eq!(channel.step_size, StepSize::Hz12500);
            assert_eq!(channel.shift, ShiftDirection::DOWN);
            assert!(channel.reverse);
            assert!(!channel.tone_enable);
            assert_eq!(channel.ctcss_mode, CtcssMode::On);
            assert!(channel.dcs_enable);
            assert!(channel.cross_tone_reverse);
            assert_eq!(channel.tone_code, ToneCode::new(14).unwrap());
            assert_eq!(channel.ctcss_code, ToneCode::new(14).unwrap());
            assert_eq!(channel.dcs_code, DcsCode::new(23).unwrap());
            assert_eq!(channel.data_speed, DataSpeed::Bps9600);
            assert_eq!(channel.urcall, ChannelName::new("REPEATER").unwrap());
            assert_eq!(channel.lockout, LockoutMode::On);
            assert_eq!(channel.data_mode, 5);
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

#[test]
fn parse_fo_wrong_field_count() {
    // Only 10 fields instead of 21
    let raw = b"FO 0,0145000000,0000600000,5,1,0,1,0,0,0";
    let r = protocol::parse(raw);
    assert!(r.is_err());
    match r.unwrap_err() {
        ProtocolError::FieldCount {
            command,
            expected,
            actual,
        } => {
            assert_eq!(command, "FO");
            assert_eq!(expected, 21);
            assert_eq!(actual, 10);
        }
        other => panic!("expected FieldCount, got {other:?}"),
    }
}

#[test]
fn serialize_fo_write() {
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz12500,
        shift: ShiftDirection::UP,
        reverse: false,
        tone_enable: true,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8).unwrap(),
        ctcss_code: ToneCode::new(8).unwrap(),
        dcs_code: DcsCode::new(0).unwrap(),
        data_speed: DataSpeed::Bps1200,
        lockout: LockoutMode::Off,
        urcall: ChannelName::new("").unwrap(),
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel,
    });
    assert_eq!(
        bytes,
        b"FO 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00\r"
    );
}

#[test]
fn fo_write_parse_round_trip() {
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz12500,
        shift: ShiftDirection::UP,
        reverse: false,
        tone_enable: true,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8).unwrap(),
        ctcss_code: ToneCode::new(8).unwrap(),
        dcs_code: DcsCode::new(0).unwrap(),
        data_speed: DataSpeed::Bps1200,
        lockout: LockoutMode::Off,
        urcall: ChannelName::new("").unwrap(),
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel: channel.clone(),
    });
    // Strip the trailing \r and parse
    let frame = &bytes[..bytes.len() - 1];
    let r = protocol::parse(frame).unwrap();
    match r {
        Response::FrequencyFull {
            band,
            channel: parsed,
        } => {
            assert_eq!(band, Band::A);
            assert_eq!(parsed, channel);
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

#[test]
fn fo_flags_0a_raw_round_trip() {
    // Verify that flags_0a_raw bits are preserved through serialize/parse
    let channel = ChannelMemory {
        flags_0a_raw: 0x2B, // bits: 10_1011 => x2=1, x3=0, x4=1, x5=3 (011)
        ..ChannelMemory::default()
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel: channel.clone(),
    });
    let frame = &bytes[..bytes.len() - 1];
    let r = protocol::parse(frame).unwrap();
    match r {
        Response::FrequencyFull {
            channel: parsed, ..
        } => {
            assert_eq!(parsed.flags_0a_raw, 0x2B);
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

#[test]
fn serialize_fq_write() {
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz12500,
        shift: ShiftDirection::UP,
        reverse: false,
        tone_enable: true,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8).unwrap(),
        ctcss_code: ToneCode::new(8).unwrap(),
        dcs_code: DcsCode::new(0).unwrap(),
        data_speed: DataSpeed::Bps1200,
        lockout: LockoutMode::Off,
        urcall: ChannelName::new("").unwrap(),
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequency {
        band: Band::A,
        channel,
    });
    // FQ write should produce same format as FO write but with FQ mnemonic
    assert_eq!(
        bytes,
        b"FQ 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00\r"
    );
}

#[test]
fn parse_fq_response_21_fields() {
    let raw = b"FQ 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::Frequency { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel.rx_frequency, Frequency::new(145_000_000));
        }
        other => panic!("expected Frequency, got {other:?}"),
    }
}

// ============================================================================
// FO — VFO mode extended values (shift=8, etc.)
// ============================================================================

#[test]
fn parse_fo_vfo_mode_extended_shift() {
    // VFO mode can return shift values outside the normal 0-3 range.
    // This response has shift=8, which the parser must accept.
    let raw = b"FO 0,0145190000,0000600000,0,8,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::FrequencyFull { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel.rx_frequency, Frequency::new(145_190_000));
            assert_eq!(channel.shift, ShiftDirection::new(8).unwrap());
            assert!(!channel.shift.is_known());
            assert_eq!(channel.urcall, ChannelName::new("CQCQCQ").unwrap());
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

#[test]
fn parse_fo_vfo_mode_all_extended_shift_values() {
    // Verify every 4-bit shift value (0-15) parses successfully.
    for shift_val in 0u8..=15 {
        let raw = format!(
            "FO 0,0145190000,0000600000,0,{shift_val},0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00"
        );
        let r = protocol::parse(raw.as_bytes());
        assert!(
            r.is_ok(),
            "FO parse failed for shift={shift_val}: {:?}",
            r.unwrap_err()
        );
    }
}

#[test]
fn fo_vfo_extended_shift_round_trip() {
    // Serialize a VFO state with shift=8, parse it back, verify round-trip.
    let channel = ChannelMemory {
        rx_frequency: Frequency::new(145_190_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        shift: ShiftDirection::new(8).unwrap(),
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8).unwrap(),
        ctcss_code: ToneCode::new(8).unwrap(),
        dcs_code: DcsCode::new(0).unwrap(),
        data_speed: DataSpeed::Bps1200,
        lockout: LockoutMode::Off,
        urcall: ChannelName::new("CQCQCQ").unwrap(),
        data_mode: 0,
    };
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel: channel.clone(),
    });
    // Strip trailing \r and parse
    let frame = &bytes[..bytes.len() - 1];
    let r = protocol::parse(frame).unwrap();
    match r {
        Response::FrequencyFull {
            band,
            channel: parsed,
        } => {
            assert_eq!(band, Band::A);
            assert_eq!(parsed.rx_frequency, channel.rx_frequency);
            assert_eq!(parsed.shift, ShiftDirection::new(8).unwrap());
            assert_eq!(parsed.urcall, channel.urcall);
            assert_eq!(parsed, channel);
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

#[test]
fn fo_vfo_tune_frequency_simulation() {
    // Simulate the tune_frequency flow: read FO, modify freq, serialize, parse.
    // Start with a VFO state that has extended shift=8.
    let vfo_response = b"FO 0,0145190000,0000600000,0,8,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(vfo_response).unwrap();
    let mut channel = match r {
        Response::FrequencyFull { channel, .. } => channel,
        other => panic!("expected FrequencyFull, got {other:?}"),
    };

    // Modify only the frequency (what tune_frequency does).
    let new_freq = Frequency::new(146_520_000);
    channel.rx_frequency = new_freq;

    // Serialize the modified channel back (FO write).
    let write_bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel: channel.clone(),
    });

    // Parse the written data to verify round-trip.
    let frame = &write_bytes[..write_bytes.len() - 1];
    let r2 = protocol::parse(frame).unwrap();
    match r2 {
        Response::FrequencyFull {
            channel: written, ..
        } => {
            // New frequency should be set.
            assert_eq!(written.rx_frequency, new_freq);
            // Extended shift should be preserved.
            assert_eq!(written.shift, ShiftDirection::new(8).unwrap());
            // All other fields should be preserved.
            assert_eq!(written.tx_offset, Frequency::new(600_000));
            assert_eq!(written.urcall, ChannelName::new("CQCQCQ").unwrap());
            assert_eq!(written.flags_0a_raw, channel.flags_0a_raw);
        }
        other => panic!("expected FrequencyFull, got {other:?}"),
    }
}

// ============================================================================
// FQ — Short (2-field) response
// ============================================================================

#[test]
fn parse_fq_short_response() {
    // FQ read can return a short 2-field response: band,frequency.
    let raw = b"FQ 0,0145190000";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::Frequency { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel.rx_frequency, Frequency::new(145_190_000));
        }
        other => panic!("expected Frequency, got {other:?}"),
    }
}

// ============================================================================
// VM — Parse response
// ============================================================================

#[test]
fn parse_vm_response_memory() {
    let r = protocol::parse(b"VM 0,1").unwrap();
    match r {
        Response::VfoMemoryMode { band, mode } => {
            assert_eq!(band, Band::A);
            assert_eq!(mode, 1);
        }
        other => panic!("expected VfoMemoryMode, got {other:?}"),
    }
}

#[test]
fn parse_vm_response_vfo() {
    let r = protocol::parse(b"VM 1,0").unwrap();
    match r {
        Response::VfoMemoryMode { band, mode } => {
            assert_eq!(band, Band::B);
            assert_eq!(mode, 0);
        }
        other => panic!("expected VfoMemoryMode, got {other:?}"),
    }
}

#[test]
fn parse_vm_response_call() {
    let r = protocol::parse(b"VM 0,2").unwrap();
    match r {
        Response::VfoMemoryMode { band, mode } => {
            assert_eq!(band, Band::A);
            assert_eq!(mode, 2);
        }
        other => panic!("expected VfoMemoryMode, got {other:?}"),
    }
}

#[test]
fn parse_vm_response_wx() {
    let r = protocol::parse(b"VM 0,3").unwrap();
    match r {
        Response::VfoMemoryMode { band, mode } => {
            assert_eq!(band, Band::A);
            assert_eq!(mode, 3);
        }
        other => panic!("expected VfoMemoryMode, got {other:?}"),
    }
}

// ============================================================================
// BC — Parse response
// ============================================================================

#[test]
fn parse_bc_response() {
    let r = protocol::parse(b"BC 0").unwrap();
    match r {
        Response::BandResponse { band } => assert_eq!(band, Band::A),
        other => panic!("expected BandResponse, got {other:?}"),
    }
}

#[test]
fn parse_bc_response_band_b() {
    let r = protocol::parse(b"BC 1").unwrap();
    match r {
        Response::BandResponse { band } => assert_eq!(band, Band::B),
        other => panic!("expected BandResponse, got {other:?}"),
    }
}
