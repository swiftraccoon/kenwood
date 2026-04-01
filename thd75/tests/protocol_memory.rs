//! Integration tests for the 3 memory protocol commands: ME, MR, 0M.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

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
fn parse_me_response_basic() {
    let raw = b"ME 000,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::MemoryChannel { channel, data } => {
            assert_eq!(channel, 0);
            assert_eq!(data.rx_frequency, Frequency::new(145_000_000));
            assert_eq!(data.tx_offset, Frequency::new(600_000));
            assert_eq!(data.step_size, StepSize::Hz12500);
            assert_eq!(data.shift, ShiftDirection::UP);
            assert!(!data.reverse);
            assert!(data.tone_enable);
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
}

#[test]
fn parse_me_response_with_name() {
    let raw = b"ME 042,0440000000,0005000000,5,2,1,0,1,1,1,0,0,0,0,0,14,14,023,1,REPEATER,1,05,0";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::MemoryChannel { channel, data } => {
            assert_eq!(channel, 42);
            assert_eq!(data.rx_frequency, Frequency::new(440_000_000));
            assert_eq!(data.urcall, ChannelName::new("REPEATER").unwrap());
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
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
fn me_write_serialize_full() {
    let ch = ChannelMemory {
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
    let wire = protocol::serialize(&Command::SetMemoryChannel {
        channel: 0,
        data: ch,
    });
    assert_eq!(
        wire,
        b"ME 000,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r"
    );
}

#[test]
fn me_write_parse_round_trip() {
    let ch = ChannelMemory {
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
    let wire = protocol::serialize(&Command::SetMemoryChannel {
        channel: 42,
        data: ch.clone(),
    });
    // Strip trailing \r and parse
    let frame = &wire[..wire.len() - 1];
    let r = protocol::parse(frame).unwrap();
    match r {
        Response::MemoryChannel {
            channel,
            data: parsed,
        } => {
            assert_eq!(channel, 42);
            assert_eq!(parsed, ch);
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
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
fn parse_mr_echo_response() {
    let r = protocol::parse(b"MR 0,000").unwrap();
    match r {
        Response::MemoryRecall { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel, 0);
        }
        other => panic!("expected MemoryRecall, got {other:?}"),
    }
}

#[test]
fn parse_mr_echo_band_b() {
    let r = protocol::parse(b"MR 1,042").unwrap();
    match r {
        Response::MemoryRecall { band, channel } => {
            assert_eq!(band, Band::B);
            assert_eq!(channel, 42);
        }
        other => panic!("expected MemoryRecall, got {other:?}"),
    }
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
fn parse_mr_read_response() {
    // Hardware returns `MR 021` (no comma) for `MR 0\r`
    let r = protocol::parse(b"MR 021").unwrap();
    match r {
        Response::CurrentChannel { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel, 21);
        }
        other => panic!("expected CurrentChannel, got {other:?}"),
    }
}

#[test]
fn parse_mr_read_response_band_b() {
    let r = protocol::parse(b"MR 1042").unwrap();
    match r {
        Response::CurrentChannel { band, channel } => {
            assert_eq!(band, Band::B);
            assert_eq!(channel, 42);
        }
        other => panic!("expected CurrentChannel, got {other:?}"),
    }
}

#[test]
fn parse_mr_read_response_channel_0() {
    let r = protocol::parse(b"MR 0000").unwrap();
    match r {
        Response::CurrentChannel { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel, 0);
        }
        other => panic!("expected CurrentChannel, got {other:?}"),
    }
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
fn parse_0m_response() {
    let r = protocol::parse(b"0M somedata").unwrap();
    assert!(
        matches!(r, Response::ProgrammingMode),
        "expected ProgrammingMode, got {r:?}"
    );
}
