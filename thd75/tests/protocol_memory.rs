//! Integration tests for the 3 memory protocol commands: ME, MR, 0M.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// ME — Memory channel read/write
// ============================================================================

#[test]
fn serialize_me_read() {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel { channel: 0 }),
        b"ME 000\r"
    );
}

#[test]
fn serialize_me_read_channel_99() {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel { channel: 99 }),
        b"ME 099\r"
    );
}

#[test]
fn serialize_me_read_channel_999() {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel { channel: 999 }),
        b"ME 999\r"
    );
}

#[test]
fn parse_me_response_basic() -> TestResult {
    // Real D75 ME format: all zeros, no tone/shift
    let raw = b"ME 000,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { channel, data } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 0);
    assert_eq!(data.rx_frequency, Frequency::new(145_000_000));
    assert_eq!(data.tx_offset, Frequency::new(600_000));
    assert_eq!(data.step_size, StepSize::Hz5000);
    assert!(!data.tone_enable);
    assert!(!data.reverse);
    assert_eq!(data.flags_0a_raw, 0);
    Ok(())
}

#[test]
fn parse_me_response_with_name() -> TestResult {
    // tone=1[7], ctcss=1[8], dcs=1[9], cross=0[10], rev=0[11], shift=1[12]
    let raw = b"ME 042,0440000000,0005000000,0,0,0,0,0,1,1,1,0,0,0,1,14,14,023,0,REPEATER,1,05,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { channel, data } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 42);
    assert_eq!(data.rx_frequency, Frequency::new(440_000_000));
    assert_eq!(data.urcall, ChannelName::new("REPEATER")?);
    Ok(())
}

#[test]
fn me_write_serialize() {
    let ch = ChannelMemory::default();
    let cmd = Command::SetMemoryChannel {
        channel: 5,
        data: ch,
    };
    let wire = protocol::serialize(&cmd);
    assert!(wire.starts_with(b"ME 005,"));
    assert!(wire.ends_with(b"\r"));
}

#[test]
fn me_write_serialize_full() -> TestResult {
    let ch = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::SIMPLEX,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    let wire = protocol::serialize(&Command::SetMemoryChannel {
        channel: 0,
        data: ch,
    });
    assert_eq!(
        wire,
        b"ME 000,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r"
    );
    Ok(())
}

#[test]
fn me_write_parse_round_trip() -> TestResult {
    let ch = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::SIMPLEX,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    let wire = protocol::serialize(&Command::SetMemoryChannel {
        channel: 42,
        data: ch.clone(),
    });
    // Strip trailing \r and parse
    let frame = wire.split_last().map(|(_, r)| r).ok_or("empty wire")?;
    let r = protocol::parse(frame)?;
    let Response::MemoryChannel {
        channel,
        data: parsed,
    } = r
    else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 42);
    assert_eq!(parsed, ch);
    Ok(())
}

// ============================================================================
// MR — Memory recall (action command, echoes band,channel)
// ============================================================================

#[test]
fn serialize_mr_recall_band_a() {
    assert_eq!(
        protocol::serialize(&Command::RecallMemoryChannel {
            band: Band::A,
            channel: 0
        }),
        b"MR 0,000\r"
    );
}

#[test]
fn serialize_mr_recall_band_b_channel_123() {
    assert_eq!(
        protocol::serialize(&Command::RecallMemoryChannel {
            band: Band::B,
            channel: 123
        }),
        b"MR 1,123\r"
    );
}

#[test]
fn parse_mr_echo_response() -> TestResult {
    let r = protocol::parse(b"MR 0,000")?;
    let Response::MemoryRecall { band, channel } = r else {
        return Err(format!("expected MemoryRecall, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel, 0);
    Ok(())
}

#[test]
fn parse_mr_echo_band_b() -> TestResult {
    let r = protocol::parse(b"MR 1,042")?;
    let Response::MemoryRecall { band, channel } = r else {
        return Err(format!("expected MemoryRecall, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(channel, 42);
    Ok(())
}

// ============================================================================
// MR — Read current channel (MR band read, no comma in response)
// ============================================================================

#[test]
fn serialize_mr_read_band_a() {
    assert_eq!(
        protocol::serialize(&Command::GetCurrentChannel { band: Band::A }),
        b"MR 0\r"
    );
}

#[test]
fn serialize_mr_read_band_b() {
    assert_eq!(
        protocol::serialize(&Command::GetCurrentChannel { band: Band::B }),
        b"MR 1\r"
    );
}

#[test]
fn parse_mr_read_response() -> TestResult {
    // Hardware returns `MR 021` (no comma) for `MR 0\r`
    let r = protocol::parse(b"MR 021")?;
    let Response::CurrentChannel { band, channel } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel, 21);
    Ok(())
}

#[test]
fn parse_mr_read_response_band_b() -> TestResult {
    let r = protocol::parse(b"MR 1042")?;
    let Response::CurrentChannel { band, channel } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(channel, 42);
    Ok(())
}

#[test]
fn parse_mr_read_response_channel_0() -> TestResult {
    let r = protocol::parse(b"MR 0000")?;
    let Response::CurrentChannel { band, channel } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel, 0);
    Ok(())
}

// ============================================================================
// 0M — Enter programming mode (action command)
// ============================================================================

#[test]
fn serialize_0m_enter_programming() {
    assert_eq!(
        protocol::serialize(&Command::EnterProgrammingMode),
        b"0M PROGRAM\r"
    );
}

#[test]
fn parse_0m_response() -> TestResult {
    let r = protocol::parse(b"0M somedata")?;
    assert!(
        matches!(r, Response::ProgrammingMode),
        "expected ProgrammingMode, got {r:?}"
    );
    Ok(())
}
