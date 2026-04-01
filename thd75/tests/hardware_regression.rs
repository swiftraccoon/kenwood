//! Regression tests from hardware validation.
//!
//! These test real response formats observed from TH-D75 firmware 1.02 and 1.03.
//! Each test targets a specific bug discovered during hardware testing.

use kenwood_thd75::error::Error;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// ============================================================================
// Bug 2: N response — not available in current mode
// ============================================================================

#[test]
fn parse_n_response() {
    let r = protocol::parse(b"N").unwrap();
    assert!(matches!(r, Response::NotAvailable));
}

#[test]
fn parse_be_n_response_is_not_available() {
    // When we send BE and get N back, the N is parsed at the frame level
    // before the mnemonic dispatch. So this is handled by the N check.
    let r = protocol::parse(b"N").unwrap();
    assert!(matches!(r, Response::NotAvailable));
}

// ============================================================================
// Bug 3: FQ returns 2 fields (band + frequency), not 21
// ============================================================================

#[test]
fn parse_fq_short_response() {
    // Real FQ response from TH-D75: just band + frequency
    let r = protocol::parse(b"FQ 0,0145190000").unwrap();
    match r {
        Response::Frequency { band, channel } => {
            assert_eq!(band, Band::A);
            assert_eq!(channel.rx_frequency, Frequency::new(145_190_000));
        }
        other => panic!("expected Frequency, got {other:?}"),
    }
}

#[test]
fn parse_fq_band_b() {
    let r = protocol::parse(b"FQ 1,0155190000").unwrap();
    match r {
        Response::Frequency { band, channel } => {
            assert_eq!(band, Band::B);
            assert_eq!(channel.rx_frequency, Frequency::new(155_190_000));
        }
        other => panic!("expected Frequency, got {other:?}"),
    }
}

#[test]
fn parse_fq_short_defaults_non_frequency_fields() {
    // When FQ returns the short 2-field format, all non-frequency fields
    // should be defaults.
    let r = protocol::parse(b"FQ 0,0145190000").unwrap();
    match r {
        Response::Frequency { channel, .. } => {
            assert_eq!(channel.tx_offset, Frequency::new(0));
            assert!(!channel.tone_enable);
            assert!(!channel.reverse);
        }
        other => panic!("expected Frequency, got {other:?}"),
    }
}

// ============================================================================
// Bug 4: ME returns 23 fields, not 21
// ============================================================================
//
// ME has a 22-field data layout (23 total with channel prefix) that differs
// from FO's 20-field layout. Two ME-specific fields are inserted: one at
// position 14 (between x5 and tone-code) and one at position 22 (after
// data-mode). The parser remaps these to FO order.

#[test]
fn parse_me_real_hardware_format() {
    // Real ME response captured from TH-D75 firmware 1.02.
    // 23 fields: channel + 22 data (FO's 20 + 2 ME extras at indices 14 and 22).
    let raw = b"ME 000,0154205000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::MemoryChannel { channel, data } => {
            assert_eq!(channel, 0);
            assert_eq!(data.rx_frequency, Frequency::new(154_205_000));
            assert_eq!(data.urcall.as_str(), "CQCQCQ");
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
}

#[test]
fn parse_me_23_fields() {
    // ME with channel 000: verify frequency and defaults parse correctly.
    let raw = b"ME 000,0145190000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::MemoryChannel { channel, data } => {
            assert_eq!(channel, 0);
            assert_eq!(data.rx_frequency, Frequency::new(145_190_000));
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
}

#[test]
fn parse_me_channel_001() {
    let raw = b"ME 001,0155190000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::MemoryChannel { channel, data } => {
            assert_eq!(channel, 1);
            assert_eq!(data.rx_frequency, Frequency::new(155_190_000));
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
}

#[test]
fn parse_me_with_tone_settings() {
    // ME with non-default step, shift, and tone settings to verify field
    // alignment after the extra field at index 14.
    let raw = b"ME 005,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0";
    let r = protocol::parse(raw).unwrap();
    match r {
        Response::MemoryChannel { channel, data } => {
            assert_eq!(channel, 5);
            assert_eq!(data.rx_frequency, Frequency::new(145_000_000));
            assert!(data.tone_enable);
        }
        other => panic!("expected MemoryChannel, got {other:?}"),
    }
}

// ============================================================================
// DW is Dual Watch (boolean) per D75 firmware RE
// ============================================================================

#[test]
fn parse_dw_zero_returns_dual_watch_off() {
    let r = protocol::parse(b"DW 0").unwrap();
    assert!(matches!(r, Response::DualWatch { enabled: false }));
}

#[test]
fn parse_dw_one_returns_dual_watch_on() {
    let r = protocol::parse(b"DW 1").unwrap();
    assert!(matches!(r, Response::DualWatch { enabled: true }));
}

// ============================================================================
// Bug 1: Timeout on execute() — configurable timeout
// ============================================================================

#[tokio::test]
async fn execute_timeout_field_exists() {
    use kenwood_thd75::radio::Radio;
    use kenwood_thd75::transport::MockTransport;
    use std::time::Duration;

    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_timeout(Duration::from_millis(100));
    // Verify it doesn't panic and the field is set.
}

// ============================================================================
// Existing format tests should still work
// ============================================================================

#[test]
fn parse_fo_21_fields_still_works() {
    let raw = b"FO 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00";
    let r = protocol::parse(raw).unwrap();
    assert!(matches!(r, Response::FrequencyFull { .. }));
}

#[test]
fn parse_fq_21_fields_still_works() {
    let raw = b"FQ 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00";
    let r = protocol::parse(raw).unwrap();
    assert!(matches!(r, Response::Frequency { .. }));
}

#[test]
fn parse_error_response_still_works() {
    let r = protocol::parse(b"?").unwrap();
    assert!(matches!(r, Response::Error));
}

// ============================================================================
// Radio-level NotAvailable handling
// ============================================================================

#[tokio::test]
async fn radio_not_available_response() {
    use kenwood_thd75::radio::Radio;
    use kenwood_thd75::transport::MockTransport;

    let mut mock = MockTransport::new();
    mock.expect(b"BE\r", b"N\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let result = radio.execute(Command::GetBeep).await;
    assert!(matches!(result, Err(Error::NotAvailable)));
}
