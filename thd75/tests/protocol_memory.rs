//! Integration tests for the 3 memory protocol commands: ME, MR, 0M.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
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
// ME: Memory channel read/write
// ============================================================================

#[test]
fn serialize_me_read() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: MemorySelector::channel(0)?,
        }),
        b"ME 000\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_channel_99() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: MemorySelector::channel(99)?,
        }),
        b"ME 099\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_channel_999() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: MemorySelector::channel(999)?,
        }),
        b"ME 999\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_program_scan_edge() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: MemorySelector::program_lower(7)?,
        }),
        b"ME L07\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_priority() {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: MemorySelector::PRIORITY,
        }),
        b"ME Pri\r"
    );
}

#[test]
fn parse_me_response_basic() -> TestResult {
    // Real D75 ME format: all zeros, no tone/shift
    let raw = b"ME 000,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { selector, record } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(selector.channel_number(), Some(0));
    assert_eq!(record.rx_frequency, Frequency::new(145_000_000));
    assert_eq!(record.tx_offset, Frequency::new(600_000));
    assert_eq!(record.step_size, StepSize::Hz5000);
    assert!(!record.tone_enable());
    assert!(!record.reverse());
    assert_eq!(record.flags_0a_raw(), 0);
    assert_eq!(record.me_field_14_raw, "0");
    assert_eq!(record.me_field_22_raw, "0");
    Ok(())
}

#[test]
fn parse_me_response_with_name() -> TestResult {
    // tone=1[7], ctcss=1[8], dcs=1[9], cross=0[10], rev=0[11], shift=1[12]
    let raw = b"ME 042,0440000000,0005000000,0,0,0,0,0,1,1,1,0,0,0,1,14,14,023,0,REPEATER,1,05,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { selector, record } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(selector.channel_number(), Some(42));
    assert_eq!(record.rx_frequency, Frequency::new(440_000_000));
    assert_eq!(record.urcall, ChannelName::new("REPEATER")?);
    assert_eq!(record.me_field_14_raw, "1");
    assert_eq!(record.me_field_22_raw, "0");
    Ok(())
}

// ============================================================================
// MR: Memory recall (action command, echoes band,channel)
// ============================================================================

#[test]
fn serialize_mr_recall_band_a() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::RecallMemoryChannel {
            band: Band::A,
            selector: MemorySelector::channel(0)?,
        }),
        b"MR 0,000\r"
    );
    Ok(())
}

#[test]
fn serialize_mr_recall_band_b_channel_123() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::RecallMemoryChannel {
            band: Band::B,
            selector: MemorySelector::channel(123)?,
        }),
        b"MR 1,123\r"
    );
    Ok(())
}

#[test]
fn parse_mr_echo_response() -> TestResult {
    let r = protocol::parse(b"MR 0,000")?;
    let Response::MemoryRecall { band, selector } = r else {
        return Err(format!("expected MemoryRecall, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(selector.channel_number(), Some(0));
    Ok(())
}

#[test]
fn parse_mr_echo_band_b() -> TestResult {
    let r = protocol::parse(b"MR 1,042")?;
    let Response::MemoryRecall { band, selector } = r else {
        return Err(format!("expected MemoryRecall, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(selector.channel_number(), Some(42));
    Ok(())
}

// ============================================================================
// MR: Read current channel (MR band read, no comma in response)
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
    let Response::CurrentChannel { selector } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(selector.channel_number(), Some(21));
    Ok(())
}

#[test]
fn parse_mr_read_response_program_scan_edge() -> TestResult {
    let r = protocol::parse(b"MR U42")?;
    let Response::CurrentChannel { selector } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(selector, MemorySelector::program_upper(42)?);
    Ok(())
}

#[test]
fn parse_mr_read_response_priority() -> TestResult {
    let r = protocol::parse(b"MR Pri")?;
    let Response::CurrentChannel { selector } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(selector, MemorySelector::PRIORITY);
    Ok(())
}

#[test]
fn memory_selector_accepts_all_documented_boundaries() -> TestResult {
    let valid = [
        ("000", MemorySelector::channel(0)?),
        ("999", MemorySelector::channel(999)?),
        ("L00", MemorySelector::program_lower(0)?),
        ("L49", MemorySelector::program_lower(49)?),
        ("U00", MemorySelector::program_upper(0)?),
        ("U49", MemorySelector::program_upper(49)?),
        ("T01", MemorySelector::regional_t(1)?),
        ("T30", MemorySelector::regional_t(30)?),
        ("A01", MemorySelector::regional_a(1)?),
        ("A10", MemorySelector::regional_a(10)?),
        ("Pri", MemorySelector::PRIORITY),
    ];

    for (wire, expected) in valid {
        let selector = MemorySelector::try_from(wire)?;
        assert_eq!(selector, expected);
        assert_eq!(selector.to_string(), wire);
    }
    Ok(())
}

#[test]
fn memory_selector_rejects_noncanonical_and_out_of_range_values() {
    for wire in [
        "", "00", "0000", "L50", "U50", "T00", "T31", "A00", "A11", "PRI", "pri", "-01",
    ] {
        assert!(
            MemorySelector::try_from(wire).is_err(),
            "invalid selector was accepted: {wire:?}"
        );
    }
}

// ============================================================================
// 0M: Enter programming mode (action command)
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
