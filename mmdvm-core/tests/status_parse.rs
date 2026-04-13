//! Tests for `ModemStatus::parse_v1` / `parse_v2`.

use mmdvm_core::{MmdvmError, ModemMode, ModemStatus};

// Dev-dep acknowledgement — not used in this file, but the test
// crate sees every dev-dep.
use proptest as _;
use thiserror as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn parse_v1_all_flags_set() -> TestResult {
    // proto=1, mode=DStar(1), state=0x7F (every flag set), dstar=0.
    let payload = [1, 1, 0x7F, 0];
    let s = ModemStatus::parse_v1(&payload)?;
    assert_eq!(s.mode, ModemMode::DStar);
    assert!(s.tx());
    assert!(s.adc_overflow());
    assert!(s.rx_overflow());
    assert!(s.tx_overflow());
    assert!(s.lockout());
    assert!(s.dac_overflow());
    assert!(s.cd());
    Ok(())
}

#[test]
fn parse_v1_buffer_counts() -> TestResult {
    // proto=1, mode=0, state=0, then dstar=10, dmr1=8, dmr2=9, ysf=7,
    // p25=6, nxdn=5, pocsag=4.
    let payload = [1, 0, 0, 10, 8, 9, 7, 6, 5, 4];
    let s = ModemStatus::parse_v1(&payload)?;
    assert_eq!(s.dstar_space, 10);
    assert_eq!(s.dmr_space1, 8);
    assert_eq!(s.dmr_space2, 9);
    assert_eq!(s.ysf_space, 7);
    assert_eq!(s.p25_space, 6);
    assert_eq!(s.nxdn_space, 5);
    assert_eq!(s.pocsag_space, 4);
    assert_eq!(s.fm_space, 0, "v1 never reports FM space");
    Ok(())
}

#[test]
fn parse_v2_buffer_counts() -> TestResult {
    // v2 layout: mode, state, reserved, dstar, dmr1, dmr2, ysf, p25,
    // nxdn, reserved, fm, pocsag.
    let payload = [1, 0, 0, 12, 11, 10, 9, 8, 7, 0, 6, 5];
    let s = ModemStatus::parse_v2(&payload)?;
    assert_eq!(s.dstar_space, 12);
    assert_eq!(s.dmr_space1, 11);
    assert_eq!(s.dmr_space2, 10);
    assert_eq!(s.ysf_space, 9);
    assert_eq!(s.p25_space, 8);
    assert_eq!(s.nxdn_space, 7);
    assert_eq!(s.fm_space, 6);
    assert_eq!(s.pocsag_space, 5);
    Ok(())
}

#[test]
fn parse_v1_too_short_errors() {
    let err = ModemStatus::parse_v1(&[1, 1, 0]);
    assert!(
        matches!(err, Err(MmdvmError::InvalidStatusLength { len: 3, min: 4 })),
        "got {err:?}"
    );
}

#[test]
fn parse_v2_too_short_errors() {
    let err = ModemStatus::parse_v2(&[0u8; 8]);
    assert!(
        matches!(err, Err(MmdvmError::InvalidStatusLength { len: 8, min: 9 })),
        "got {err:?}"
    );
}

#[test]
fn parse_v1_partial_buffer_fields_default_to_zero() -> TestResult {
    // Only the mandatory 4 bytes — everything else must default to 0.
    let payload = [1, 1, 0, 7];
    let s = ModemStatus::parse_v1(&payload)?;
    assert_eq!(s.dstar_space, 7);
    assert_eq!(s.dmr_space1, 0);
    assert_eq!(s.dmr_space2, 0);
    assert_eq!(s.ysf_space, 0);
    assert_eq!(s.p25_space, 0);
    assert_eq!(s.nxdn_space, 0);
    assert_eq!(s.pocsag_space, 0);
    Ok(())
}

#[test]
fn parse_v2_includes_fm_space() -> TestResult {
    // Explicitly test the new v2 FM-space field, which didn't exist in v1.
    let mut payload = [0u8; 11];
    payload[0] = 1; // mode=DStar (just for diversity)
    payload[10] = 0x42;
    let s = ModemStatus::parse_v2(&payload)?;
    assert_eq!(s.fm_space, 0x42);
    Ok(())
}

#[test]
fn parse_v2_tx_lockout_cd_flags() -> TestResult {
    // state byte bits: 0x01 TX, 0x10 lockout, 0x40 CD.
    let state = 0x01 | 0x10 | 0x40;
    let payload = [1, state, 0, 0, 0, 0, 0, 0, 0];
    let s = ModemStatus::parse_v2(&payload)?;
    assert!(s.tx());
    assert!(s.lockout());
    assert!(s.cd());
    assert!(!s.adc_overflow());
    assert!(!s.rx_overflow());
    assert!(!s.tx_overflow());
    assert!(!s.dac_overflow());
    Ok(())
}
