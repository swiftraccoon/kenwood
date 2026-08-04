//! Integration tests for the 8 VFO protocol commands:
//! AG, SQ, SM, MD, FS, FT, SH, UP, RA.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint. (`::aprs` is spelled that way because the
// `types::*` glob shadows the bare crate name.)
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
// AG -- AF Gain (bare read, bare write, 3-digit zero-padded per KI4LAX)
// ============================================================================

#[test]
fn serialize_ag_read() {
    assert_eq!(protocol::serialize(&Command::GetAfGain), b"AG\r");
}

#[test]
fn serialize_ag_write() -> TestResult {
    // AG write is bare (no band), 3-digit zero-padded per KI4LAX.
    assert_eq!(
        protocol::serialize(&Command::SetAfGain {
            level: AfGainLevel::new(15)?
        }),
        b"AG 015\r"
    );
    Ok(())
}

#[test]
fn serialize_ag_write_upper_bound() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetAfGain {
            level: AfGainLevel::new(200)?
        }),
        b"AG 200\r"
    );
    Ok(())
}

#[test]
fn parse_ag_response() -> TestResult {
    let r = protocol::parse(b"AG 091")?;
    let Response::AfGain { level } = r else {
        return Err(format!("expected AfGain, got {r:?}").into());
    };
    assert_eq!(level, AfGainLevel::new(91)?);
    Ok(())
}

#[test]
fn parse_ag_low() -> TestResult {
    let r = protocol::parse(b"AG 005")?;
    let Response::AfGain { level } = r else {
        return Err(format!("expected AfGain, got {r:?}").into());
    };
    assert_eq!(level, AfGainLevel::new(5)?);
    Ok(())
}

#[test]
fn parse_ag_rejects_out_of_range_value() {
    assert!(protocol::parse(b"AG 201").is_err());
}

// ============================================================================
// SQ -- Squelch (one- or two-digit response; unpadded writes; range 0-6)
// ============================================================================

#[test]
fn serialize_sq_read() {
    assert_eq!(
        protocol::serialize(&Command::GetSquelch { band: Band::A }),
        b"SQ 0\r"
    );
}

#[test]
fn serialize_sq_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetSquelch {
            band: Band::A,
            level: SquelchLevel::new(3)?
        }),
        b"SQ 0,3\r"
    );
    Ok(())
}

#[test]
fn serialize_sq_write_band_b() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetSquelch {
            band: Band::B,
            level: SquelchLevel::new(5)?
        }),
        b"SQ 1,5\r"
    );
    Ok(())
}

#[test]
fn parse_sq_response() -> TestResult {
    let r = protocol::parse(b"SQ 0,03")?;
    let Response::Squelch { band, level } = r else {
        return Err(format!("expected Squelch, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(level, SquelchLevel::new(3)?);
    Ok(())
}

#[test]
fn parse_sq_no_padding() -> TestResult {
    let r = protocol::parse(b"SQ 0,3")?;
    let Response::Squelch { band, level } = r else {
        return Err(format!("expected Squelch, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(level, SquelchLevel::new(3)?);
    Ok(())
}

#[test]
fn parse_sq_out_of_range_rejected() {
    // Squelch 9 exceeds valid range 0-6, so strict validation rejects it
    assert!(protocol::parse(b"SQ 1,09").is_err());
}

#[test]
fn parse_sq_rejects_noncanonical_spelling() {
    for malformed in [b"SQ 0,003".as_slice(), b"SQ 0,+3", b"SQ 0, 3", b"SQ 0,3 "] {
        assert!(
            protocol::parse(malformed).is_err(),
            "accepted malformed SQ response {malformed:?}"
        );
    }
}

// ============================================================================
// SM -- S-meter (read-only, zero-padded to 4 digits)
// ============================================================================

#[test]
fn serialize_sm_read() {
    assert_eq!(
        protocol::serialize(&Command::GetSmeter { band: Band::A }),
        b"SM 0\r"
    );
}

#[test]
fn parse_sm_response() -> TestResult {
    let r = protocol::parse(b"SM 0,0005")?;
    let Response::Smeter { band, level } = r else {
        return Err(format!("expected Smeter, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(level, SMeterReading::new(5)?);
    Ok(())
}

#[test]
fn parse_sm_zero() -> TestResult {
    let r = protocol::parse(b"SM 1,0000")?;
    let Response::Smeter { band, level } = r else {
        return Err(format!("expected Smeter, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(level, SMeterReading::new(0)?);
    Ok(())
}

#[test]
fn parse_sm_out_of_range_rejected() {
    // S-meter 20 exceeds valid range 0-5, so strict validation rejects it
    assert!(protocol::parse(b"SM 0,0020").is_err());
}

// ============================================================================
// MD -- Operating mode
// ============================================================================

#[test]
fn serialize_md_read() {
    assert_eq!(
        protocol::serialize(&Command::GetOperatingMode { band: Band::A }),
        b"MD 0\r"
    );
}

#[test]
fn serialize_md_write() {
    assert_eq!(
        protocol::serialize(&Command::SetOperatingMode {
            band: Band::A,
            mode: OperatingMode::Dv
        }),
        b"MD 0,1\r"
    );
}

#[test]
fn parse_md_fm() -> TestResult {
    let r = protocol::parse(b"MD 0,0")?;
    let Response::OperatingMode { band, mode } = r else {
        return Err(format!("expected OperatingMode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, OperatingMode::Fm);
    Ok(())
}

#[test]
fn parse_md_lsb() -> TestResult {
    // MD mode 3 = LSB on D75 (not AM; AM is mode 2)
    let r = protocol::parse(b"MD 1,3")?;
    let Response::OperatingMode { band, mode } = r else {
        return Err(format!("expected OperatingMode, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(mode, OperatingMode::Lsb);
    Ok(())
}

// ============================================================================
// FS -- Fine step (bare read, no band parameter)
// ============================================================================

#[test]
fn serialize_fs_bare_read() {
    assert_eq!(protocol::serialize(&Command::GetFineStep), b"FS\r");
}

#[test]
fn parse_fs_response() -> TestResult {
    let r = protocol::parse(b"FS 0")?;
    let Response::FineStep { step } = r else {
        return Err(format!("expected FineStep, got {r:?}").into());
    };
    assert_eq!(step, FineStep::Hz20);
    Ok(())
}

#[test]
fn parse_fs_value_3() -> TestResult {
    let r = protocol::parse(b"FS 3")?;
    let Response::FineStep { step } = r else {
        return Err(format!("expected FineStep, got {r:?}").into());
    };
    assert_eq!(step, FineStep::Hz1000);
    Ok(())
}

// ============================================================================
// FT -- Fine Tune (bare read, Boolean write)
// ============================================================================

#[test]
fn serialize_ft_read() {
    assert_eq!(protocol::serialize(&Command::GetFineTune), b"FT\r");
    assert_eq!(
        protocol::serialize(&Command::SetFineTune { enabled: true }),
        b"FT 1\r"
    );
}

#[test]
fn parse_ft_response_bare() -> TestResult {
    let r = protocol::parse(b"FT 1")?;
    let Response::FineTune { enabled } = r else {
        return Err(format!("expected FineTune, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_ft_rejects_non_boolean_and_band_prefix() {
    assert!(protocol::parse(b"FT 2").is_err());
    assert!(protocol::parse(b"FT 0,1").is_err());
}

// ============================================================================
// SH -- Filter width (by mode index, not band)
// ============================================================================

#[test]
fn serialize_sh_read_ssb() {
    assert_eq!(
        protocol::serialize(&Command::GetFilterWidth {
            mode: FilterMode::Ssb
        }),
        b"SH 0\r"
    );
}

#[test]
fn serialize_sh_read_cw() {
    assert_eq!(
        protocol::serialize(&Command::GetFilterWidth {
            mode: FilterMode::Cw
        }),
        b"SH 1\r"
    );
}

#[test]
fn parse_sh_response() -> TestResult {
    let r = protocol::parse(b"SH 1,3")?;
    let Response::FilterWidth { width } = r else {
        return Err(format!("expected FilterWidth, got {r:?}").into());
    };
    assert_eq!(width.mode(), FilterMode::Cw);
    assert_eq!(width, FilterWidthIndex::new(FilterMode::Cw, 3)?);
    Ok(())
}

#[test]
fn parse_sh_rejects_unqualified_extra_and_cross_domain_values() {
    assert!(
        protocol::parse(b"SH 3").is_err(),
        "a width without its mode must not be invented as SSB"
    );
    assert!(
        protocol::parse(b"SH 0,3,1").is_err(),
        "SH has exactly two response fields"
    );
    assert!(
        protocol::parse(b"SH 2,4").is_err(),
        "AM has no filter-width index 4"
    );
}

#[test]
fn serialize_sh_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetFilterWidth {
            width: FilterWidthIndex::new(FilterMode::Cw, 4)?
        }),
        b"SH 1,4\r"
    );
    Ok(())
}

#[test]
fn serialize_sh_write_ssb() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetFilterWidth {
            width: FilterWidthIndex::new(FilterMode::Ssb, 3)?
        }),
        b"SH 0,3\r"
    );
    Ok(())
}

// ============================================================================
// UP -- Frequency up (action, no response data)
// ============================================================================

#[test]
fn serialize_up() {
    assert_eq!(protocol::serialize(&Command::FrequencyUp), b"UP\r");
}

// ============================================================================
// RA -- Attenuator
// ============================================================================

#[test]
fn serialize_ra_read() {
    assert_eq!(
        protocol::serialize(&Command::GetAttenuator { band: Band::A }),
        b"RA 0\r"
    );
}

#[test]
fn serialize_ra_write_on() {
    assert_eq!(
        protocol::serialize(&Command::SetAttenuator {
            band: Band::B,
            enabled: true
        }),
        b"RA 1,1\r"
    );
}

#[test]
fn serialize_ra_write_off() {
    assert_eq!(
        protocol::serialize(&Command::SetAttenuator {
            band: Band::A,
            enabled: false
        }),
        b"RA 0,0\r"
    );
}

#[test]
fn parse_ra_enabled() -> TestResult {
    let r = protocol::parse(b"RA 0,1")?;
    let Response::Attenuator { band, enabled } = r else {
        return Err(format!("expected Attenuator, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_ra_disabled() -> TestResult {
    let r = protocol::parse(b"RA 1,0")?;
    let Response::Attenuator { band, enabled } = r else {
        return Err(format!("expected Attenuator, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert!(!enabled);
    Ok(())
}

#[test]
fn parse_ra_rejects_non_boolean() {
    assert!(protocol::parse(b"RA 0,2").is_err());
}
