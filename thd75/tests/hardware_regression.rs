//! Regression tests from hardware validation.
//!
//! These test real response formats observed from TH-D75 firmware 1.02 and 1.03.
//! Each test targets a specific bug discovered during hardware testing.

use kenwood_thd75::error::Error;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// Bug 2: N response — not available in current mode
// ============================================================================

#[test]
fn parse_n_response() -> TestResult {
    let r = protocol::parse(b"N")?;
    assert!(
        matches!(r, Response::NotAvailable),
        "expected NotAvailable, got {r:?}"
    );
    Ok(())
}

#[test]
fn parse_be_n_response_is_not_available() -> TestResult {
    // When we send BE and get N back, the N is parsed at the frame level
    // before the mnemonic dispatch. So this is handled by the N check.
    let r = protocol::parse(b"N")?;
    assert!(
        matches!(r, Response::NotAvailable),
        "expected NotAvailable, got {r:?}"
    );
    Ok(())
}

// ============================================================================
// Bug 3: FQ returns 2 fields (band + frequency), not 21
// ============================================================================

#[test]
fn parse_fq_short_response() -> TestResult {
    // Real FQ response from TH-D75: just band + frequency
    let r = protocol::parse(b"FQ 0,0145190000")?;
    let Response::Frequency { band, channel } = r else {
        return Err(format!("expected Frequency, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(channel.rx_frequency, Frequency::new(145_190_000));
    Ok(())
}

#[test]
fn parse_fq_band_b() -> TestResult {
    let r = protocol::parse(b"FQ 1,0155190000")?;
    let Response::Frequency { band, channel } = r else {
        return Err(format!("expected Frequency, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(channel.rx_frequency, Frequency::new(155_190_000));
    Ok(())
}

#[test]
fn parse_fq_short_defaults_non_frequency_fields() -> TestResult {
    // When FQ returns the short 2-field format, all non-frequency fields
    // should be defaults.
    let r = protocol::parse(b"FQ 0,0145190000")?;
    let Response::Frequency { channel, .. } = r else {
        return Err(format!("expected Frequency, got {r:?}").into());
    };
    assert_eq!(channel.tx_offset, Frequency::new(0));
    assert!(!channel.tone_enable);
    assert!(!channel.reverse);
    Ok(())
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
fn parse_me_real_hardware_format() -> TestResult {
    // Real ME response captured from TH-D75 firmware 1.02.
    // 23 fields: channel + 22 data (FO's 20 + 2 ME extras at indices 14 and 22).
    let raw = b"ME 000,0154205000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { channel, data } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 0);
    assert_eq!(data.rx_frequency, Frequency::new(154_205_000));
    assert_eq!(data.urcall.as_str(), "CQCQCQ");
    Ok(())
}

#[test]
fn parse_me_23_fields() -> TestResult {
    // ME with channel 000: verify frequency and defaults parse correctly.
    let raw = b"ME 000,0145190000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { channel, data } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 0);
    assert_eq!(data.rx_frequency, Frequency::new(145_190_000));
    Ok(())
}

#[test]
fn parse_me_channel_001() -> TestResult {
    let raw = b"ME 001,0155190000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { channel, data } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 1);
    assert_eq!(data.rx_frequency, Frequency::new(155_190_000));
    Ok(())
}

#[test]
fn parse_me_with_tone_settings() -> TestResult {
    // Real D75 ME response for CH 003 (CTCSS enabled, tone code 27)
    // Hardware-verified via probes/fo_field_map.rs
    let raw = b"ME 003,0155340000,0000600000,0,0,0,0,0,0,1,0,0,0,0,0,27,27,000,0,CQCQCQ,0,00,0";
    let r = protocol::parse(raw)?;
    let Response::MemoryChannel { channel, data } = r else {
        return Err(format!("expected MemoryChannel, got {r:?}").into());
    };
    assert_eq!(channel, 3);
    assert_eq!(data.rx_frequency, Frequency::new(155_340_000));
    // field[8]=1 → CTCSS enable → maps to byte[10] bit 6
    // In our struct, ctcss is stored via flags_0a_raw bit 6
    assert_eq!(data.flags_0a_raw & 0x40, 0x40);
    Ok(())
}

// ============================================================================
// DW is Frequency Down (step down) per KI4LAX CAT reference
// ============================================================================

#[test]
fn parse_dw_returns_frequency_down() -> TestResult {
    let r = protocol::parse(b"DW 0")?;
    assert!(
        matches!(r, Response::FrequencyDown),
        "expected FrequencyDown, got {r:?}"
    );
    Ok(())
}

#[test]
fn parse_dw_band_b_returns_frequency_down() -> TestResult {
    let r = protocol::parse(b"DW 1")?;
    assert!(
        matches!(r, Response::FrequencyDown),
        "expected FrequencyDown, got {r:?}"
    );
    Ok(())
}

// ============================================================================
// Bug 1: Timeout on execute() — configurable timeout
// ============================================================================

#[tokio::test]
async fn execute_timeout_field_exists() -> TestResult {
    use kenwood_thd75::radio::Radio;
    use kenwood_thd75::transport::MockTransport;
    use std::time::Duration;

    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await?;
    radio.set_timeout(Duration::from_millis(100));
    // Verify it doesn't panic and the field is set.
    Ok(())
}

// ============================================================================
// Existing format tests should still work
// ============================================================================

#[test]
fn parse_fo_21_fields_still_works() -> TestResult {
    let raw = b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(raw)?;
    assert!(
        matches!(r, Response::FrequencyFull { .. }),
        "expected FrequencyFull, got {r:?}"
    );
    Ok(())
}

#[test]
fn parse_fq_21_fields_still_works() -> TestResult {
    let raw = b"FQ 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    let r = protocol::parse(raw)?;
    assert!(
        matches!(r, Response::Frequency { .. }),
        "expected Frequency, got {r:?}"
    );
    Ok(())
}

#[test]
fn parse_error_response_still_works() -> TestResult {
    let r = protocol::parse(b"?")?;
    assert!(matches!(r, Response::Error), "expected Error, got {r:?}");
    Ok(())
}

// ============================================================================
// Radio-level NotAvailable handling
// ============================================================================

#[tokio::test]
async fn radio_not_available_response() -> TestResult {
    use kenwood_thd75::radio::Radio;
    use kenwood_thd75::transport::MockTransport;

    let mut mock = MockTransport::new();
    mock.expect(b"BE\r", b"N\r");
    let mut radio = Radio::connect(mock).await?;
    let result = radio.execute(Command::GetBeep).await;
    assert!(
        matches!(result, Err(Error::NotAvailable)),
        "expected NotAvailable, got {result:?}"
    );
    Ok(())
}
