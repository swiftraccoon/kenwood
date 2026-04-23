//! Integration tests for the 8 VFO protocol commands:
//! AG, SQ, SM, MD, FS, FT, SH, UP, RA.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// AG -- AF Gain (bare read, bare write — 3-digit zero-padded per KI4LAX)
// ============================================================================

#[test]
fn serialize_ag_read() {
    assert_eq!(protocol::serialize(&Command::GetAfGain), b"AG\r");
}

#[test]
fn serialize_ag_write() {
    // AG write is bare (no band), 3-digit zero-padded per KI4LAX.
    assert_eq!(
        protocol::serialize(&Command::SetAfGain {
            band: Band::A,
            level: AfGainLevel::new(15)
        }),
        b"AG 015\r"
    );
}

#[test]
fn serialize_ag_write_band_b() {
    // Band is ignored — AG is global. 3-digit zero-padded.
    assert_eq!(
        protocol::serialize(&Command::SetAfGain {
            band: Band::B,
            level: AfGainLevel::new(39)
        }),
        b"AG 039\r"
    );
}

#[test]
fn parse_ag_response() -> TestResult {
    let r = protocol::parse(b"AG 091")?;
    let Response::AfGain { level } = r else {
        return Err(format!("expected AfGain, got {r:?}").into());
    };
    assert_eq!(level, AfGainLevel::new(91));
    Ok(())
}

#[test]
fn parse_ag_low() -> TestResult {
    let r = protocol::parse(b"AG 005")?;
    let Response::AfGain { level } = r else {
        return Err(format!("expected AfGain, got {r:?}").into());
    };
    assert_eq!(level, AfGainLevel::new(5));
    Ok(())
}

// ============================================================================
// SQ -- Squelch (no zero-padding; D75 range 0-6 per KI4LAX)
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
    // Squelch 9 exceeds valid range 0-6 — strict validation rejects it
    assert!(protocol::parse(b"SQ 1,09").is_err());
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
    // S-meter 20 exceeds valid range 0-5 — strict validation rejects it
    assert!(protocol::parse(b"SM 0,0020").is_err());
}

// ============================================================================
// MD -- Mode
// ============================================================================

#[test]
fn serialize_md_read() {
    assert_eq!(
        protocol::serialize(&Command::GetMode { band: Band::A }),
        b"MD 0\r"
    );
}

#[test]
fn serialize_md_write() {
    assert_eq!(
        protocol::serialize(&Command::SetMode {
            band: Band::A,
            mode: Mode::Dv
        }),
        b"MD 0,1\r"
    );
}

#[test]
fn parse_md_fm() -> TestResult {
    let r = protocol::parse(b"MD 0,0")?;
    let Response::Mode { band, mode } = r else {
        return Err(format!("expected Mode, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(mode, Mode::Fm);
    Ok(())
}

#[test]
fn parse_md_lsb() -> TestResult {
    // MD mode 3 = LSB on D75 (not AM — AM is mode 2)
    let r = protocol::parse(b"MD 1,3")?;
    let Response::Mode { band, mode } = r else {
        return Err(format!("expected Mode, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(mode, Mode::Lsb);
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
// FT -- Function type (bare read, no band parameter)
// ============================================================================

#[test]
fn serialize_ft_read() {
    assert_eq!(protocol::serialize(&Command::GetFunctionType), b"FT\r");
}

#[test]
fn parse_ft_response_bare() -> TestResult {
    let r = protocol::parse(b"FT 2")?;
    let Response::FunctionType { enabled } = r else {
        return Err(format!("expected FunctionType, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_ft_response_with_band_prefix() -> TestResult {
    // Backward compatibility: handle "band,data" format in response
    let r = protocol::parse(b"FT 0,2")?;
    let Response::FunctionType { enabled } = r else {
        return Err(format!("expected FunctionType, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
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
    let Response::FilterWidth { mode, width } = r else {
        return Err(format!("expected FilterWidth, got {r:?}").into());
    };
    assert_eq!(mode, FilterMode::Cw);
    assert_eq!(width, FilterWidthIndex::new(3, FilterMode::Cw)?);
    Ok(())
}

#[test]
fn serialize_sh_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetFilterWidth {
            mode: FilterMode::Cw,
            width: FilterWidthIndex::new(4, FilterMode::Cw)?
        }),
        b"SH 1,4\r"
    );
    Ok(())
}

#[test]
fn serialize_sh_write_ssb() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetFilterWidth {
            mode: FilterMode::Ssb,
            width: FilterWidthIndex::new(3, FilterMode::Ssb)?
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
    assert_eq!(
        protocol::serialize(&Command::FrequencyUp { band: Band::A }),
        b"UP 0\r"
    );
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
