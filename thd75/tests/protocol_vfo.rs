//! Integration tests for the 8 VFO protocol commands:
//! AG, SQ, SM, MD, FS, FT, SH, UP, RA.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

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
            level: 15
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
            level: 39
        }),
        b"AG 039\r"
    );
}

#[test]
fn parse_ag_response() {
    match protocol::parse(b"AG 091").unwrap() {
        Response::AfGain { level } => {
            assert_eq!(level, 91);
        }
        other => panic!("expected AfGain, got {other:?}"),
    }
}

#[test]
fn parse_ag_low() {
    match protocol::parse(b"AG 005").unwrap() {
        Response::AfGain { level } => {
            assert_eq!(level, 5);
        }
        other => panic!("expected AfGain, got {other:?}"),
    }
}

// ============================================================================
// SQ -- Squelch (no zero-padding; D75 range 0-5)
// ============================================================================

#[test]
fn serialize_sq_read() {
    assert_eq!(
        protocol::serialize(&Command::GetSquelch { band: Band::A }),
        b"SQ 0\r"
    );
}

#[test]
fn serialize_sq_write() {
    assert_eq!(
        protocol::serialize(&Command::SetSquelch {
            band: Band::A,
            level: 3
        }),
        b"SQ 0,3\r"
    );
}

#[test]
fn serialize_sq_write_band_b() {
    assert_eq!(
        protocol::serialize(&Command::SetSquelch {
            band: Band::B,
            level: 5
        }),
        b"SQ 1,5\r"
    );
}

#[test]
fn parse_sq_response() {
    match protocol::parse(b"SQ 0,03").unwrap() {
        Response::Squelch { band, level } => {
            assert_eq!(band, Band::A);
            assert_eq!(level, 3);
        }
        other => panic!("expected Squelch, got {other:?}"),
    }
}

#[test]
fn parse_sq_no_padding() {
    match protocol::parse(b"SQ 0,3").unwrap() {
        Response::Squelch { band, level } => {
            assert_eq!(band, Band::A);
            assert_eq!(level, 3);
        }
        other => panic!("expected Squelch, got {other:?}"),
    }
}

#[test]
fn parse_sq_band_b() {
    match protocol::parse(b"SQ 1,09").unwrap() {
        Response::Squelch { band, level } => {
            assert_eq!(band, Band::B);
            assert_eq!(level, 9);
        }
        other => panic!("expected Squelch, got {other:?}"),
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
fn parse_sm_response() {
    match protocol::parse(b"SM 0,0005").unwrap() {
        Response::Smeter { band, level } => {
            assert_eq!(band, Band::A);
            assert_eq!(level, 5);
        }
        other => panic!("expected Smeter, got {other:?}"),
    }
}

#[test]
fn parse_sm_zero() {
    match protocol::parse(b"SM 1,0000").unwrap() {
        Response::Smeter { band, level } => {
            assert_eq!(band, Band::B);
            assert_eq!(level, 0);
        }
        other => panic!("expected Smeter, got {other:?}"),
    }
}

#[test]
fn parse_sm_max() {
    match protocol::parse(b"SM 0,0020").unwrap() {
        Response::Smeter { band, level } => {
            assert_eq!(band, Band::A);
            assert_eq!(level, 20);
        }
        other => panic!("expected Smeter, got {other:?}"),
    }
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
fn parse_md_fm() {
    match protocol::parse(b"MD 0,0").unwrap() {
        Response::Mode { band, mode } => {
            assert_eq!(band, Band::A);
            assert_eq!(mode, Mode::Fm);
        }
        other => panic!("expected Mode, got {other:?}"),
    }
}

#[test]
fn parse_md_lsb() {
    // MD mode 3 = LSB on D75 (not AM — AM is mode 2)
    match protocol::parse(b"MD 1,3").unwrap() {
        Response::Mode { band, mode } => {
            assert_eq!(band, Band::B);
            assert_eq!(mode, Mode::Lsb);
        }
        other => panic!("expected Mode, got {other:?}"),
    }
}

// ============================================================================
// FS -- Frequency step (band-indexed)
// ============================================================================

#[test]
fn serialize_fs_read_band_a() {
    assert_eq!(
        protocol::serialize(&Command::GetFrequencyStep { band: Band::A }),
        b"FS 0\r"
    );
}

#[test]
fn serialize_fs_read_band_b() {
    assert_eq!(
        protocol::serialize(&Command::GetFrequencyStep { band: Band::B }),
        b"FS 1\r"
    );
}

#[test]
fn parse_fs_response() {
    match protocol::parse(b"FS 0,5").unwrap() {
        Response::FrequencyStep { band, step } => {
            assert_eq!(band, Band::A);
            assert_eq!(step, StepSize::Hz12500);
        }
        other => panic!("expected FrequencyStep, got {other:?}"),
    }
}

#[test]
fn parse_fs_band_b() {
    match protocol::parse(b"FS 1,0").unwrap() {
        Response::FrequencyStep { band, step } => {
            assert_eq!(band, Band::B);
            assert_eq!(step, StepSize::Hz5000);
        }
        other => panic!("expected FrequencyStep, got {other:?}"),
    }
}

// ============================================================================
// FT -- Function type (bare read, no band parameter)
// ============================================================================

#[test]
fn serialize_ft_read() {
    assert_eq!(protocol::serialize(&Command::GetFunctionType), b"FT\r");
}

#[test]
fn parse_ft_response_bare() {
    match protocol::parse(b"FT 2").unwrap() {
        Response::FunctionType { value } => {
            assert_eq!(value, 2);
        }
        other => panic!("expected FunctionType, got {other:?}"),
    }
}

#[test]
fn parse_ft_response_with_band_prefix() {
    // Backward compatibility: handle "band,data" format in response
    match protocol::parse(b"FT 0,2").unwrap() {
        Response::FunctionType { value } => {
            assert_eq!(value, 2);
        }
        other => panic!("expected FunctionType, got {other:?}"),
    }
}

// ============================================================================
// SH -- Filter width (by mode index, not band)
// ============================================================================

#[test]
fn serialize_sh_read_ssb() {
    assert_eq!(
        protocol::serialize(&Command::GetFilterWidth { mode_index: 0 }),
        b"SH 0\r"
    );
}

#[test]
fn serialize_sh_read_cw() {
    assert_eq!(
        protocol::serialize(&Command::GetFilterWidth { mode_index: 1 }),
        b"SH 1\r"
    );
}

#[test]
fn parse_sh_response() {
    match protocol::parse(b"SH 1,3").unwrap() {
        Response::FilterWidth { mode_index, width } => {
            assert_eq!(mode_index, 1);
            assert_eq!(width, 3);
        }
        other => panic!("expected FilterWidth, got {other:?}"),
    }
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
fn parse_ra_enabled() {
    match protocol::parse(b"RA 0,1").unwrap() {
        Response::Attenuator { band, enabled } => {
            assert_eq!(band, Band::A);
            assert!(enabled);
        }
        other => panic!("expected Attenuator, got {other:?}"),
    }
}

#[test]
fn parse_ra_disabled() {
    match protocol::parse(b"RA 1,0").unwrap() {
        Response::Attenuator { band, enabled } => {
            assert_eq!(band, Band::B);
            assert!(!enabled);
        }
        other => panic!("expected Attenuator, got {other:?}"),
    }
}
