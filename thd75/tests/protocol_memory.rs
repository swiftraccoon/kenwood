//! Integration tests for the generic ME and MR memory commands.

use kenwood_thd75::error::ProtocolError;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
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
// ME: Memory channel read/write
// ============================================================================

#[test]
fn serialize_me_read() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: RegularChannel::new(0)?.into(),
        }),
        b"ME 000\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_channel_99() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: RegularChannel::new(99)?.into(),
        }),
        b"ME 099\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_channel_999() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: RegularChannel::new(999)?.into(),
        }),
        b"ME 999\r"
    );
    Ok(())
}

#[test]
fn serialize_me_read_program_scan_edge() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::GetMemoryChannel {
            selector: MemoryChannelAddress::program_lower(7)?,
        }),
        b"ME L07\r"
    );
    Ok(())
}

#[test]
fn memory_channel_address_rejects_output_only_priority_label() {
    assert!(MemoryChannelAddress::try_from("Pri").is_err());
}

#[test]
fn parse_me_response_basic() -> TestResult {
    // Real D75 ME format: all zeros, no tone/shift
    let raw = b"ME 000,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { selector, record } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(selector.regular_channel(), Some(RegularChannel::new(0)?));
    assert_eq!(
        record.channel.receive_frequency,
        Frequency::new(145_000_000)
    );
    assert_eq!(
        record.channel.transmit_offset_or_frequency,
        Frequency::new(600_000)
    );
    assert_eq!(record.channel.receive_step, StepSize::Hz5000);
    assert_eq!(record.channel.tone_mode, ToneMode::Off);
    assert!(!record.channel.reverse);
    assert_eq!(record.channel.shift, ShiftDirection::Simplex);
    assert!(!record.split);
    assert!(!record.scan_lockout);
    assert_eq!(
        record.transmit_value(),
        ChannelTransmitValue::RepeaterOffset(Frequency::new(600_000)),
    );
    Ok(())
}

#[test]
fn parse_me_response_with_name() -> TestResult {
    // Exactly one tone mode is active. ME field 13 is split, field 14 is
    // shift, and field 22 is scan lockout.
    let raw = b"ME 042,0440000000,0005000000,0,0,0,0,0,1,0,0,0,0,1,2,14,14,023,0,REPEATER,1,05,1";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { selector, record } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(selector.regular_channel(), Some(RegularChannel::new(42)?));
    assert_eq!(
        record.channel.receive_frequency,
        Frequency::new(440_000_000)
    );
    assert_eq!(record.channel.tone_mode, ToneMode::Tone);
    assert_eq!(record.channel.shift, ShiftDirection::Minus);
    assert_eq!(record.channel.ur_call, DstarCallsign::new("REPEATER")?);
    assert!(record.split);
    assert!(record.scan_lockout);
    assert_eq!(
        record.transmit_value(),
        ChannelTransmitValue::SplitTransmitFrequency(Frequency::new(5_000_000)),
    );
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
            selector: RegularChannel::new(0)?.into(),
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
            selector: RegularChannel::new(123)?.into(),
        }),
        b"MR 1,123\r"
    );
    Ok(())
}

#[test]
fn parse_mr_echo_response() -> TestResult {
    let r = protocol::parse(b"MR 0,000")?;
    let Response::MemoryRecallAck { band, selector } = r else {
        return Err(format!("expected MemoryRecall, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(selector.regular_channel(), Some(RegularChannel::new(0)?));
    Ok(())
}

#[test]
fn parse_mr_echo_band_b() -> TestResult {
    let r = protocol::parse(b"MR 1,042")?;
    let Response::MemoryRecallAck { band, selector } = r else {
        return Err(format!("expected MemoryRecall, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(selector.regular_channel(), Some(RegularChannel::new(42)?));
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
    assert_eq!(selector.regular_channel(), Some(RegularChannel::new(21)?));
    Ok(())
}

#[test]
fn parse_mr_read_response_program_scan_edge() -> TestResult {
    let r = protocol::parse(b"MR U42")?;
    let Response::CurrentChannel { selector } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(
        selector,
        CurrentMemorySelector::Address(MemoryChannelAddress::program_upper(42)?),
    );
    Ok(())
}

#[test]
fn parse_mr_read_response_priority() -> TestResult {
    let r = protocol::parse(b"MR Pri")?;
    let Response::CurrentChannel { selector } = r else {
        return Err(format!("expected CurrentChannel, got {r:?}").into());
    };
    assert_eq!(selector, CurrentMemorySelector::Priority);
    Ok(())
}

#[test]
fn memory_channel_address_accepts_all_firmware_input_boundaries() -> TestResult {
    let valid = [
        ("000", RegularChannel::new(0)?.into()),
        ("999", RegularChannel::new(999)?.into()),
        ("L00", MemoryChannelAddress::program_lower(0)?),
        ("L49", MemoryChannelAddress::program_lower(49)?),
        ("U00", MemoryChannelAddress::program_upper(0)?),
        ("U49", MemoryChannelAddress::program_upper(49)?),
        ("T01", MemoryChannelAddress::regional_t(1)?),
        ("T30", MemoryChannelAddress::regional_t(30)?),
        ("A01", MemoryChannelAddress::regional_a(1)?),
        ("A10", MemoryChannelAddress::regional_a(10)?),
    ];

    for (wire, expected) in valid {
        let address = MemoryChannelAddress::try_from(wire)?;
        assert_eq!(address, expected);
        assert_eq!(address.to_string(), wire);
    }
    Ok(())
}

#[test]
fn memory_channel_address_rejects_noncanonical_and_out_of_range_values() {
    for wire in [
        "", "00", "0000", "L50", "U50", "T00", "T31", "A00", "A11", "Pri", "PRI", "pri", "-01",
    ] {
        assert!(
            MemoryChannelAddress::try_from(wire).is_err(),
            "invalid address was accepted: {wire:?}"
        );
    }
}

#[test]
fn current_memory_selector_adds_only_the_output_only_priority_label() -> TestResult {
    assert_eq!(
        CurrentMemorySelector::try_from("021")?,
        CurrentMemorySelector::Address(RegularChannel::new(21)?.into()),
    );
    assert_eq!(
        CurrentMemorySelector::try_from("Pri")?,
        CurrentMemorySelector::Priority,
    );
    assert_eq!(CurrentMemorySelector::Priority.address(), None);
    for malformed in ["PRI", "pri", "P00", "L50"] {
        assert!(CurrentMemorySelector::try_from(malformed).is_err());
    }
    Ok(())
}

#[test]
fn generic_cat_parser_rejects_programming_mode_frames() {
    let result = protocol::parse(b"0M");
    assert!(
        matches!(result, Err(ProtocolError::UnknownCommand(ref command)) if command == "0M"),
        "0M must remain private to the MCP state machine, got {result:?}"
    );
}
